use super::*;

impl TranscodeManager {
    /// Number of active HLS sessions across the public VOD registry and the
    /// test-only historical live registry (for /metrics and activity).
    pub async fn active_sessions(&self) -> usize {
        let rolling = self.sessions.lock().await.len();
        rolling + self.vod.active_sessions().await
    }

    /// Publish a provisional successor under the incarnation's stable bearer
    /// capability after the replicated owner CAS succeeds.
    #[cfg(test)]
    pub(crate) async fn adopt_session_id(
        &self,
        provisional_id: &str,
        durable_session_id: &str,
    ) -> bool {
        let Some(adoption) = self.session_adoption_token(durable_session_id) else {
            return false;
        };
        self.adopt_session_id_with_token(provisional_id, durable_session_id, adoption)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn adopt_session_id_with_token(
        &self,
        provisional_id: &str,
        durable_session_id: &str,
        adoption: SessionAdoptionToken,
    ) -> bool {
        self.adopt_session_id_with_owner(provisional_id, durable_session_id, adoption, ())
            .await
            .is_ok()
    }

    /// Move a provisional worker and its cleanup owner through the same
    /// cancellation boundary. Once the map contains the durable capability,
    /// `owner` is updated synchronously before any further await or fallible
    /// bookkeeping. A panic or cancellation after that point therefore tears
    /// down the durable id; one before it still tears down the provisional id.
    pub(crate) async fn adopt_session_id_with_owner<O: SessionAdoptionOwner>(
        &self,
        provisional_id: &str,
        durable_session_id: &str,
        adoption: SessionAdoptionToken,
        mut owner: O,
    ) -> Result<O, O> {
        if provisional_id == durable_session_id {
            return if self.sessions.lock().await.contains_key(durable_session_id) {
                owner.adopted_session_id(durable_session_id);
                Ok(owner)
            } else {
                Err(owner)
            };
        }
        let release_gate = &adoption.gate;
        if release_gate.released.load(Acquire) {
            return Err(owner);
        }
        let _release_transition = Arc::clone(&release_gate.transition).lock_owned().await;
        if release_gate.released.load(Acquire) {
            return Err(owner);
        }
        let mut sessions = self.sessions.lock().await;
        if release_gate.released.load(Acquire) {
            return Err(owner);
        }
        if sessions.contains_key(durable_session_id) {
            return Err(owner);
        }
        let Some(session) = sessions.remove(provisional_id) else {
            return Err(owner);
        };
        sessions.insert(durable_session_id.to_owned(), session);
        owner.adopted_session_id(durable_session_id);
        drop(sessions);

        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in requests.values_mut() {
            if matches!(&entry.state, RequestState::Ready(id) if id == provisional_id) {
                entry.state = RequestState::Ready(durable_session_id.to_owned());
            }
        }
        Ok(owner)
    }

    /// The fMP4 init object this session actually publishes.
    ///
    /// Anything that opens the init by name — the exact-codec probe, the
    /// Apple High-tier rewrite — must ask, because a fenced successor names
    /// its init after its ownership epoch. `None` for an unknown session.
    pub(crate) async fn session_init_object(&self, session_id: &str) -> Option<String> {
        if self.vod.session_file_id(session_id).await.is_some() {
            return Some("init.mp4".to_owned());
        }
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id)?;
        Some(init_object_name(
            session
                .takeover
                .as_ref()
                .map(|takeover| takeover.owner_epoch),
        ))
    }

    /// Snapshot only the monotone coordinates needed by the replicated owner
    /// heartbeat. Session identities are cloned under the map lock; the
    /// segment locks are then sampled without holding that global lock.
    pub(crate) async fn session_frontiers(
        &self,
        session_ids: &[String],
    ) -> HashMap<String, SessionFrontier> {
        let selected = {
            let sessions = self.sessions.lock().await;
            session_ids
                .iter()
                .filter_map(|session_id| {
                    sessions
                        .get(session_id)
                        .cloned()
                        .map(|session| (session_id.clone(), session))
                })
                .collect::<Vec<_>>()
        };
        let mut frontiers = HashMap::with_capacity(selected.len());
        // A VOD session's frontier is film-addressed: the end of the last
        // segment it was served. The renewal batch drops any route without a
        // frontier entry and fences it as "cluster lease lost", so a missing
        // arm here is a kill, not a default.
        for session_id in session_ids {
            if selected.iter().any(|(id, _)| id == session_id) {
                continue;
            }
            if let Some(ms) = self.vod.frontier_ms(session_id).await {
                frontiers.insert(
                    session_id.clone(),
                    SessionFrontier {
                        produced_playable_through_ms: ms,
                        fetched_through_ms: ms,
                        media_sequence: 0,
                    },
                );
            }
        }
        for (session_id, session) in selected {
            let (local_produced_through_ms, media_sequence) = {
                let segments = session.segments.lock().await;
                (
                    segments.produced_playable_end_ms().unwrap_or(0).max(0),
                    segments.next_media_sequence(),
                )
            };
            let local_fetched_through_ms = session
                .fetched_end_ms
                .load(Relaxed)
                .clamp(0, local_produced_through_ms);
            let offset = session.frontier_offset_ms();
            let produced_playable_through_ms = offset.saturating_add(local_produced_through_ms);
            let fetched_through_ms = offset
                .saturating_add(local_fetched_through_ms)
                .min(produced_playable_through_ms);
            frontiers.insert(
                session_id,
                SessionFrontier {
                    produced_playable_through_ms,
                    fetched_through_ms,
                    media_sequence,
                },
            );
        }
        frontiers
    }

    /// Every live session, paired with how it is really delivering.
    ///
    /// The pair is what the activity array needs and `SessionInfo` cannot
    /// give it: a copy-remux and a transcode are the same struct, and the one
    /// field that ever hinted at the difference — `encoder` — is a label, not
    /// a kind. It reads "cached" on a cache hit and is *rewritten* under the
    /// hardware→software fallback, so a page inferring the method from it
    /// would relabel a stream mid-play. The method is fixed when the session
    /// is created and never moves.
    pub async fn list_deliveries(&self) -> Vec<(SessionInfo, crate::delivery::Method)> {
        self.list_deliveries_bounded(usize::MAX).await
    }

    /// Bounded VOD identities for the cluster-only activity snapshot. These
    /// are already complete delivery facts, unlike historical live-session
    /// candidates, and never consult the removed presentation engine.
    pub(crate) async fn vod_delivery_infos_bounded(
        &self,
        limit: usize,
    ) -> Vec<crate::vodserve::VodDeliveryInfo> {
        self.vod.delivery_infos_bounded(limit).await
    }

    /// A diagnostics-safe prefix that bounds the expensive per-session
    /// telemetry reads before they begin.
    pub async fn list_deliveries_bounded(
        &self,
        limit: usize,
    ) -> Vec<(SessionInfo, crate::delivery::Method)> {
        let candidates = self.delivery_candidates_bounded(limit).await;
        let ids = candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        let mut details = self
            .delivery_details_bounded(&ids, limit)
            .await
            .into_iter()
            .map(|detail| (detail.0.id.clone(), detail))
            .collect::<HashMap<_, _>>();
        let mut deliveries = candidates
            .into_iter()
            .filter_map(|candidate| details.remove(&candidate.id))
            .collect::<Vec<_>>();
        deliveries.extend(self.vod.delivery_infos().await.into_iter().map(|info| {
            let method = info.method;
            (vod_delivery_session_info(info), method)
        }));
        deliveries.sort_by(|left, right| {
            right
                .0
                .started_unix
                .cmp(&left.0.started_unix)
                .then(left.0.id.cmp(&right.0.id))
        });
        deliveries.truncate(limit);
        deliveries
    }

    /// Top-K session identities without awaiting any per-session telemetry.
    pub async fn delivery_candidates_bounded(&self, limit: usize) -> Vec<DeliveryCandidate> {
        let sessions = self.sessions.lock().await;
        let ids = crate::delivery::newest_ids_bounded(
            sessions
                .iter()
                .map(|(id, session)| (id.as_str(), session.started_unix)),
            limit,
        );
        let mut candidates = ids
            .into_iter()
            .filter_map(|id| {
                sessions.get(&id).map(|session| DeliveryCandidate {
                    id,
                    started_unix: session.started_unix,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .started_unix
                .cmp(&left.started_unix)
                .then(left.id.cmp(&right.id))
        });
        candidates
    }

    /// Enrich only explicitly selected sessions. Clone the Arcs under the map
    /// lock, then release it before awaiting any session-owned lock.
    pub async fn delivery_details_bounded(
        &self,
        ids: &[String],
        limit: usize,
    ) -> Vec<(SessionInfo, crate::delivery::Method)> {
        let selected = {
            let sessions = self.sessions.lock().await;
            ids.iter()
                .take(limit)
                .filter_map(|id| {
                    sessions
                        .get(id)
                        .cloned()
                        .map(|session| (id.clone(), session))
                })
                .collect::<Vec<_>>()
        };
        let limits = self.ahead_limits().await;
        let (global_live_bytes, global_ahead_bytes) = self.global_flow_bytes().await;
        let mut out = Vec::with_capacity(selected.len());
        for (id, s) in selected {
            out.push((
                session_info(&id, &s, limits, global_live_bytes, global_ahead_bytes).await,
                s.method,
            ));
        }
        out
    }

    /// One session's live telemetry, for the player's stats overlay. Does not
    /// touch the last-request clock: asking how a stream is doing is not the same as
    /// fetching from it, and a status poll must not keep an abandoned session
    /// alive past the idle reaper.
    pub async fn session_status(&self, session_id: &str) -> Option<SessionInfo> {
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        let limits = self.ahead_limits().await;
        let (global_live_bytes, global_ahead_bytes) = self.global_flow_bytes().await;
        Some(
            session_info(
                session_id,
                &session,
                limits,
                global_live_bytes,
                global_ahead_bytes,
            )
            .await,
        )
    }

    /// Correlate a shipped marker-prewarm placeholder with one authoritative
    /// VOD playback. Direct play (reported as either legacy `direct` or
    /// telemetry `direct_play`) and rolling sessions deliberately return
    /// false so their client-emitted miss remains the metric truth.
    pub(crate) async fn consume_vod_marker_prewarm_placeholder(
        &self,
        user_id: i64,
        file_id: i64,
        method: &str,
    ) -> bool {
        if !matches!(method, "remux" | "transcode") {
            return false;
        }
        let user_scope = serde_json::json!(["user_id", user_id]).to_string();
        let live_rolling =
            self.sessions.lock().await.values().any(|session| {
                session.file_id == file_id && session.supersession_user == user_scope
            });
        let recently_rolling =
            recent_rolling_marker_ambiguity(&self.recent_marker_ambiguities, &user_scope, file_id);
        let rolling_ambiguous = live_rolling || recently_rolling;
        if rolling_ambiguous {
            // The shipped marker payload has no playback identity. Keep its
            // miss instead of assigning it to a same-file VOD session when a
            // rolling presentation could have sent it.
            return false;
        }
        self.vod
            .consume_marker_prewarm_placeholder(user_id, file_id, method)
            .await
    }

    /// Status for either HLS presentation. A VOD lookup goes first because a
    /// session id belongs to exactly one registry and its diagnostics have no
    /// honest live-transcode equivalent.
    #[cfg(test)]
    pub async fn hls_session_status(&self, session_id: &str) -> Option<HlsSessionInfo> {
        self.hls_session_status_publication(session_id)
            .await
            .and_then(|publication| publication.result.ok())
    }

    /// Status bound to the exact VOD incarnation or rolling producer attempt
    /// that was sampled. The HTTP layer must bodylessly authorize this owner
    /// immediately before returning telemetry.
    /// The destination the named session's client last settled on.
    ///
    /// `None` covers three cases a caller must treat identically: no such
    /// session, an actor that has retired, and a client that has not exchanged
    /// yet. In all three there is no ordering to compare against, so the
    /// correct answer is to do the work rather than skip it.
    pub(crate) async fn settled_target_for_session(
        &self,
        session_id: &str,
    ) -> Option<crate::playback_control::SettledTarget> {
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        session.control.snapshot().await?.settled_target
    }

    pub(crate) async fn hls_session_status_publication(
        &self,
        session_id: &str,
    ) -> Option<VodResponsePublication<HlsSessionInfo>> {
        if let Some(publication) = self.vod.status_publication(session_id).await {
            return Some(VodResponsePublication {
                result: publication
                    .result
                    .map(|status| HlsSessionInfo::Vod(Box::new(status))),
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            });
        }
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        let producer_attempt = session.control.current_producer_attempt();
        let limits = self.ahead_limits().await;
        let (global_live_bytes, global_ahead_bytes) = self.global_flow_bytes().await;
        let status = session_info(
            session_id,
            &session,
            limits,
            global_live_bytes,
            global_ahead_bytes,
        )
        .await;
        Some(VodResponsePublication {
            result: Ok(HlsSessionInfo::Live(Box::new(status))),
            owner: MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                session,
                producer_attempt,
            }),
        })
    }

    fn rolling_terminal_operation(
        &self,
        control: &crate::playback_control::LocalControlRequest<'_>,
    ) -> Result<Option<RollingTerminalOperation>, crate::playback_control::ControlStateError> {
        let mut terminal = self
            .terminal_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(operation) = terminal.get(control.session_id).cloned() else {
            return Ok(None);
        };
        if operation.expired(crate::media_sessions::unix_ms()) {
            terminal.remove(control.session_id);
            return Ok(None);
        }
        if operation.identity.matches(control) {
            Ok(Some(operation))
        } else {
            Err(crate::playback_control::ControlStateError::SessionEnded)
        }
    }

    /// Fenced behavior-neutral control for either HLS presentation. A newly
    /// accepted sequence renews the selected engine's established activity
    /// clock with the explicit `control` reason; replay and rejection do not.
    #[cfg(test)]
    pub(crate) async fn hls_session_control(
        self: &Arc<Self>,
        control: crate::playback_control::LocalControlRequest<'_>,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        self.hls_session_control_with_terminal(control, i64::MAX, None, None)
            .await
    }

    pub(crate) async fn hls_session_control_with_terminal(
        self: &Arc<Self>,
        control: crate::playback_control::LocalControlRequest<'_>,
        deadline_unix_ms: i64,
        terminal_committer: Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
        preparation_admission: Option<
            Arc<dyn crate::playback_control::PreparationSettlementAdmission>,
        >,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        match self.rolling_terminal_operation(&control) {
            Ok(Some(operation)) => {
                let mut result = match operation.result.wait().await {
                    Ok(result) => result,
                    Err(error) => return Some(Err(error)),
                };
                result.disposition = crate::playback_control::ControlDisposition::Replay;
                // A stored result carries the observation of the exchange that
                // produced it. Replaying it must not replay that: one viewer
                // action is one measurement, and a client retrying a lost
                // terminal response would otherwise pay the shadow's reads and
                // count its change again on every retry.
                result.selection = crate::playback_control::SelectionObservation::default();
                if let Some(commit) = &result.terminal_commit {
                    commit.retry();
                }
                return Some(Ok(result));
            }
            Err(error) => return Some(Err(error)),
            Ok(None) => {}
        }
        let retired = self
            .retired_presentations
            .lock()
            .await
            .get(control.session_id)
            .cloned();
        if let Some(retired) = retired {
            if retired
                .session
                .control
                .snapshot()
                .await
                .is_some_and(|lease| {
                    lease.terminal
                        == Some(crate::playback_control::RollingTerminalCause::PauseExpired)
                })
            {
                return Some(Err(
                    crate::playback_control::ControlStateError::PauseExpired,
                ));
            }
        }
        if let Some(result) = self
            .vod
            .control_with_terminal(
                control.clone(),
                deadline_unix_ms,
                terminal_committer.clone(),
                preparation_admission.clone(),
            )
            .await
        {
            return Some(result);
        }
        let session = self
            .sessions
            .lock()
            .await
            .get(control.session_id)
            .cloned()?;
        let transition = session.child_transition.lock().await;
        if !self
            .sessions
            .lock()
            .await
            .get(control.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            return None;
        }
        if let Err(error) = crate::playback_control::verify_authority(
            self.store.as_ref(),
            control.session_id,
            control.generation,
            control.owner_node_id,
            control.owner_epoch,
        )
        .await
        {
            return Some(Err(error));
        }
        let session_id = control.session_id.to_owned();
        // Start the detached consumer before sending the actor command. If
        // the request future is cancelled after mailbox admission, the actor
        // can still publish a ticket that this session-owned worker applies.
        self.ensure_flow_worker(&session_id, Arc::clone(&session));
        #[cfg(test)]
        let control_pause = session
            .control_applied_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let terminal_admission =
            (control.snapshot.demand == crate::playback_control::PlaybackDemand::End).then(|| {
                Arc::new(RollingTerminalAdmission {
                    manager: Arc::clone(self),
                    session_id: session_id.clone(),
                    session: Arc::clone(&session),
                    identity: RollingTerminalIdentity::from_request(&control),
                    terminal_committer: terminal_committer.clone(),
                    #[cfg(test)]
                    control_pause: control_pause.clone(),
                }) as Arc<dyn crate::playback_control::RollingTerminalAdmission>
            });
        let (
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive,
            platform,
            selection,
            lease_expires_at_unix_ms,
            lease_timeout_ms,
            flow_ticket,
            lease_state,
            acknowledged_end,
        ) = match session
            .accept_control(
                control,
                deadline_unix_ms,
                terminal_admission,
                preparation_admission,
            )
            .await?
        {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        drop(transition);
        if acknowledged_end {
            let terminal = session
                .terminal_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            return Some(match terminal {
                Some(terminal) => match terminal.result.wait().await {
                    Ok(mut result) => {
                        result.disposition = disposition;
                        if disposition == crate::playback_control::ControlDisposition::Replay {
                            // See the fast path above: a replayed result must
                            // not replay its observation.
                            result.selection =
                                crate::playback_control::SelectionObservation::default();
                            if let Some(commit) = &result.terminal_commit {
                                commit.retry();
                            }
                        }
                        Ok(result)
                    }
                    Err(error) => Err(error),
                },
                None => Err(crate::playback_control::ControlStateError::Unavailable),
            });
        }
        Some(
            Arc::clone(self)
                .finish_hls_session_control(
                    session_id,
                    session,
                    disposition,
                    accepted_sequence,
                    action,
                    action_suppressed,
                    preparation_directive,
                    platform,
                    selection,
                    lease_expires_at_unix_ms,
                    lease_timeout_ms,
                    flow_ticket,
                    lease_state,
                    false,
                    None,
                    None,
                    #[cfg(test)]
                    control_pause,
                )
                .await,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_hls_session_control(
        self: Arc<Self>,
        session_id: String,
        session: Arc<Session>,
        disposition: crate::playback_control::ControlDisposition,
        accepted_sequence: u64,
        action: crate::playback_control::ControlAction,
        action_suppressed: bool,
        preparation_directive: Option<crate::playback_control::PreparationDirective>,
        platform: crate::playback_control::ClientPlatform,
        selection: crate::playback_control::SelectionObservation,
        lease_expires_at_unix_ms: i64,
        lease_timeout_ms: u32,
        flow_ticket: u64,
        lease_state: &'static str,
        acknowledged_end: bool,
        terminal_handoff: Option<crate::playback_control::TerminalResponseHandoff>,
        terminal_committer: Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
        #[cfg(test)] control_pause: Option<Arc<tokio::sync::Barrier>>,
    ) -> Result<
        crate::playback_control::LocalControlResult,
        crate::playback_control::ControlStateError,
    > {
        let rescue = terminal_handoff.clone();
        let outcome = async {
            #[cfg(test)]
            if let Some(pause) = control_pause {
                pause.wait().await;
                pause.wait().await;
            }
            // The actor also tickets replayed sequences. A caller whose first
            // response was lost can therefore wait for physical convergence.
            session.control.wait_for_flow(flow_ticket).await;
            let limits = self.ahead_limits().await;
            let (global_live_bytes, global_ahead_bytes) = self.global_flow_bytes().await;
            let _final_transition = session.child_transition.lock().await;
            if session.control.is_retired() && !acknowledged_end {
                return Err(
                    if session.control.snapshot().await.is_some_and(|lease| {
                        lease.terminal
                            == Some(crate::playback_control::RollingTerminalCause::PauseExpired)
                    }) {
                        crate::playback_control::ControlStateError::PauseExpired
                    } else {
                        crate::playback_control::ControlStateError::SessionEnded
                    },
                );
            }
            if !self
                .sessions
                .lock()
                .await
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                return Err(crate::playback_control::ControlStateError::OwnerTransition);
            }
            let status = session_info(
                &session_id,
                &session,
                limits,
                global_live_bytes,
                global_ahead_bytes,
            )
            .await;
            if session.control.is_retired() && !acknowledged_end {
                return Err(
                    if session.control.snapshot().await.is_some_and(|lease| {
                        lease.terminal
                            == Some(crate::playback_control::RollingTerminalCause::PauseExpired)
                    }) {
                        crate::playback_control::ControlStateError::PauseExpired
                    } else {
                        crate::playback_control::ControlStateError::SessionEnded
                    },
                );
            }
            if !self
                .sessions
                .lock()
                .await
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                return Err(crate::playback_control::ControlStateError::OwnerTransition);
            }
            let mut result = crate::playback_control::LocalControlResult {
                disposition,
                accepted_sequence,
                action,
                action_suppressed,
                preparation_directive,
                lease_expires_at_unix_ms,
                lease_timeout_ms,
                lease_state,
                status: HlsSessionInfo::Live(Box::new(status)),
                platform,
                terminal_handoff,
                terminal_commit: None,
                selection,
            };
            if acknowledged_end {
                if let Some(committer) = terminal_committer {
                    result.terminal_commit = Some(committer.start(&result));
                } else if let Some(handoff) = &result.terminal_handoff {
                    handoff.complete();
                }
            }
            Ok(result)
        }
        .await;
        if outcome.is_err() {
            if let Some(handoff) = rescue {
                handoff.complete();
            }
        }
        outcome
    }

    pub(super) async fn emit_session_event(
        &self,
        session_id: &str,
        session: &Session,
        event: &str,
        fields: SessionEventFields<'_>,
    ) {
        emit_session_event_to_store(Arc::clone(&self.store), session_id, session, event, fields)
            .await;
    }

    /// Linearize manager retirement with producer replacement.
    ///
    /// Replacement holds `child_transition` until a successor is published.
    /// Teardown transfers the exact Arc to a detached owner, which takes that
    /// same gate before actor Terminal admission and retains all process,
    /// scratch, and admission ownership through confirmed reap. Whichever
    /// wins still fixes the exact child: a late replacement sees `retired`,
    /// while late retirement owns the installed successor until terminal
    /// proof.
    #[cfg(test)]
    pub(super) async fn retire_session(&self, session_id: &str, session: &Arc<Session>) -> bool {
        self.retire_session_until(session_id, session, None)
            .await
            .expect("unbounded session retirement cannot expire")
    }

    #[cfg(test)]
    pub(super) async fn retire_session_until(
        &self,
        session_id: &str,
        session: &Arc<Session>,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<bool, String> {
        self.retire_session_until_with_cause(session_id, session, deadline, "ended")
            .await
            .map(|outcome| outcome.removed)
    }

    pub(super) async fn retire_session_until_with_cause(
        &self,
        session_id: &str,
        session: &Arc<Session>,
        deadline: Option<tokio::time::Instant>,
        cause: &'static str,
    ) -> Result<RollingRetirementOutcome, String> {
        loop {
            // Transfer the exact Arc and registry handles before the first
            // await. A joined follower observes the same physical settlement
            // and winning cause; it never creates a second cleanup owner.
            let (_committed, ticket) = spawn_rolling_retirement_owner(
                Arc::clone(&self.sessions),
                Arc::clone(&self.retired_presentations),
                Arc::clone(&self.active_session_count),
                Arc::clone(&self.store),
                Arc::clone(&self.recent_marker_ambiguities),
                session_id.to_owned(),
                Arc::clone(session),
                deadline,
                cause,
            );
            let participation = ticket.participation;
            let settlement = Arc::clone(&ticket.settlement);
            let waited = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, ticket.wait())
                    .await
                    .map_err(|_| replacement_deadline_error())?,
                None => ticket.wait().await,
            };
            match waited {
                Ok(removed) => {
                    return Ok(RollingRetirementOutcome {
                        removed,
                        participation,
                        cause: settlement.cause(),
                    });
                }
                Err(_) if deadline.is_none() && !session.control.is_retired() => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Publish a newly spawned session only while the process-local serving
    /// fence is open. Admission reads the fence's synchronous loss generation,
    /// not the teardown loop's delayed mirror, and rechecks it around actor and
    /// registry waits plus the final move-only install transaction.
    pub(super) async fn register_session(
        &self,
        session_id: &str,
        session: Arc<Session>,
        producer_attempt: u64,
    ) -> Result<(), SessionRegistrationRejection> {
        let promised_namespace = self
            .retired_presentations
            .lock()
            .await
            .get(session_id)
            .is_some_and(|retired| Instant::now() < retired.serve_until);
        let adoption = match (!promised_namespace)
            .then(|| self.session_adoption_token(session_id))
            .flatten()
        {
            Some(adoption) => adoption,
            None => {
                let rejection = SessionRegistrationRejection::AdoptionCapacity;
                session.fail(PlaylistError::SessionFailed(
                    "rolling session registration rejected: adoption capacity".to_owned(),
                ));
                let (_commit, retirement) = spawn_rolling_retirement_owner(
                    Arc::clone(&self.sessions),
                    Arc::clone(&self.retired_presentations),
                    Arc::clone(&self.active_session_count),
                    Arc::clone(&self.store),
                    Arc::clone(&self.recent_marker_ambiguities),
                    session_id.to_owned(),
                    Arc::clone(&session),
                    None,
                    "capacity",
                );
                let _ = retirement.wait().await;
                return Err(rejection);
            }
        };
        let release_gate = &adoption.gate;
        let admitted_generation = self.serving_authority.admit();
        // Actor authorization may wait behind the bounded per-session
        // mailbox; never hold the global session registry while doing so.
        let authorization = match (release_gate.released.load(Acquire), admitted_generation) {
            (true, _) => Err(SessionRegistrationRejection::SessionRelease),
            (false, None) => Err(SessionRegistrationRejection::ServingFence),
            (false, Some(admitted_generation)) => {
                let authorization = session
                    .control
                    .authorize_producer_install(producer_attempt)
                    .await
                    .map_err(SessionRegistrationRejection::Producer);
                if self.publication_authority_is_current(admitted_generation) {
                    authorization
                } else {
                    Err(SessionRegistrationRejection::ServingFence)
                }
            }
        };
        let rejection = match authorization {
            Err(rejection) => Some(rejection),
            Ok(authorization) => {
                // Actor admission may be slow. Serialize only the final
                // registry/install transaction with release; the monotone bit
                // rejects an authorization that became stale while waiting.
                let _release_transition = Arc::clone(&release_gate.transition).lock_owned().await;
                let admitted_generation = admitted_generation
                    .expect("successful registration authorization has a serving generation");
                // Publication below is synchronous while both the registry
                // guard and the actor's exact move-only install fence are
                // held. The serving generation is checked again inside both.
                let mut sessions = self.sessions.lock().await;
                if release_gate.released.load(Acquire) {
                    Some(SessionRegistrationRejection::SessionRelease)
                } else if !self.publication_authority_is_current(admitted_generation) {
                    Some(SessionRegistrationRejection::ServingFence)
                } else {
                    match session
                        .control
                        .lock_authorized_producer_install(authorization)
                    {
                        Err(reason) => Some(SessionRegistrationRejection::Producer(reason)),
                        Ok(install) => {
                            if release_gate.released.load(Acquire) {
                                drop(install);
                                Some(SessionRegistrationRejection::SessionRelease)
                            } else if !self.publication_authority_is_current(admitted_generation) {
                                drop(install);
                                Some(SessionRegistrationRejection::ServingFence)
                            } else {
                                sessions.insert(session_id.to_owned(), Arc::clone(&session));
                                self.active_session_count.store(sessions.len(), Relaxed);
                                drop(install);
                                None
                            }
                        }
                    }
                }
            }
        };
        let Some(rejection) = rejection else {
            session.ensure_publication_worker(session_id);
            return Ok(());
        };
        // Rejection occurred before registry publication, so ordinary
        // retire_session cannot find this Arc. Tear it down explicitly and
        // promptly; Child::kill_on_drop is only the last-resort backstop.
        // Store the typed local failure before actor End: a concurrent
        // playlist reader must see a 502-class producer failure, not observe
        // only actor retirement and collapse it into an anonymous 404.
        let failure = format!("rolling session registration rejected: {rejection:?}");
        session.fail(PlaylistError::SessionFailed(failure));
        let cause = match rejection {
            SessionRegistrationRejection::ServingFence => {
                // Publish the typed fence synchronously before transferring
                // cancellation-independent process ownership.
                session.project_authority_fence();
                "authority_fence"
            }
            SessionRegistrationRejection::SessionRelease => {
                session.project_authority_fence();
                "client_released"
            }
            SessionRegistrationRejection::Producer(_) => "failed",
            SessionRegistrationRejection::AdoptionCapacity => "capacity",
        };
        let (_commit, retirement) = spawn_rolling_retirement_owner(
            Arc::clone(&self.sessions),
            Arc::clone(&self.retired_presentations),
            Arc::clone(&self.active_session_count),
            Arc::clone(&self.store),
            Arc::clone(&self.recent_marker_ambiguities),
            session_id.to_owned(),
            Arc::clone(&session),
            None,
            cause,
        );
        // Dropping this registration future cannot cancel the already-spawned
        // owner. The await only gives the synchronous caller a settled result.
        let _ = retirement.wait().await;
        Err(rejection)
    }

    /// End one session now. True if it existed.
    ///
    /// `reason` distinguishes the two callers in the log, because they mean
    /// opposite things: a client releasing a stream it has finished with is
    /// routine, and an admin killing one from the activity page is somebody
    /// intervening.
    pub async fn stop_session(&self, session_id: &str, reason: &'static str) -> bool {
        // A VOD session ends with a tombstone rather than a retirement: its
        // rendition may outlive it (admitted cache, other readers), but this
        // id answers 410 ever after. The cause keys off the same reason
        // strings the live path already uses.
        {
            let cause = if reason.contains("superseded") {
                crate::vodserve::Terminal::Superseded
            } else if reason.contains("revoked") || reason.contains("credential") {
                crate::vodserve::Terminal::Revoked
            } else if reason.contains("replaced") || reason.contains("rescan") {
                crate::vodserve::Terminal::Replaced
            } else if reason.contains("admin") || reason.contains("operator") {
                crate::vodserve::Terminal::AdminStop
            } else {
                crate::vodserve::Terminal::Deleted
            };
            if self.vod.end(session_id, cause).await {
                return true;
            }
        }
        let Some(session) = self.sessions.lock().await.get(session_id).cloned() else {
            return false;
        };
        let event_reason = if reason.contains("released") {
            "client_released"
        } else if reason.contains("admin") {
            "killed"
        } else {
            reason
        };
        let Ok(outcome) = self
            .retire_session_until_with_cause(session_id, &session, None, event_reason)
            .await
        else {
            return false;
        };
        tracing::info!(
            session = %session_log_id(session_id),
            requested_reason = reason,
            winning_reason = %outcome.cause,
            participation = ?outcome.participation,
            "transcode session retirement settled"
        );
        outcome.removed
    }

    pub(super) fn session_adoption_gate(
        &self,
        session_id: &str,
    ) -> Option<Arc<SessionReleaseGate>> {
        let mut entries = self
            .session_release_gates
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_session_release_gates(&mut entries);
        if let Some(gate) = entries.entries.get(session_id).and_then(|entry| {
            entry
                .active_release
                .clone()
                .or_else(|| entry.gate.upgrade())
        }) {
            return Some(gate);
        }
        let in_flight = entries
            .entries
            .values()
            .filter(|entry| {
                entry.active_release.is_none()
                    && entry.retain_until.is_none()
                    && entry.gate.strong_count() > 0
            })
            .count();
        if in_flight >= MAX_IN_FLIGHT_SESSION_ADOPTION_GATES {
            return None;
        }
        let gate = Arc::new(SessionReleaseGate {
            transition: Arc::new(tokio::sync::Mutex::new(())),
            released: AtomicBool::new(false),
        });
        entries.entries.insert(
            session_id.to_owned(),
            SessionReleaseGateEntry {
                gate: Arc::downgrade(&gate),
                active_release: None,
                retain_until: None,
            },
        );
        Some(gate)
    }

    /// Capture the release generation before an authoritative route/CAS
    /// decision. Holding this token prevents a completed release from being
    /// forgotten and re-created as a fresh, permissive gate by a delayed
    /// request.
    /// The gate a `PreparationExecutor` needs for this session (M6 §3.4),
    /// resolved through **both** engines.
    ///
    /// VOD first, and that order is the whole point. Every public create is
    /// `Presentation::Vod` and lives in `VodServe`'s registry; `self.sessions`
    /// holds only the incident-recovery sessions. Asking the rolling actor
    /// alone is the failure [`PreparationGate`]'s own doc predicts — a gate
    /// that exists only on the actor lets M6 stage successors on a path
    /// viewers do not take, and its acceptance passes while the feature fires
    /// on nothing. A first version of this did exactly that.
    ///
    /// `None` when neither engine has a live local session: an expired,
    /// remote-owned or already-retired playback stages nothing, because a row
    /// the gate never holds is reaped only by the maintenance backstop and
    /// until then costs the user an admission slot.
    /// The VOD engine, for the test that proves a public session resolves a
    /// preparation gate through it.
    #[cfg(test)]
    pub(crate) fn vod_for_test(&self) -> &Arc<crate::vodserve::VodServe> {
        &self.vod
    }

    pub(crate) async fn session_preparation_gate(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn crate::playback_control::PreparationGate>> {
        if let Some(gate) = self.vod.preparation_gate(session_id).await {
            return Some(gate);
        }
        let control = self.sessions.lock().await.get(session_id)?.control.clone();
        Some(Arc::new(control) as Arc<dyn crate::playback_control::PreparationGate>)
    }

    pub(crate) async fn promote_prepared_session(&self, session_id: &str) {
        let _ = self.vod.promote_prepared_session(session_id).await;
    }

    pub(crate) fn foreground_media_waiting(&self) -> bool {
        self.admissions.live_is_waiting()
    }

    /// Record that the durable row now names this ask.
    ///
    /// Both engines, because either may own the session and neither knows
    /// which. A session that has gone by the time this lands is not an error:
    /// the row is written and keyed by playback, so it outlives the session
    /// that recorded it and the only thing lost is one exchange's worth of
    /// suppression.
    pub(crate) async fn record_desired_persisted(&self, session_id: &str, digest: &str) {
        if self.vod.record_desired_persisted(session_id, digest).await {
            return;
        }
        let control = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| session.control.clone());
        if let Some(control) = control {
            control.record_desired_persisted(digest).await;
        }
    }

    pub(crate) fn session_adoption_token(&self, session_id: &str) -> Option<SessionAdoptionToken> {
        Some(SessionAdoptionToken {
            gate: self.session_adoption_gate(session_id)?,
            registry: Arc::clone(&self.session_release_gates),
            session_id: session_id.to_owned(),
        })
    }

    fn activate_session_release_gate(&self, session_id: &str) -> Arc<SessionReleaseGate> {
        let mut entries = self
            .session_release_gates
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_session_release_gates(&mut entries);
        let entry = entries
            .entries
            .entry(session_id.to_owned())
            .or_insert_with(|| {
                let gate = Arc::new(SessionReleaseGate {
                    transition: Arc::new(tokio::sync::Mutex::new(())),
                    released: AtomicBool::new(false),
                });
                SessionReleaseGateEntry {
                    gate: Arc::downgrade(&gate),
                    active_release: Some(gate),
                    retain_until: None,
                }
            });
        let gate = entry
            .active_release
            .clone()
            .or_else(|| entry.gate.upgrade())
            .unwrap_or_else(|| {
                Arc::new(SessionReleaseGate {
                    transition: Arc::new(tokio::sync::Mutex::new(())),
                    released: AtomicBool::new(false),
                })
            });
        entry.gate = Arc::downgrade(&gate);
        entry.active_release = Some(Arc::clone(&gate));
        entry.retain_until = None;
        gate
    }

    /// Lift only the strong active-release owner. Delayed adopters retain the
    /// same gate Arc and therefore still observe its monotone `released` bit;
    /// a later unrelated operation cannot turn their stale generation live.
    pub(crate) fn complete_session_release(&self, session_id: &str) {
        let mut entries = self
            .session_release_gates
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_session_release_gates(&mut entries);
        if let Some(entry) = entries.entries.get_mut(session_id) {
            let deadline = Instant::now() + SESSION_RELEASE_GATE_RETENTION;
            entry.retain_until = Some(deadline);
            entries
                .retained
                .push_back((session_id.to_owned(), deadline));
        }
        prune_session_release_gates(&mut entries);
    }

    /// Complete a terminal generation after the Store has definitively
    /// published terminal state or absence. Fresh operations are fenced by
    /// that authoritative result, while operations admitted before it retain
    /// the exact released Arc in their adoption token. Removing the registry
    /// owner here keeps arbitrary absent DELETE keys out of the retained
    /// lease-loss safety window.
    pub(crate) fn complete_session_release_durable(
        &self,
        proof: &crate::media_sessions::DurableReleaseProof,
    ) {
        let session_id = proof.session_id();
        let mut registry = self
            .session_release_gates
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_session_release_gates(&mut registry);
        let remove = registry.entries.get_mut(session_id).is_some_and(|entry| {
            entry.active_release = None;
            entry.retain_until = None;
            entry.gate.strong_count() == 0
        });
        if remove {
            registry.entries.remove(session_id);
        }
    }

    pub(super) fn session_release_is_active(&self, session_id: &str) -> bool {
        self.session_release_gates
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(session_id)
            .and_then(|entry| {
                entry
                    .active_release
                    .as_ref()
                    .map(Arc::clone)
                    .or_else(|| entry.gate.upgrade())
            })
            .is_some_and(|gate| gate.released.load(Acquire))
    }

    /// Phase one of a durable terminal decision: close response publication
    /// and late adoption without choosing a local tombstone cause. The Store
    /// CAS then persists the first winning cause, and `begin_session_terminal`
    /// projects that exact winner in phase two.
    pub(crate) async fn begin_session_publication_fence(self: &Arc<Self>, session_id: &str) {
        let gate = self.activate_session_release_gate(session_id);
        let _transition = Arc::clone(&gate.transition).lock_owned().await;
        // This is the publication linearization point. Once set, every media
        // authorization path rejects even if cancellation prevents the
        // optional actor mirror below from running.
        gate.released.store(true, Release);
        if let Some(session) = self.sessions.lock().await.get(session_id).cloned() {
            session.project_authority_fence();
        }
    }

    /// Fail closed one public capability before its durable End mutation.
    ///
    /// VOD has no rolling actor, so its exact tombstone must be installed
    /// synchronously here. Rolling publication is synchronously authority-
    /// fenced, while confirmed process reap continues under the universal
    /// detached retirement owner and cannot delay submission of the durable
    /// Store mutation.
    #[cfg(test)]
    pub(crate) async fn begin_session_release(self: &Arc<Self>, session_id: &str) {
        self.begin_session_terminal(
            session_id,
            crate::vodserve::Terminal::Deleted,
            "released by client",
        )
        .await;
    }

    /// Project a terminal capability generation on the actual worker before
    /// any durable settlement or exact cleanup. This is shared by client
    /// release, supersession, lease loss and operator stop so a preparing VOD
    /// attachment cannot appear after any terminal decision.
    pub(crate) async fn begin_session_terminal(
        self: &Arc<Self>,
        session_id: &str,
        terminal: crate::vodserve::Terminal,
        reason: &'static str,
    ) {
        let gate = self.activate_session_release_gate(session_id);
        let manager = Arc::clone(self);
        let session_id = session_id.to_owned();
        let (projected_tx, projected_rx) = tokio::sync::oneshot::channel();
        // Transfer the whole proof before the first cancellable wait. If the
        // caller disappears, this supervisor still retries a panicked attempt
        // until the release generation and concrete worker are fail-closed.
        tokio::spawn(async move {
            loop {
                let attempt_manager = Arc::clone(&manager);
                let attempt_gate = Arc::clone(&gate);
                let attempt_session_id = session_id.clone();
                let attempt = tokio::spawn(async move {
                    attempt_manager
                        .project_session_terminal(
                            attempt_gate,
                            attempt_session_id,
                            terminal,
                            reason,
                        )
                        .await;
                });
                match attempt.await {
                    Ok(()) => {
                        let _ = projected_tx.send(());
                        return;
                    }
                    Err(error) => {
                        tracing::error!(%error, "session terminal projection attempt failed; retrying");
                        tokio::task::yield_now().await;
                    }
                }
            }
        });
        projected_rx
            .await
            .expect("session terminal projection supervisor exited before proof");
    }

    async fn project_session_terminal(
        self: Arc<Self>,
        gate: Arc<SessionReleaseGate>,
        session_id: String,
        terminal: crate::vodserve::Terminal,
        reason: &'static str,
    ) {
        let _transition = Arc::clone(&gate.transition).lock_owned().await;
        // The transition makes this bit and every final attachment decision
        // one total order. Setting it before acquiring the transition would
        // allow an attachment already inside the critical section to pass an
        // earlier check and then commit after the bit changed.
        gate.released.store(true, Release);
        if self.vod.begin_end_detached(&session_id, terminal).await {
            return;
        }
        let rolling = self.sessions.lock().await.get(&session_id).cloned();
        if let Some(session) = &rolling {
            // This atomic projection is the response-publication boundary.
            // Actor settlement and physical reap must not delay durable End;
            // the detached retirement owner below retains both.
            session.project_authority_fence();
        }
        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            manager.stop_session(&session_id, reason).await;
        });
    }

    /// Snapshot the live capability ids without holding the session map while
    /// replicated lease I/O runs.
    pub async fn active_session_ids(&self) -> Vec<String> {
        self.sessions.lock().await.keys().cloned().collect()
    }

    /// Snapshot only workers still eligible for durable lease renewal. A
    /// fenced worker remains in the map until teardown acquires its child
    /// transition, but its lease authority must stop at the fencing verdict.
    pub async fn renewable_session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| !session.control.is_retired())
            .map(|(session_id, _)| session_id.clone())
            .collect();
        // VOD sessions hold durable routes too; invisible here, the lease
        // loop settles their routes as stale ~3 s after create and every
        // request answers 410. Tombstoned ones are deliberately absent —
        // dropping out of the live set is how their routes get ended.
        ids.extend(self.vod.live_or_preparing_session_ids().await);
        ids
    }

    /// Make a set of sessions immediately unservable without waiting for
    /// process teardown or recursive scratch cleanup. The detached cleanup
    /// path later takes `child_transition`, removes the registry entries, and
    /// reaps their resources; every serving path observes this monotonic bit.
    pub async fn fence_sessions(&self, session_ids: &[String]) {
        let fenced = {
            let sessions = self.sessions.lock().await;
            let fenced = session_ids
                .iter()
                .filter_map(|session_id| sessions.get(session_id).cloned())
                .collect::<Vec<_>>();
            // Publish every fail-closed projection before releasing the map.
            // An in-flight response must reacquire this map before visibility,
            // so it can never pass between the snapshot and a later actor
            // round trip for another Session.
            for session in &fenced {
                session.project_authority_fence();
            }
            fenced
        };
        futures_util::future::join_all(
            fenced
                .iter()
                .map(|session| session.settle_authority_fence()),
        )
        .await;
    }

    async fn stop_all_sessions_for_serving_fence(&self) {
        let sessions = {
            let registry = self.sessions.lock().await;
            let sessions = registry
                .iter()
                .map(|(id, session)| (id.clone(), Arc::clone(session)))
                .collect::<Vec<_>>();
            for (_, session) in &sessions {
                // Serving authority is lost now, not after any actor mailbox
                // or an earlier Session happens to unblock.
                session.project_authority_fence();
            }
            sessions
        };
        futures_util::future::join_all(sessions.into_iter().map(
            |(session_id, session)| async move {
                session.settle_authority_fence().await;
                if self
                    .retire_session_until_with_cause(&session_id, &session, None, "authority_fence")
                    .await
                    .is_ok_and(|outcome| outcome.removed)
                {
                    // The detached retirement winner owns the single event;
                    // this loop only records local teardown progress.
                    tracing::warn!(
                        session = %session_log_id(&session_id),
                        "transcode session self-fenced after quorum loss"
                    );
                }
            },
        ))
        .await;
    }

    /// Kill every mutable HLS producer on the first loss transition. The
    /// watch is process-local and changes synchronously with readiness, so
    /// teardown never waits for another Store request to time out.
    pub(crate) async fn serving_fence_loop(
        self: Arc<Self>,
        mut serving: tokio::sync::watch::Receiver<crate::serving_fence::ServingState>,
    ) {
        loop {
            let state = *serving.borrow_and_update();
            let previous_generation = self.serving_loss_generation.load(Acquire);
            if !state.ready || state.loss_generation != previous_generation {
                // Keep the gate closed until the old generation is fully
                // retired. A racing registration either observes false in its
                // final pre-insert check or publishes before this loop obtains
                // the registry lock, in which case the snapshot includes it.
                self.serving_ready.store(false, Release);
                self.serving_loss_generation
                    .store(state.loss_generation, Release);
                self.stop_all_sessions_for_serving_fence().await;
            }
            self.serving_ready.store(state.ready, Release);
            if serving.changed().await.is_err() {
                break;
            }
        }
    }

    /// Abort a remote start only when this process can prove it owns the
    /// worker behind that incarnation.
    ///
    /// The ordinary proof is the process-local idempotency record left by the
    /// start request. A fenced successor has no such record — nothing on this
    /// node requested it — so it carries the incarnation on the session
    /// itself. Without the second proof DELETE and the peer abort endpoint are
    /// permanent no-ops against every taken-over session: they return success
    /// while the replacement encoder keeps running and keeps its admission
    /// slot, and §7.2's "a delete racing takeover resolves to ended without a
    /// replacement child remaining alive" holds only by the slower lease-loss
    /// path.
    async fn rolling_session_for_request(
        &self,
        request_id: &str,
        session_id: &str,
    ) -> Option<Arc<Session>> {
        let request_matches = self.requests.lock().is_ok_and(|requests| {
            requests.get(request_id).is_some_and(
                |entry| matches!(&entry.state, RequestState::Ready(ready) if ready == session_id),
            )
        });
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        session
            .takeover
            .as_ref()
            .map_or(request_matches, |takeover| {
                takeover.incarnation_id == request_id
            })
            .then_some(session)
    }

    /// Resolve only the exact rolling owner generation from a lease snapshot.
    /// The stable public id and incarnation survive takeover, so both are
    /// insufficient by themselves: epoch 1 is the original request-owned
    /// worker and later epochs must match the embedded takeover identity.
    async fn rolling_session_for_owner(
        &self,
        incarnation_id: &str,
        session_id: &str,
        owner_epoch: i64,
    ) -> Option<Arc<Session>> {
        let request_matches = owner_epoch == 1
            && self.requests.lock().is_ok_and(|requests| {
                requests.get(incarnation_id).is_some_and(|entry| {
                    matches!(&entry.state, RequestState::Ready(ready) if ready == session_id)
                })
            });
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        session
            .takeover
            .as_ref()
            .map_or(request_matches, |takeover| {
                takeover.incarnation_id == incarnation_id && takeover.owner_epoch == owner_epoch
            })
            .then_some(session)
    }

    pub(crate) async fn fence_session_for_owner(
        &self,
        incarnation_id: &str,
        session_id: &str,
        owner_epoch: i64,
    ) -> bool {
        let Some(session) = self
            .rolling_session_for_owner(incarnation_id, session_id, owner_epoch)
            .await
        else {
            return false;
        };
        session.project_authority_fence();
        true
    }

    pub(crate) async fn owns_session_for_owner(
        &self,
        incarnation_id: &str,
        session_id: &str,
        owner_epoch: i64,
    ) -> bool {
        if self
            .rolling_session_for_owner(incarnation_id, session_id, owner_epoch)
            .await
            .is_some()
        {
            return true;
        }
        owner_epoch == 1
            && self.requests.lock().is_ok_and(|requests| {
                requests.get(incarnation_id).is_some_and(
                |entry| matches!(&entry.state, RequestState::Ready(ready) if ready == session_id),
            )
            })
            && self.vod.owns_or_preparing(session_id).await
    }

    pub(crate) async fn stop_session_for_owner(
        &self,
        incarnation_id: &str,
        session_id: &str,
        owner_epoch: i64,
        reason: &'static str,
    ) -> bool {
        let Some(session) = self
            .rolling_session_for_owner(incarnation_id, session_id, owner_epoch)
            .await
        else {
            return false;
        };
        session.project_authority_fence();
        self.retire_session_until_with_cause(session_id, &session, None, reason)
            .await
            .is_ok_and(|outcome| outcome.removed)
    }

    pub async fn stop_session_for_request(
        &self,
        request_id: &str,
        session_id: &str,
        reason: &'static str,
    ) -> bool {
        if let Some(session) = self
            .rolling_session_for_request(request_id, session_id)
            .await
        {
            session.project_authority_fence();
            return self
                .retire_session_until_with_cause(session_id, &session, None, reason)
                .await
                .is_ok_and(|outcome| outcome.removed);
        }
        // A VOD session is stoppable by its durable route without a match in
        // the live maps: its ids are unguessable and never recycled, and the
        // route store is the caller's authority.
        self.vod.owns(session_id).await && self.stop_session(session_id, reason).await
    }

    /// Abort an epoch-1 VOD worker only when the original start request still
    /// maps to that exact public id. Kept separate from rolling request
    /// matching so a delayed start-abort cannot fall through and kill a later
    /// takeover generation with the same incarnation.
    pub(crate) async fn stop_vod_session_for_request(
        &self,
        request_id: &str,
        session_id: &str,
        reason: &'static str,
    ) -> bool {
        let request_matches = self.requests.lock().is_ok_and(|requests| {
            requests.get(request_id).is_some_and(
                |entry| matches!(&entry.state, RequestState::Ready(ready) if ready == session_id),
            )
        });
        request_matches
            && self.vod.owns(session_id).await
            && self.stop_session(session_id, reason).await
    }

    /// `stop_session` with a ceiling on how long teardown may take.
    ///
    /// Retirement takes the child-transition gate, kills and confirms the
    /// process, releases admission, and removes the exact registry Arc. Those
    /// steps may be serialized behind a hardware-to-software replacement that
    /// is itself respawning ffmpeg. Unique scratch cleanup is a separate
    /// retrying owner and is deliberately outside this replacement boundary.
    ///
    /// `hold` is anything the caller must not release until teardown has
    /// really finished — in practice the cluster replacement guard. Dropping
    /// that guard while the child is still alive lets the next start for the
    /// same player through, and since a takeover deliberately supersedes
    /// nothing, the result is two encoders for one player.
    pub(crate) async fn stop_session_until<T: Send + 'static>(
        self: &Arc<Self>,
        session_id: &str,
        reason: &'static str,
        deadline: tokio::time::Instant,
        hold: T,
    ) -> bool {
        // Transfer the replacement guard and all later actor/resource work
        // before the first cancellable await. The deadline limits only this
        // caller's wait; the exact cleanup owner continues to confirmed reap.
        let manager = Arc::clone(self);
        let owned_id = session_id.to_owned();
        let teardown = tokio::spawn(async move {
            manager
                .fence_sessions(std::slice::from_ref(&owned_id))
                .await;
            let stopped = manager.stop_session(&owned_id, reason).await;
            drop(hold);
            stopped
        });
        match tokio::time::timeout_at(deadline, teardown).await {
            Ok(Ok(stopped)) => stopped,
            Ok(Err(_)) => false,
            Err(_) => {
                tracing::warn!(
                    session = %session_log_id(session_id),
                    reason,
                    "session teardown outlived its deadline; fenced, teardown continues detached"
                );
                false
            }
        }
    }

    /// Resolve a rolling capability without extending its lease.
    ///
    /// Object readers call this before publication/integrity/wait checks and
    /// renew only after they have a concrete response to serve. Keeping those
    /// operations separate prevents repeated misses from becoming synthetic
    /// playback activity.
    #[cfg(test)]
    pub(super) async fn live_session(&self, session_id: &str) -> Option<Arc<Session>> {
        let session = self.sessions.lock().await.get(session_id).cloned()?;
        (!session.control.is_retired()).then_some(session)
    }

    pub(super) fn publication_authority_is_current(&self, admitted_generation: u64) -> bool {
        self.serving_authority.is_current(admitted_generation)
    }

    pub(super) async fn retired_owner_is_current(
        &self,
        session_id: &str,
        session: &Arc<Session>,
        producer_attempt: u64,
    ) -> bool {
        self.retired_presentations
            .lock()
            .await
            .get(session_id)
            .is_some_and(|retired| {
                retired.producer_attempt == producer_attempt
                    && Arc::ptr_eq(&retired.session, session)
                    && Instant::now() < retired.serve_until
            })
    }

    pub(super) async fn retired_object_is_current(
        &self,
        session_id: &str,
        session: &Arc<Session>,
        producer_attempt: u64,
        object_name: Option<&str>,
    ) -> bool {
        if !self
            .retired_owner_is_current(session_id, session, producer_attempt)
            .await
        {
            return false;
        }
        let Some(index) = object_name.and_then(segment_index) else {
            return object_name.is_some_and(is_init_object);
        };
        session
            .segments
            .lock()
            .await
            .segs
            .iter()
            .find(|segment| segment.index == index)
            .is_some_and(|segment| segment.visibility.is_servable(Instant::now()))
    }
}
