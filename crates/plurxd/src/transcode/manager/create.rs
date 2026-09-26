use super::*;

impl TranscodeManager {
    /// Create a session, or hand back the one an identical request already
    /// created.
    ///
    /// The idempotency matters more than it looks. Session creation spawns a
    /// process and kills its predecessor, and it used to be a GET — which is
    /// idempotent *by definition*, so anything in the path that felt entitled
    /// to replay a GET could spawn a second encoder and orphan the first.
    /// Automatic quality switching would have multiplied how often that
    /// mattered. A repeated `request_id` now returns the same session;
    /// the same id asking for something different is a conflict rather than a
    /// quiet second stream.
    ///
    /// The id is *reserved* before any work starts, not checked and then
    /// acted on: the old shape read the map, released the lock, spawned
    /// ffmpeg, and recorded the result — so two concurrent retries with the
    /// same id could both pass the check, spawn two encoders, and leave one
    /// caller holding a session its twin's supersession had already killed.
    /// Now the second caller finds the reservation and waits for the first
    /// one's session instead.
    #[cfg(test)]
    pub async fn create_session(
        &self,
        req: &SessionRequest,
        user_name: &str,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["username", user_name]).to_string();
        // A legacy process-local start has no cluster identity: no user id, no
        // incarnation, and therefore no epoch. The ledger refuses an empty
        // epoch, so this is "no budget" rather than "an unused one".
        let recovery = SessionRecoveryIdentity {
            user_id: 0,
            incarnation_id: String::new(),
            recovery_epoch: String::new(),
        };
        self.create_session_inner(
            req,
            user_name,
            &supersession_user,
            &recovery,
            None,
            None,
            None,
            Priority::Live,
        )
        .await
        .map(|creation| creation.info)
    }

    /// Start a cluster-owned replacement while retaining its process-local
    /// serialization gate for the ingress activation verdict.
    pub async fn create_cluster_session(
        &self,
        req: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        user_name: &str,
        deadline: tokio::time::Instant,
        admitted_serving_generation: u64,
    ) -> Result<ClusterSessionStart, String> {
        self.create_cluster_session_with_priority(
            req,
            recovery,
            user_name,
            deadline,
            admitted_serving_generation,
            Priority::Live,
        )
        .await
    }

    /// Start a provisional make-before-break worker only from spare capacity.
    /// It never registers as a foreground waiter, so the incumbent and any
    /// other viewer keep admission priority while the durable preparation
    /// owner retains exact cancellation authority.
    pub(crate) async fn create_cluster_prepared_session(
        &self,
        req: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        user_name: &str,
        deadline: tokio::time::Instant,
        admitted_serving_generation: u64,
    ) -> Result<ClusterSessionStart, String> {
        self.create_cluster_session_with_priority(
            req,
            recovery,
            user_name,
            deadline,
            admitted_serving_generation,
            Priority::Speculative,
        )
        .await
    }

    async fn create_cluster_session_with_priority(
        &self,
        req: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        user_name: &str,
        deadline: tokio::time::Instant,
        admitted_serving_generation: u64,
        priority: Priority,
    ) -> Result<ClusterSessionStart, String> {
        let user_id = recovery.user_id;
        let serving_admission = ClusterServingAdmission {
            generation: admitted_serving_generation,
            deadline,
        };
        self.require_cluster_serving_authority(serving_admission)?;
        let supersession_user = serde_json::json!(["user_id", user_id]).to_string();
        let gate_key =
            serde_json::json!([supersession_user.as_str(), req.playback_id.as_str(),]).to_string();
        let replacement = self
            .acquire_cluster_replacement_gate(
                gate_key,
                req.previous_session_id.as_deref(),
                deadline,
            )
            .await?;
        if tokio::time::Instant::now() >= deadline {
            return Err(capacity_error(
                "the replacement start expired before it could finish provisional work",
            ));
        }
        self.require_cluster_serving_authority(serving_admission)?;
        let creation = self
            .create_session_inner(
                req,
                user_name,
                &supersession_user,
                recovery,
                Some(deadline),
                None,
                Some(serving_admission),
                priority,
            )
            .await?;
        replacement.publish_fenceable(&creation.info.session_id);
        Ok(ClusterSessionStart {
            info: creation.info,
            replacement,
            created: creation.created,
        })
    }

    fn require_cluster_serving_authority(
        &self,
        admission: ClusterServingAdmission,
    ) -> Result<(), String> {
        if tokio::time::Instant::now() >= admission.deadline {
            return Err(capacity_error(
                "the replacement start expired before it could finish provisional work",
            ));
        }
        self.serving_authority
            .is_current(admission.generation)
            .then_some(())
            .ok_or_else(|| {
                serving_fence_error("the node lost authority during cluster session creation")
            })
    }

    /// Acquire the player replacement serialization separately from worker
    /// creation. The takeover supervisor retains this guard while a child
    /// task performs cancellation-sensitive creation, so even a panic cannot
    /// reopen the player key before exact cleanup has been transferred.
    pub(crate) async fn acquire_cluster_takeover_replacement(
        &self,
        req: &SessionRequest,
        user_id: i64,
        deadline: tokio::time::Instant,
    ) -> Result<ClusterReplacementGuard, String> {
        let supersession_user = serde_json::json!(["user_id", user_id]).to_string();
        let gate_key =
            serde_json::json!([supersession_user.as_str(), req.playback_id.as_str()]).to_string();
        self.acquire_cluster_replacement_gate(
            gate_key,
            req.previous_session_id.as_deref(),
            deadline,
        )
        .await
    }

    /// Create the takeover worker while the caller owns the already-acquired
    /// replacement guard. `takeover.provisional_session_id` is fixed before
    /// this call so a supervising task can reap an exact late registration.
    pub(crate) async fn create_cluster_takeover_session_under_guard(
        &self,
        req: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        user_name: &str,
        deadline: tokio::time::Instant,
        takeover: SessionTakeoverStart,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["user_id", recovery.user_id]).to_string();
        // Same check the ordinary cluster start makes after its gate wait: a
        // start with no budget left cannot finish, and spawning ffmpeg only to
        // abandon it costs an admission slot for nothing.
        if tokio::time::Instant::now() >= deadline {
            return Err(capacity_error(
                "the takeover start expired while waiting for this player's gate",
            ));
        }
        self.create_session_inner(
            req,
            user_name,
            &supersession_user,
            recovery,
            Some(deadline),
            Some(takeover),
            None,
            Priority::Live,
        )
        .await
        .map(|creation| creation.info)
    }

    pub(super) async fn acquire_cluster_replacement_gate(
        &self,
        key: String,
        predecessor_session_id: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ClusterReplacementGuard, String> {
        for _ in 0..MAX_CLUSTER_REPLACEMENT_REENTRIES {
            let gate = {
                let mut entries = self
                    .cluster_replacement_gates
                    .entries
                    .lock()
                    .map_err(|_| "cluster replacement gate registry was poisoned".to_owned())?;
                entries.retain(|_, gate| gate.strong_count() > 0);
                if let Some(gate) = entries.get(&key).and_then(Weak::upgrade) {
                    gate
                } else {
                    if entries.len() >= MAX_CLUSTER_REPLACEMENT_GATES {
                        return Err(capacity_error(
                            "too many player replacements are active on this worker",
                        ));
                    }
                    let gate = Arc::new(ReplacementGate::new());
                    entries.insert(key.clone(), Arc::downgrade(&gate));
                    gate
                }
            };
            let gate_deadline = std::cmp::min(
                deadline,
                tokio::time::Instant::now() + CLUSTER_REPLACEMENT_GATE_WAIT,
            );
            let (gate, permit) =
                match tokio::time::timeout_at(gate_deadline, Arc::clone(&gate.lock).lock_owned())
                    .await
                {
                    Ok(permit) => (gate, permit),
                    Err(_) => match self.reclaim_stalled_replacement_gate(&key, &gate).await? {
                        Some(reclaimed) => reclaimed,
                        // A holder inside its budget keeps its player. The
                        // caller waits out the bounded refusal and re-posts.
                        None => {
                            return Err(replacement_wait_error(
                                "it has not finished releasing this player",
                            ))
                        }
                    },
                };
            if !gate.claim()? {
                // Reclaimed while this start queued. The permit serializes a
                // key nobody uses any more, so start over against whatever the
                // registry holds now.
                drop(permit);
                continue;
            }
            return Ok(ClusterReplacementGuard {
                registry: Arc::clone(&self.cluster_replacement_gates),
                key,
                gate,
                permit: Some(permit),
                // Acquired after serialization and retained across provisional
                // creation plus the durable activation verdict. The lease tick
                // reads workers before this registry, so it observes either the
                // predecessor process or this settlement protection while the
                // successor remains make-before-break provisional.
                _predecessor_settlement: predecessor_session_id.map(SessionSettlementGuard::begin),
            });
        }
        Err(replacement_wait_error(
            "this player's key changed hands repeatedly while the start waited",
        ))
    }

    /// Take a player's key back from a hold that can be proved not to need it.
    ///
    /// `Ok(None)` means the holder is a start still inside its budget. Those
    /// are never reclaimed: the work under this gate routinely takes tens of
    /// seconds — the incident that prompted this fix had two 50-second starts —
    /// so a timer short enough to unwedge a player is short enough to destroy
    /// every healthy one, including the re-posts of the client's own retry
    /// ladder. That is what the first version of this fix got wrong.
    ///
    /// Two things are provable instead. An **abandoned** hold has said so
    /// itself: its request is answered and its guard lives on inside cleanup,
    /// which is exactly the measured failure. A hold past
    /// [`CLUSTER_REPLACEMENT_HOLD_CEILING`] has outlived every budget it asked
    /// for, so whatever it is waiting on it is not going to finish in time to
    /// matter.
    ///
    /// Fencing first is what keeps the reclaim exact. An abandoned hold always
    /// has an id to fence, because the cleanup that abandons it publishes the
    /// session it is tearing down. Installing a fresh gate and retiring the old
    /// one is what keeps the reclaimed holder harmless: it keeps its own `Arc`
    /// and its own lock, and anyone still queued on that lock re-enters the
    /// registry instead of believing it serializes this player.
    async fn reclaim_stalled_replacement_gate(
        &self,
        key: &str,
        gate: &Arc<ReplacementGate>,
    ) -> Result<Option<(Arc<ReplacementGate>, tokio::sync::OwnedMutexGuard<()>)>, String> {
        let (reason, fenceable) = {
            let mut state = gate
                .state
                .lock()
                .map_err(|_| "cluster replacement gate state was poisoned".to_owned())?;
            if state.retired {
                // Someone else reclaimed this gate already; queue against
                // whatever they installed rather than retiring it twice.
                return Ok(None);
            }
            // No holder means the lock was released between the timeout and
            // here. Nothing to reclaim; the ordinary path will win it.
            let Some(holder) = state.holder.as_ref() else {
                return Ok(None);
            };
            let judged = if holder.abandoned {
                ("abandoned", holder.fenceable.clone())
            } else if holder.since.elapsed() >= CLUSTER_REPLACEMENT_HOLD_CEILING {
                ("hold_ceiling", holder.fenceable.clone())
            } else {
                // A start inside its budget keeps its player.
                return Ok(None);
            };
            // Retired in the same critical section that judged it, so a start
            // that wins the lock from here on cannot claim this gate and
            // become a second owner. Anything that won the lock BEFORE this
            // point already replaced `holder`, and the judgement above would
            // have seen that fresh hold and refused.
            state.retired = true;
            judged
        };
        if !fenceable.is_empty() {
            self.fence_sessions(&fenceable).await;
        }
        let successor = Arc::new(ReplacementGate::new());
        {
            let mut entries = self
                .cluster_replacement_gates
                .entries
                .lock()
                .map_err(|_| "cluster replacement gate registry was poisoned".to_owned())?;
            // Re-read under the registry lock: another arrival may have
            // reclaimed the same gate while this one awaited the fence. Its
            // successor is as good as ours, so queue against that instead of
            // installing a second one.
            if entries
                .get(key)
                .and_then(Weak::upgrade)
                .is_some_and(|current| !Arc::ptr_eq(&current, gate))
            {
                // Someone installed a successor while this reclaim awaited the
                // fence. Theirs is as good as ours, so stand down — and undo
                // the retirement, because a gate nobody will replace must not
                // be left refusing every claimant.
                if let Ok(mut state) = gate.state.lock() {
                    state.retired = false;
                }
                return Ok(None);
            }
            entries.insert(key.to_owned(), Arc::downgrade(&successor));
        }
        tracing::warn!(
            target: "plurxd::transcode",
            reason,
            fenced = fenceable.len(),
            "reclaimed a player's replacement key from a hold that could not use it"
        );
        crate::playback_control::record_replacement_reclaimed(reason);
        // Uncontended by construction — nothing else has reached this gate yet.
        let permit = Arc::clone(&successor.lock).lock_owned().await;
        Ok(Some((successor, permit)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn create_session_inner(
        &self,
        req: &SessionRequest,
        user_name: &str,
        supersession_user: &str,
        recovery: &SessionRecoveryIdentity,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        serving_admission: Option<ClusterServingAdmission>,
        priority: Priority,
    ) -> Result<SessionCreation, String> {
        if let Some(admission) = serving_admission {
            self.require_cluster_serving_authority(admission)?;
        }
        match (&req.previous_session_id, req.reopen_reason) {
            (None, None) | (Some(_), Some(_)) => {}
            _ => {
                return Err(invalid_reopen_error(
                    "previous_session_id and reopen_reason must be sent together",
                ))
            }
        }
        if req.reopen_reason.is_some() && req.request_id.is_none() {
            return Err(invalid_reopen_error(
                "a stall reopen requires request_id so its target can be replayed",
            ));
        }
        let claim = match req.request_id.as_deref() {
            Some(key) => match self.claim_request(key, req, supersession_user).await? {
                Claimed::Recovered(info) => {
                    if let Some(admission) = serving_admission {
                        self.require_cluster_serving_authority(admission)?;
                    }
                    return Ok(SessionCreation {
                        info,
                        created: false,
                    });
                }
                Claimed::Mine(claim, normalized) => Some((claim, normalized)),
            },
            None => None,
        };
        let normalized;
        let (claim, req) = match claim {
            Some((claim, request)) => {
                normalized = request;
                (Some(claim), &normalized)
            }
            None => (None, req),
        };
        if let Some(admission) = serving_admission {
            self.require_cluster_serving_authority(admission)?;
        }

        // Immutable VOD remains first. During the index backfill, a typed
        // prerequisite refusal may use the retained live engine rather than
        // turning background preparation into a catalogue-wide outage.
        let info = if req.presentation == Presentation::Live {
            let started = self
                .start_live_recovery_session(
                    req,
                    user_name,
                    supersession_user,
                    recovery,
                    replacement_deadline,
                    takeover,
                    priority,
                )
                .await?;
            // Not a fallback decision: the request named the live presentation
            // before it got here, and no setting was consulted. Counted after
            // the start succeeded, for the same reason as the arm below.
            record_live_recovery(LiveRecoveryReason::RequestedLive);
            started
        } else {
            let vod = self
                .try_vod_session(
                    req,
                    user_name,
                    supersession_user,
                    replacement_deadline,
                    takeover.is_some(),
                    serving_admission,
                )
                .await;
            match vod {
                Ok(info) => info,
                Err(error) => {
                    // One list, not two. `from_refusal` is what decides both
                    // whether a code may fall back and which reason it is
                    // counted under, so adding a fifth code cannot produce
                    // sessions the engine serves and nothing attributes.
                    let live_recovery_reason = vod_refusal(&error)
                        .and_then(|(code, _)| LiveRecoveryReason::from_refusal(code));
                    if let Some(reason) = live_recovery_reason {
                        if self.live_hls_recovery_enabled().await? {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                file_id = req.file_id,
                                refusal = reason.label(),
                                "VOD prerequisite unavailable; using temporary live-HLS recovery"
                            );
                            let started = self
                                .start_live_recovery_session(
                                    req,
                                    user_name,
                                    supersession_user,
                                    recovery,
                                    replacement_deadline,
                                    takeover,
                                    priority,
                                )
                                .await?;
                            // Counted after the start succeeded. `?` above is
                            // a refusal — capacity, spawn, deadline — and a
                            // metric whose HELP says "sessions served" must
                            // not include streams nobody ever received.
                            record_live_recovery(reason);
                            started
                        } else {
                            return Err(error);
                        }
                    } else {
                        return Err(error);
                    }
                }
            }
        };
        if let Some(claim) = claim {
            let mut live: std::collections::HashSet<String> =
                self.sessions.lock().await.keys().cloned().collect();
            // VOD sessions are live too: without them here, the next create's
            // completion would purge their Ready records, breaking both
            // idempotent replay and the cluster stop path's match check.
            live.extend(self.vod.session_ids().await);
            claim.complete(&info.session_id, &live);
        }
        Ok(SessionCreation {
            info,
            created: true,
        })
    }

    /// Whether a typed VOD prerequisite refusal may fall back to the retained
    /// live engine. Absent means yes: availability first, unless an operator
    /// turns it off in Settings → Playback → Streaming.
    ///
    /// One rule for every build. This used to read `== Some("1")` under
    /// `cfg(test)` and `!= Some("0")` otherwise, so every VOD-refusal
    /// regression exercised a policy production never runs — the suite could
    /// be green on a fallback path the fleet takes and the tests never enter.
    /// Tests that want the refusal now say so, which is one line each and
    /// leaves the default meaning the same thing everywhere.
    /// Whether a VOD prerequisite failure may fall back to the retained
    /// growing-HLS engine.
    ///
    /// `pub(crate)` because takeover consults it too. That was the fourth
    /// reader of this setting, and adding a reader is what settled the parse:
    /// three sites compared the raw string against `Some("0")` and the
    /// Developer card's comment pinned itself to that on purpose, so that the
    /// row could not report a switch the engine was not honouring. They all
    /// share `stored_switch` now, which keeps that pin while giving a
    /// hand-written ` OFF ` the answer an operator would expect.
    pub(crate) async fn live_hls_recovery_enabled(&self) -> Result<bool, String> {
        let configured = self
            .store
            .get_setting(plurx_core::store::keys::VOD_LIVE_RECOVERY)
            .await
            .map_err(|error| {
                start_infrastructure_error(format!("reading live-HLS recovery setting: {error}"))
            })?;
        Ok(plurx_core::store::stored_switch(
            configured.as_deref(),
            true,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_live_recovery_session(
        &self,
        req: &SessionRequest,
        user_name: &str,
        supersession_user: &str,
        recovery: &SessionRecoveryIdentity,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        priority: Priority,
    ) -> Result<StartInfo, String> {
        match req.kind {
            SessionKind::Transcode { height } => {
                self.start_with_audio_offset(
                    req.file_id,
                    height,
                    req.start_seconds,
                    req.audio_index,
                    req.subtitle_burn,
                    req.audio_offset_ms,
                    user_name,
                    supersession_user,
                    recovery,
                    replacement_deadline,
                    takeover,
                    &req.playback_id,
                    req.automatic,
                    req.hdr10,
                    priority,
                )
                .await
            }
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } => {
                self.start_copy_with_audio_offset(
                    req.file_id,
                    req.start_seconds,
                    req.audio_index,
                    req.audio_offset_ms,
                    CopySessionOptions {
                        convert_dolby_vision,
                        transcode_audio: aac,
                        preserve_dolby_vision,
                    },
                    user_name,
                    supersession_user,
                    recovery,
                    replacement_deadline,
                    takeover,
                    &req.playback_id,
                    req.automatic,
                )
                .await
            }
        }
    }

    /// Whether the subtitle-source store holds this track as a real track
    /// with no cues, by the burn path's rule. Uncounted; see
    /// [`crate::subtitle_source::stored_as_empty`].
    async fn burn_track_is_stored_empty(
        &self,
        file: &plurx_core::domain::MediaFile,
        index: i64,
    ) -> bool {
        crate::subtitle_source::stored_as_empty(
            &self.subtitle_source_access(),
            file,
            index,
            crate::subtitle_source::Live::Path(&file.path),
        )
        .await
    }

    /// Freeze an executable encoded recipe before any rendition is named.
    /// Copy remains index-driven; selecting burn pixels requires an encoder
    /// even when the incoming request otherwise asks for source quality.
    pub(super) async fn prepare_vod_encoding(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        if matches!(req.kind, SessionKind::Copy { .. }) {
            match req.subtitle_burn {
                None => return Ok(None),
                // A copy whose burn track the store holds as having no cues
                // has nothing to burn: it stays the copy it would have been,
                // rather than a full re-encode to overlay nothing.
                Some(index) if self.burn_track_is_stored_empty(file, index).await => {
                    tracing::info!(
                        target: "plurxd::transcode",
                        file_id = file.id,
                        subtitle_index = index,
                        "the burn track has no cues; serving the copy without an overlay"
                    );
                    return Ok(None);
                }
                Some(_) => {}
            }
        }
        let source = crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .map_err(|error| {
                vod_refusal_error(
                    "vod_source_rescan_required",
                    format!("the source could not be held for encoded preparation: {error}"),
                )
            })?;
        // Bind preparation, burn extraction, key construction, and the final
        // producer open to the same inspected object, not scanner seconds.
        let source_object_version = source.object_version().to_owned();
        let source_height = file.height.filter(|height| *height >= 2).ok_or_else(|| {
            vod_refusal_error(
                "vod_video_geometry_unknown",
                "the source has no usable video height",
            )
        })?;
        let target_height = match req.kind {
            SessionKind::Transcode { height } if height > 0 => height.min(source_height),
            SessionKind::Copy { .. } => source_height,
            _ => {
                return Err(vod_refusal_error(
                    "vod_invalid_height",
                    "the requested height must be positive",
                ))
            }
        }
        .max(2)
            & !1;
        if plurx_core::transcode::output_size(file, target_height).is_none() {
            return Err(vod_refusal_error(
                "vod_video_geometry_unknown",
                "the source has no usable video dimensions",
            ));
        }
        if req
            .audio_index
            .is_some_and(|index| index < 0 || index as usize >= file.audio_streams.len())
        {
            return Err(vod_refusal_error(
                "vod_audio_track_missing",
                "the requested audio track no longer exists",
            ));
        }
        let subtitle_burn = req
            .subtitle_burn
            .map(|index| {
                let stream = usize::try_from(index)
                    .ok()
                    .and_then(|index| file.subtitle_streams.get(index))
                    .ok_or_else(|| {
                        vod_refusal_error(
                            "vod_subtitle_track_missing",
                            "the requested subtitle track no longer exists",
                        )
                    })?;
                Ok::<_, String>(plurx_core::transcode::SubtitleBurn {
                    subtitle_index: index,
                    bitmap: plurx_core::tracks::is_bitmap_subtitle(&stream.codec),
                })
            })
            .transpose()?;
        if let Some(burn) = &subtitle_burn {
            if let Some(reason) = crate::pipeprobe::burn_filters().await.refusal(burn.bitmap) {
                return Err(unsupported_build_error(reason));
            }
        }
        let probe = self
            .store
            .get_file_probe_json(file.id)
            .await
            .map_err(|error| {
                start_infrastructure_error(format!("reading the stored source probe: {error}"))
            })?;
        let held_probe = crate::ffmpeg::held_source_probe_json(&source.handle)
            .await
            .map_err(|error| {
                vod_refusal_error(
                    "vod_source_rescan_required",
                    format!("the held source could not be verified against its scan: {error}"),
                )
            })?;
        let comparison = probe
            .as_deref()
            .map(|stored| crate::ffmpeg::compare_probe_documents(stored, &held_probe))
            .transpose()
            .map_err(|error| {
                vod_refusal_error(
                    "vod_source_rescan_required",
                    format!("the stored source probe cannot be verified: {error}"),
                )
            })?;
        if comparison
            .as_ref()
            .is_some_and(|result| result.admitted_on_reporter_drift)
        {
            // Attributable by design. This source was admitted on its media
            // facts rather than on the whole document, because the stored scan
            // and this node's FFprobe are different builds. Reanalyzing the
            // item restores the stricter comparison.
            crate::ffmpeg::note_reporter_drift_admission();
            tracing::info!(
                target: "plurxd::transcode",
                file_id = file.id,
                "admitted a held source on its media facts: its stored scan came from a \
                 different FFprobe build"
            );
        }
        if !comparison.as_ref().is_some_and(|result| result.same) {
            // Why this refused belongs in the product. `ps auxwww` on the box
            // is not an acceptable answer for work this server refuses, and
            // neither is a log line only an operator with a shell can read.
            // The normalized field paths are built to be safe to show —
            // bounded to eight, no values, no pathname, no container tag text
            // — and they come from the same normalized documents the verdict
            // used, so the explanation cannot contradict the decision. The
            // typed code is unchanged, so every client keeps its existing
            // terminal classification; only the sentence gets useful.
            let detail = match comparison.as_ref() {
                // A file that was never probed takes this branch too. It is
                // an unknown source, not a changed one, and saying so is the
                // difference between a five-minute repair and a snapshot dig.
                None => "this file has no stored probe".to_string(),
                Some(result) => format!(
                    "the stored probe differs from the source at {}",
                    result.rendered_differences()
                ),
            };
            tracing::warn!(
                target: "plurxd::transcode",
                file_id = file.id,
                detail = %detail,
                "refusing an encoded session: the held source does not match its stored probe"
            );
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                format!("{detail}; reanalyze this item before playback"),
            ));
        }
        let grid = crate::vodencode::frame_grid(probe.as_deref()).ok_or_else(|| {
            vod_refusal_error(
                "vod_frame_cadence_unknown",
                "the source probe has no usable video cadence; rescan the file",
            )
        })?;
        let (mut encoder, mut grade) = self
            .encoder_and_grade_for(file, req.hdr10, target_height, subtitle_burn.is_some())
            .await?;
        // After the encoder and grade, deliberately. `encoder_and_grade_for`
        // can refuse this source outright (an unknown Dolby Vision profile, an
        // unproven Profile 5 renderer), and a refusal must not first start a
        // detached full-source extraction and answer "pending" while it runs.
        // A `Nothing` answer chooses the encoder and grade again without the
        // burn: the HTTP layer admits an HDR delivery whose burn track the
        // store holds as `empty`, and that session must keep its range.
        let (subtitle_burn, burn_file) = match subtitle_burn {
            Some(burn) => {
                // The only caller that passes the short budget. A start has 50 s
                // for everything; a cold burn sidecar on a remux of this size needs
                // 400. Refusing in seconds with a pending answer is the only thing
                // that leaves the viewer better off — including on the speculative
                // prepared-successor path, which would otherwise hold a preparation
                // slot for the length of a full-film demux.
                // Read lazily: a warm sidecar never reads the setting.
                let stored = self.subtitle_source_access();
                match crate::subtitles::ensure_burn_source(
                    &self.subtitle_cache,
                    file,
                    burn.subtitle_index,
                    Some(&source_object_version),
                    crate::subtitles::SIDECAR_JOIN_BUDGET,
                    &stored,
                )
                .await?
                {
                    crate::subtitles::BurnSource::File(handle) => (Some(burn), Some(handle)),
                    crate::subtitles::BurnSource::Nothing => {
                        tracing::info!(
                            target: "plurxd::transcode",
                            file_id = file.id,
                            subtitle_index = burn.subtitle_index,
                            "the selected subtitle track has no cues; starting without an overlay"
                        );
                        // The HTTP HDR guard now lets an `empty` track through
                        // on an HDR delivery, so the grade chosen for a burn
                        // is no longer always the burn-free one: choose again
                        // without the burn, and keep the range.
                        (encoder, grade) = self
                            .encoder_and_grade_for(file, req.hdr10, target_height, false)
                            .await?;
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };
        let software_threads = Workload::of(file, target_height)
            .software_threads()
            .min(self.software_budget().await)
            .max(1) as u32;
        let mut options = self.live_lookup_options(
            self.rate_control_snapshot(),
            encoder,
            file,
            target_height,
            0.0,
            req.audio_index,
            subtitle_burn,
            Some(software_threads),
            grade,
        );
        let subtitle = if let Some(subtitle) = burn_file {
            #[cfg(unix)]
            {
                options.subtitle_file = Some("/dev/fd/5".into());
            }
            #[cfg(windows)]
            {
                options.subtitle_file = Some(
                    plurx_core::fs_secure::std_file_path(&subtitle).map_err(|error| {
                        vod_refusal_error(
                            "vod_decoder_plan_refused",
                            format!("the held subtitle path could not be resolved: {error}"),
                        )
                    })?,
                );
            }
            Some(Arc::new(subtitle))
        } else {
            None
        };
        let held_plan_handle = source.handle.try_clone().map(Arc::new).map_err(|error| {
            vod_refusal_error(
                "vod_decoder_plan_refused",
                format!("the held source could not be retained for decoder planning: {error}"),
            )
        })?;
        let plan = BoundPlanCaller::Vod.finish(
            self.resolve_held_movie_plan(
                file,
                &options,
                encoder,
                crate::decode_facts::DecodeFactSource::new(
                    held_plan_handle,
                    Arc::new(tokio::sync::Semaphore::new(1)),
                ),
                Instant::now() + DECODE_PLAN_PROBE_BUDGET,
                None,
            )
            .await,
        )?;
        let resources = TranscodeResourceEstimate::of(&plan, &Workload::of(file, target_height));
        if !source.unchanged() {
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                "the source changed during encoded recipe preparation; rescan it before playback",
            ));
        }
        let subtitle_digest = if let Some(subtitle) = &subtitle {
            Some(
                crate::vodencode::digest_subtitle(subtitle)
                    .await
                    .map_err(start_infrastructure_error)?,
            )
        } else {
            None
        };
        let engine = crate::ffmpeg::EncodedEngine::capture(
            options
                .subtitle_burn
                .as_ref()
                .is_some_and(|burn| !burn.bitmap),
        )
        .await
        .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
        if !source.unchanged() {
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                "the source changed while attesting the encoded engine; rescan it before playback",
            ));
        }
        Ok(Some(Arc::new(crate::vodencode::Encoding {
            source_object_version,
            plan,
            resources,
            options,
            grid,
            subtitle,
            subtitle_digest,
            ffmpeg_build: crate::ffmpeg::ffmpeg_build().await,
            executable: crate::ffmpeg::EncodedExecutable::capture()
                .await
                .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?,
            engine,
            admissions: self.admissions.clone(),
            store: Arc::clone(&self.store),
            speculative: std::sync::atomic::AtomicBool::new(false),
            queued: std::sync::Mutex::new(None),
            policy_retry: std::sync::atomic::AtomicBool::new(false),
            handoff_wait: std::sync::atomic::AtomicBool::new(false),
            last_refusal: std::sync::Mutex::new(None),
            handoff_claim: std::sync::Mutex::new(None),
            #[cfg(test)]
            admission_pause: std::sync::Mutex::new(None),
        })))
    }

    /// The only public HLS presentation. A request either receives immutable
    /// VOD or fails with a stable refusal; it never enters the live arms.
    async fn try_vod_session(
        &self,
        req: &SessionRequest,
        user_name: &str,
        supersession_user: &str,
        replacement_deadline: Option<tokio::time::Instant>,
        is_takeover: bool,
        serving_admission: Option<ClusterServingAdmission>,
    ) -> Result<StartInfo, String> {
        if let Some(admission) = serving_admission {
            self.require_cluster_serving_authority(admission)?;
        }
        if req.presentation != Presentation::Vod {
            return Err(vod_refusal_error(
                "live_presentation_removed",
                "the growing live HLS presentation has been removed; create a VOD session",
            ));
        }
        if is_takeover {
            return Err(vod_refusal_error(
                "vod_reopen_required",
                "this session must be reopened as a new VOD handle after owner takeover",
            ));
        }
        let Some(settings) = self.vod_settings(req).await? else {
            return Err(vod_refusal_error(
                "vod_disabled",
                "VOD session creation is disabled on this server",
            ));
        };
        let mut file = self
            .store
            .get_file(req.file_id)
            .await
            .map_err(|error| {
                start_infrastructure_error(format!("reading the source file: {error}"))
            })?
            .ok_or_else(|| "the file no longer exists".to_owned())?;
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            req.audio_offset_ms.clamp(-15_000, 15_000)
        };
        let encoding = self.prepare_vod_encoding(req, &file).await?;
        let target_height = encoding
            .as_ref()
            .map_or(file.height.unwrap_or(0), |encoding| {
                encoding.options.target_height
            });
        let grade = encoding.as_ref().map_or(OutputGrade::Sdr, |encoding| {
            encoding.options.pipeline.output_grade()
        });
        let kind = encoding
            .as_ref()
            .map_or(req.kind, |encoding| SessionKind::Transcode {
                height: encoding.options.target_height,
            });
        let encoder = encoding
            .as_ref()
            .map_or("vod", |encoding| encoding.plan.encoder().label());
        let codec_qualification = encoding.as_ref().map(|encoding| {
            (
                encoding.plan.encoder(),
                encoding.options.pipeline.output_grade(),
                encoding.options.pipeline,
            )
        });
        let prepared = crate::vodserve::VodRecipeRequest {
            request: req,
            encoding,
        };
        // Cluster activation is make-before-break: the Store pointer CAS and
        // exact post-CAS terminal projection are the only operations allowed
        // to retire the authoritative predecessor. If provisional capacity is
        // unavailable, fail this replacement and leave the current player
        // intact. Legacy process-local callers retain their historical sweep.
        if replacement_deadline.is_none() {
            self.reap_superseded_before(None, supersession_user, &req.playback_id)
                .await?;
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let item_title = self
            .store
            .get_item(file.item_id)
            .await
            .ok()
            .flatten()
            .map(|item| item.title)
            .unwrap_or_else(|| format!("#{}", file.item_id));
        let attribution = crate::vodserve::VodAttribution {
            user_name,
            item_title: &item_title,
            supersession_user,
        };
        let start = if let Some(admission) = serving_admission {
            self.vod
                .try_create_cluster(
                    prepared,
                    &file,
                    &settings,
                    attribution,
                    session_id,
                    crate::vodserve::VodServingAdmission::new(
                        self.serving_authority.clone(),
                        admission.generation,
                        admission.deadline.into_std(),
                    ),
                )
                .await
                .map_err(|error| {
                    if vod_refusal(&error).is_some()
                        || is_serving_fence_error(&error)
                        || is_retryable_capacity_error(&error)
                        || unsupported_build_reason(&error).is_some()
                    {
                        error
                    } else {
                        start_infrastructure_error(error)
                    }
                })?
        } else {
            self.vod
                .try_create(prepared, &file, &settings, attribution, session_id)
                .await?
        };
        if let Some((encoder, grade, pipeline)) = codec_qualification {
            self.record_codec_qualification_session(encoder, grade, Some(pipeline));
        }
        Ok(StartInfo {
            playlist_url: format!("/api/v1/hls/{}/index.m3u8", start.session_id),
            session_id: start.session_id,
            duration_ms: Some(start.duration_ms),
            // Like a cached generation: the timeline is the whole film from
            // zero, and the client seeks — that is the point.
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            target_height,
            kind,
            encoder,
            // A copy session encodes nothing; same answer the old copy arm
            // gave without retaining its live presentation.
            grade,
            vod: true,
            control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        })
    }

    /// Read the VOD serving settings. Absence means enabled; an explicit `0`
    /// is a maintenance kill switch that refuses playback rather than routing
    /// it through the removed live presentation.
    pub(super) async fn vod_settings(
        &self,
        req: &SessionRequest,
    ) -> Result<Option<crate::vodserve::VodSettings>, String> {
        let read = |key: &'static str| {
            let store = Arc::clone(&self.store);
            async move {
                store
                    .get_setting(key)
                    .await
                    .map_err(|error| start_infrastructure_error(format!("reading {key}: {error}")))
            }
        };
        if read(plurx_core::store::keys::VOD_PRESENTATION)
            .await?
            .is_some_and(|value| value.trim() == "0")
        {
            return Ok(None);
        }
        /// Un-admitted working sets across the node when the operator has not
        /// said otherwise: enough for a handful of concurrent films' ahead
        /// windows without threatening a small disk.
        const DEFAULT_WORKING_SET_BYTES: u64 = 8 << 30;
        /// The server's ceiling on one blocking segment fetch. hls.js's own
        /// manifest-load budget is 10 s (M0-P3), so the default answer comes
        /// back typed before a stock player gives up on its own.
        const DEFAULT_BLOCK_BUDGET_SECS: f64 = 8.0;
        const MAX_BLOCK_BUDGET_SECS: f64 = 30.0;
        const DEFAULT_MATERIALIZE_BUDGET_SECS: f64 = 30.0;
        const MAX_MATERIALIZE_BUDGET_SECS: f64 = 300.0;
        /// Sixteen viewers each at the per-session cap of four. The ceiling is
        /// a sanity bound, not a capacity claim: each parked GET holds a
        /// response open and a retention pin, so an unbounded value lets one
        /// seek storm park work until the node runs out of sockets.
        const DEFAULT_BLOCKED_GET_CAP: usize = 64;
        const MAX_BLOCKED_GET_CAP: usize = 4_096;
        let working_set_bytes = match read(plurx_core::store::keys::VOD_WORKING_SET_BYTES).await? {
            Some(raw) => match raw.trim().parse::<u64>() {
                // The settings surface refuses a zero on the way in; one that
                // arrived by another route is still not a budget this can run
                // with, and "not configured" is the honest reading.
                Ok(0) | Err(_) => DEFAULT_WORKING_SET_BYTES,
                Ok(bytes) => bytes,
            },
            None => DEFAULT_WORKING_SET_BYTES,
        };
        let server_cap = match read(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS).await? {
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| s.min(MAX_BLOCK_BUDGET_SECS))
                .unwrap_or(DEFAULT_BLOCK_BUDGET_SECS),
            None => DEFAULT_BLOCK_BUDGET_SECS,
        };
        let block_secs = req
            .block_budget_secs
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(|s| s.min(server_cap))
            .unwrap_or(server_cap);
        let materialize_secs =
            match read(plurx_core::store::keys::VOD_MATERIALIZE_BUDGET_SECS).await? {
                Some(raw) => raw
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s >= 10.0)
                    .map(|s| s.min(MAX_MATERIALIZE_BUDGET_SECS))
                    .unwrap_or(DEFAULT_MATERIALIZE_BUDGET_SECS),
                None => DEFAULT_MATERIALIZE_BUDGET_SECS,
            };
        // Admitted renditions are the copy cache, so they answer to the same
        // budget the pre-transcode cache does. `0`/absent keeps admission
        // closed: renditions serve and evict under the working set, and
        // nothing is promised durability.
        let completed_cache_bytes = match read(plurx_core::store::keys::CACHE_MAX_GB).await? {
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .unwrap_or(0)
                .saturating_mul(1 << 30),
            None => 0,
        };
        // Absent or unparseable keeps the built-in default: a node that has
        // never been tuned still bounds its parked work. Clamped rather than
        // trusted, because a zero would refuse every blocked GET — turning
        // every seek into an immediate 503 — and an unbounded value would let
        // one seek storm park work until the node ran out of sockets.
        let blocked_get_cap = match read(plurx_core::store::keys::VOD_BLOCKED_GET_CAP).await? {
            Some(raw) => raw
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|cap| *cap > 0)
                .map(|cap| cap.min(MAX_BLOCKED_GET_CAP))
                .unwrap_or(DEFAULT_BLOCKED_GET_CAP),
            None => DEFAULT_BLOCKED_GET_CAP,
        };
        Ok(Some(crate::vodserve::VodSettings {
            working_set_bytes,
            completed_cache_bytes,
            block_budget: Duration::from_secs_f64(block_secs),
            materialize_budget: Duration::from_secs_f64(materialize_secs),
            blocked_get_cap,
        }))
    }

    /// The VOD dispatch half of [`Self::playlist`]: `None` when the id is not
    /// a VOD session's.
    pub(crate) async fn vod_playlist(
        &self,
        session_id: &str,
    ) -> Option<VodResponsePublication<Vec<u8>>> {
        self.vod
            .playlist(session_id)
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    /// The VOD dispatch half of [`Self::segment`]: `None` when the id is not
    /// a VOD session's.
    pub(crate) async fn vod_segment(
        &self,
        session_id: &str,
        name: &str,
    ) -> Option<VodResponsePublication<Option<crate::vodserve::SegmentReady>>> {
        self.vod
            .segment(session_id, name)
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    pub(crate) async fn vod_segment_before(
        &self,
        session_id: &str,
        name: &str,
        deadline: Instant,
    ) -> Option<VodResponsePublication<Option<crate::vodserve::SegmentReady>>> {
        self.vod
            .segment_before(session_id, name, Some(deadline))
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    /// True for an attached VOD capability or one still in the slow
    /// resurrection preparation window. Lease loss uses this classification
    /// to close the stable release generation before a late attachment.
    pub(crate) async fn vod_owns_or_preparing(&self, session_id: &str) -> bool {
        self.vod.owns_or_preparing(session_id).await
    }

    pub(crate) fn begin_vod_preparation(
        &self,
        session_id: &str,
    ) -> crate::vodserve::VodPreparationGuard {
        self.vod.begin_preparing_session(session_id)
    }

    pub(crate) async fn vod_live_or_preparing_session_ids(&self) -> Vec<String> {
        self.vod.live_or_preparing_session_ids().await
    }

    /// Frozen source facts carried by the exact VOD response owner. Transforms
    /// must not resolve these through the reusable live session id: a
    /// resurrection can replace that attachment while preparation is in flight.
    pub(crate) fn vod_file_for_owner(
        &self,
        owner: &MediaResponseOwner,
    ) -> Option<plurx_core::domain::MediaFile> {
        let MediaResponseOwnerKind::Vod(owner) = &owner.0 else {
            return None;
        };
        Some(self.vod.response_owner_file(owner))
    }

    /// Live (un-tombstoned) VOD session ids, for operator surfaces.
    pub async fn vod_live_session_ids(&self) -> Vec<String> {
        self.vod.live_session_ids().await
    }

    /// Rebuild a reaped VOD session from its durable route's recipe (plan
    /// §2.5: sessions are handles, and a handle whose durable route is still
    /// active resurrects instead of failing the viewer). The caller has
    /// already verified the route: this node owns it, it is active, and its
    /// lease has not expired. `false` when the recipe is not a VOD one or the
    /// rendition cannot be re-attached — the caller then answers as it always
    /// has.
    /// Resurrect within the caller's absolute request deadline. Preparation
    /// may be cancelled before attachment, while VodServe's final reader-graph
    /// and registry swap is synchronous after it owns every affected lock, so
    /// timeout cannot expose a half-attached incarnation.
    pub(crate) async fn vod_resurrect_before(
        &self,
        recipe_json: &str,
        session_id: &str,
        user_id: i64,
        adoption: SessionAdoptionToken,
        deadline: Instant,
        speculative: bool,
    ) -> bool {
        if Instant::now() >= deadline {
            return false;
        }
        // Publish VOD intent before the first settings/file/user Store await.
        // Lease loss must close this exact adoption generation even while
        // resurrection is still gathering inputs and no session is attached.
        let _vod_preparing = self.vod.begin_preparing_session(session_id);
        let release_gate = &adoption.gate;
        if release_gate.released.load(Acquire) {
            return false;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            let Ok(remote) =
                serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(recipe_json)
            else {
                return false;
            };
            let req = remote.request;
            if req.presentation != Presentation::Vod {
                return false;
            }
            let Ok(Some(settings)) = self.vod_settings(&req).await else {
                return false;
            };
            let Ok(Some(mut file)) = self.store.get_file(req.file_id).await else {
                return false;
            };
            file.audio_offset_ms = if file.audio_streams.is_empty() {
                0
            } else {
                req.audio_offset_ms.clamp(-15_000, 15_000)
            };
            let Ok(encoding) = self.prepare_vod_encoding(&req, &file).await else {
                return false;
            };
            if speculative {
                if let Some(encoding) = encoding.as_ref() {
                    encoding.mark_speculative();
                }
            }
            let user_name = self
                .store
                .get_user(user_id)
                .await
                .ok()
                .flatten()
                .map(|user| user.username)
                .unwrap_or_else(|| format!("user #{user_id}"));
            let item_title = self
                .store
                .get_item(file.item_id)
                .await
                .ok()
                .flatten()
                .map(|item| item.title)
                .unwrap_or_else(|| format!("#{}", file.item_id));
            let supersession_user = serde_json::json!(["user_id", user_id]).to_string();
            match self
                .vod
                .try_create_before_release(
                    crate::vodserve::VodRecipeRequest {
                        request: &req,
                        encoding,
                    },
                    &file,
                    &settings,
                    crate::vodserve::VodAttribution {
                        user_name: &user_name,
                        item_title: &item_title,
                        supersession_user: &supersession_user,
                    },
                    session_id.to_owned(),
                    crate::vodserve::VodReleaseFence::new(
                        Arc::clone(&release_gate.transition),
                        &release_gate.released,
                    ),
                )
                .await
            {
                Ok(_) => {
                    if speculative {
                        self.vod
                            .mark_prepared_incarnation(session_id, &remote.incarnation_id)
                            .await;
                    }
                    tracing::info!(
                        target: "plurxd::transcode",
                        session = %session_log_id(session_id),
                        "resurrected a vod session from its durable route"
                    );
                    true
                }
                _ => false,
            }
        })
        .await
        .unwrap_or(false)
    }

    /// The VOD serving maintenance loop, spawned beside [`Self::reap_loop`].
    pub async fn vod_maintain_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.vod.maintain().await;
        }
    }

    /// Resolve a `request_id` to either a reservation this call owns or the
    /// session an identical create already made. A new claim normalizes a
    /// bound stall reopen and records its resolved height in the claim before
    /// returning to the caller; session creation (and therefore predecessor
    /// supersession) cannot begin earlier. Replays compare client intent and
    /// recover the stored result without reading the predecessor again.
    pub(super) async fn claim_request(
        &self,
        key: &str,
        request: &SessionRequest,
        supersession_user: &str,
    ) -> Result<Claimed<'_>, String> {
        let intent_fingerprint = request.intent_fingerprint(supersession_user);
        let deadline = Instant::now() + INFLIGHT_WAIT;
        loop {
            // What the map says right now, decided under one lock so there is
            // no gap between reading the entry and reserving the key.
            enum Step {
                Reserved,
                Wait,
                Recover(String, Option<i64>),
            }
            let step = {
                let mut requests = self.requests.lock().expect("requests mutex");
                match requests.get(key) {
                    Some(entry) if entry.intent_fingerprint != intent_fingerprint => {
                        return Err(format!(
                            "request {key} was already used for a different stream"
                        ));
                    }
                    Some(RequestEntry {
                        state: RequestState::InFlight,
                        ..
                    }) => Step::Wait,
                    Some(RequestEntry {
                        state: RequestState::Ready(session_id),
                        target_height,
                        ..
                    }) => Step::Recover(session_id.clone(), *target_height),
                    None => {
                        requests.insert(
                            key.to_owned(),
                            RequestEntry {
                                intent_fingerprint: intent_fingerprint.clone(),
                                target_height: None,
                                state: RequestState::InFlight,
                            },
                        );
                        Step::Reserved
                    }
                }
            };
            match step {
                Step::Reserved => {
                    let claim = RequestClaim {
                        requests: &self.requests,
                        key: Some(key.to_owned()),
                    };
                    let (normalized, target_height) = match self
                        .normalize_claimed_request(request, supersession_user)
                        .await
                    {
                        Ok(normalized) => normalized,
                        Err(error) => {
                            drop(claim);
                            return Err(error);
                        }
                    };
                    {
                        let mut requests = self.requests.lock().expect("requests mutex");
                        let Some(entry) = requests.get_mut(key) else {
                            return Err("the request claim disappeared during normalization".into());
                        };
                        entry.target_height = target_height;
                    }
                    return Ok(Claimed::Mine(claim, normalized));
                }
                Step::Recover(session_id, persisted_target) => {
                    if let Some(info) = self.recover(&session_id).await {
                        if persisted_target.is_some_and(|height| height != info.target_height) {
                            return Err(format!(
                                "request {key} resolved to a target that differs from its session"
                            ));
                        }
                        tracing::debug!(target: "plurxd::transcode", session = %session_log_id(&session_id), request_id = key, "idempotent create: same session");
                        return Ok(Claimed::Recovered(info));
                    }
                    // Its session is gone; the entry is stale, not
                    // authoritative. Remove exactly the entry that was seen —
                    // a peer may have re-reserved the key meanwhile — and
                    // re-decide from the top.
                    let mut requests = self.requests.lock().expect("requests mutex");
                    if matches!(requests.get(key), Some(RequestEntry { state: RequestState::Ready(sid), .. }) if *sid == session_id)
                    {
                        requests.remove(key);
                    }
                }
                Step::Wait => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "a create with request id {key} has been in flight for over {}s; \
                             assume it died and retry",
                            INFLIGHT_WAIT.as_secs()
                        ));
                    }
                    tokio::time::sleep(INFLIGHT_POLL).await;
                }
            }
        }
    }

    /// Resolve one bound stall request from the exact predecessor session.
    /// All predecessor facts are copied under one session-map lock, so the
    /// rung is observed once even if a seek or another device supersedes that
    /// session immediately after this read.
    async fn normalize_claimed_request(
        &self,
        request: &SessionRequest,
        supersession_user: &str,
    ) -> Result<(SessionRequest, Option<i64>), String> {
        let Some(previous_session_id) = request.previous_session_id.as_deref() else {
            let target_height = match request.kind {
                SessionKind::Transcode { height } => Some(height),
                SessionKind::Copy { .. } => None,
            };
            return Ok((request.clone(), target_height));
        };
        let Some(ReopenReason::Stall) = request.reopen_reason else {
            return Err(invalid_reopen_error("unsupported reopen reason"));
        };
        // A stall reopen bound to a VOD predecessor: validate the binding
        // against the VOD registry and pass the request through untouched. A
        // VOD session has no persisted rung to inherit — the reopen decides
        // its own presentation, so a client falling back to the live one
        // simply omits the flag.
        if let Some(facts) = self.vod.reopen_facts(previous_session_id).await {
            if facts.supersession_user != supersession_user
                || facts.playback_id != request.playback_id
                || facts.file_id != request.file_id
            {
                return Err(invalid_reopen_error(
                    "the previous session does not belong to this user, playback, and file",
                ));
            }
            let target_height = match request.kind {
                SessionKind::Transcode { height } if !request.automatic => Some(height),
                _ => None,
            };
            return Ok((request.clone(), target_height));
        }
        let (
            previous,
            previous_user,
            previous_playback,
            previous_file,
            previous_height,
            automatic,
            kind,
        ) = {
            let sessions = self.sessions.lock().await;
            let previous = sessions
                .get(previous_session_id)
                .ok_or_else(|| invalid_reopen_error("the previous session is no longer running"))?;
            (
                Arc::clone(previous),
                previous.supersession_user.clone(),
                previous.playback_id.clone(),
                previous.file_id,
                previous.target_height,
                previous.automatic,
                previous.kind,
            )
        };
        if previous_user != supersession_user
            || previous_playback != request.playback_id
            || previous_file != request.file_id
        {
            return Err(invalid_reopen_error(
                "the previous session does not belong to this user, playback, and file",
            ));
        }

        // The rung step exists for a link that could not keep up. A predecessor
        // that stopped completing deliveries while media it had not fetched was
        // published did not prove that: nothing was flowing to be too slow. The
        // decision is made here, before the create is persisted, so a transport
        // replay of the same request returns the same answer.
        let wedged = delivery_wedge_signature(&previous).await;

        let mut normalized = request.clone();
        normalized.automatic = automatic;
        normalized.kind = if automatic && !wedged {
            // One rung is the server-owned upper bound, not a reason to ignore
            // stronger link evidence from the player that actually stalled.
            // A lower requested rung can only make the retry safer; a stale or
            // maliciously higher request can never prevent the bounded step.
            let client_height = match request.kind {
                SessionKind::Transcode { height } => height,
                SessionKind::Copy { .. } => previous_height,
            };
            SessionKind::Transcode {
                height: one_rung_below(previous_height).min(client_height),
            }
        } else {
            // A wedged predecessor's successor is the same delivery again: a
            // copy stays a copy, at the rung the viewer was already watching.
            kind
        };

        let target_height = if wedged {
            previous_height
        } else {
            match normalized.kind {
                SessionKind::Transcode { height } => height,
                SessionKind::Copy { .. } => previous_height,
            }
        };
        Ok((normalized, Some(target_height)))
    }
}
