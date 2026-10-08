enum VodRecipeAttachment {
    Recovery { speculative: bool },
    FirstPreparation,
}

/// Cancellation after actual capture still retires only that exact private owner.
struct FirstPreparationCleanup {
    vod: Arc<crate::vodserve::VodServe>,
    session_id: String,
    owner: Option<crate::vodserve::ResponseOwner>,
}
impl Drop for FirstPreparationCleanup {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let vod = self.vod.clone();
            let session_id = self.session_id.clone();
            tokio::spawn(async move {
                vod.end_for_owner(&session_id, &owner).await;
            });
        }
    }
}

use super::*;

fn recovered_retained_output_matches(
    capture: &crate::vodserve::RetainedOutputCapture,
    candidate_expected: Option<&RetainedOutputFacts>,
    actual: Option<&RetainedOutputFacts>,
) -> bool {
    match capture {
        crate::vodserve::RetainedOutputCapture::Restore(expected) => expected.as_ref() == actual,
        crate::vodserve::RetainedOutputCapture::ReceiverUnavailable => actual.is_none(),
        crate::vodserve::RetainedOutputCapture::New => {
            candidate_expected.is_none_or(|expected| actual == Some(expected))
        }
    }
}

impl TranscodeManager {
    /// Retained output is authoritative; an initial claim is negotiated only
    /// after this producer has selected the encoded route and its AAC lattice.
    pub(super) fn encoded_start_audio_options(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        options: TranscodeOptions,
    ) -> Result<TranscodeOptions, String> {
        Self::encoded_audio_options(
            file,
            req.audio_index,
            req.audio_claim.as_ref(),
            req.audio_delivery.as_ref(),
            options,
        )
    }

    pub(super) fn encoded_audio_options(
        file: &plurx_core::domain::MediaFile,
        audio_index: Option<i64>,
        claim: Option<&plurx_core::playback::audio::AudioClaim>,
        retained: Option<&plurx_core::playback::audio::AudioDelivery>,
        mut options: TranscodeOptions,
    ) -> Result<TranscodeOptions, String> {
        if let Some(audio) = retained {
            options.set_audio_delivery(audio.clone());
        } else if let Some(claim) = claim {
            let selected = audio_index.map_or_else(
                || file.audio_streams.first(),
                |index| {
                    file.audio_streams
                        .iter()
                        .find(|stream| stream.index == index)
                },
            );
            options.set_audio_delivery(plurx_core::playback::audio::resolve_audio(
                selected,
                &claim.profile(),
                plurx_core::playback::audio::AudioRoute::EncodedVod,
                file.audio_offset_ms,
            ));
        }
        if options
            .audio
            .as_ref()
            .is_some_and(|audio| !audio.is_encoded_vod_compatible())
        {
            return Err(vod_refusal_error(
                "vod_audio_recipe_invalid",
                "encoded VOD requires its fixed AAC sample lattice",
            ));
        }
        Ok(options)
    }

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
        // A process-local start has no durable recovery identity.
        self.create_session_inner(
            req,
            user_name,
            &supersession_user,
            None,
            None,
            None,
            None,
            (Priority::Live, crate::vodserve::RetainedOutputCapture::New),
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
            (Priority::Live, crate::vodserve::RetainedOutputCapture::New),
        )
        .await
    }

    /// Start a provisional make-before-break worker only from spare capacity.
    pub(crate) async fn create_cluster_session_for_receiver(
        &self,
        request: (&SessionRequest, Option<u8>, Option<&RetainedOutputFacts>),
        recovery: &SessionRecoveryIdentity,
        user_name: &str,
        deadline: tokio::time::Instant,
        admitted_serving_generation: u64,
    ) -> Result<ClusterSessionStart, String> {
        let capture = if request.1 == Some(1) {
            request
                .2
                .cloned()
                .map_or(crate::vodserve::RetainedOutputCapture::New, |facts| {
                    crate::vodserve::RetainedOutputCapture::Restore(Some(facts))
                })
        } else {
            crate::vodserve::RetainedOutputCapture::ReceiverUnavailable
        };
        self.create_cluster_session_with_priority(
            request.0,
            recovery,
            user_name,
            deadline,
            admitted_serving_generation,
            (Priority::Live, capture),
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
            (
                Priority::Speculative,
                crate::vodserve::RetainedOutputCapture::New,
            ),
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
        priority: (Priority, crate::vodserve::RetainedOutputCapture),
    ) -> Result<ClusterSessionStart, String> {
        let serving_admission = ClusterServingAdmission {
            generation: admitted_serving_generation,
            deadline,
        };
        self.require_cluster_serving_authority(serving_admission)?;
        let supersession_user = recovery.supersession_scope();
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
                Some(recovery),
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
        principal: &plurx_core::playback_principal::PlaybackPrincipal,
        deadline: tokio::time::Instant,
    ) -> Result<ClusterReplacementGuard, String> {
        let supersession_user = principal_supersession_scope(principal);
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
        let supersession_user = recovery.supersession_scope();
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
            Some(recovery),
            Some(deadline),
            Some(takeover),
            None,
            (
                Priority::Live,
                crate::vodserve::RetainedOutputCapture::ReceiverUnavailable,
            ),
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
                            ));
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

    pub(crate) async fn validate_candidate_planning_binding(
        &self,
        request: &SessionRequest,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<(), String> {
        let Some(binding) = request
            .candidate_context
            .as_ref()
            .and_then(|context| context.planning_binding.as_ref())
        else {
            return Ok(());
        };
        let deadline = deadline
            .unwrap_or_else(|| crate::media_pool::create_stage_deadline(Duration::from_secs(2)));
        let snapshot = tokio::time::timeout_at(
            deadline,
            self.store
                .playback_planning_snapshot(request.file_id, &super::QUALITY_PLANNING_KEYS),
        )
        .await
        .map_err(|_| catalog_input_error("planning revalidation deadline"))?
        .map_err(|error| catalog_input_error(error.to_string()))?
        .ok_or_else(|| catalog_input_error("candidate source missing"))?;
        if *binding != crate::media_pool::PlanningBinding::from_snapshot(&snapshot) {
            return Err(catalog_input_error(
                "candidate source or settings changed before admission",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn create_session_inner(
        &self,
        req: &SessionRequest,
        user_name: &str,
        supersession_user: &str,
        recovery: Option<&SessionRecoveryIdentity>,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        serving_admission: Option<ClusterServingAdmission>,
        priority: (Priority, crate::vodserve::RetainedOutputCapture),
    ) -> Result<SessionCreation, String> {
        let (priority, retained_capture) = priority;
        if let Some(admission) = serving_admission {
            self.require_cluster_serving_authority(admission)?;
        }
        match (&req.previous_session_id, req.reopen_reason) {
            (None, None) | (Some(_), Some(_)) => {}
            _ => {
                return Err(invalid_reopen_error(
                    "previous_session_id and reopen_reason must be sent together",
                ));
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
                    if !recovered_retained_output_matches(
                        &retained_capture,
                        req.candidate_context
                            .as_ref()
                            .and_then(|context| context.retained_output.as_ref()),
                        info.retained_output.as_ref(),
                    ) {
                        return Err(vod_refusal_error("retained_artifact_unavailable", "the recovered presentation does not own the exact issued retained artifact"));
                    }
                    if let Some(admission) = serving_admission {
                        self.require_cluster_serving_authority(admission)?;
                    }
                    return Ok(SessionCreation {
                        info: *info,
                        created: false,
                    });
                }
                Claimed::Mine(claim, normalized) => Some((claim, *normalized)),
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

        self.validate_candidate_planning_binding(req, replacement_deadline)
            .await?;

        if req.passive_vod
            && (!req.vod_only
                || req.presentation != Presentation::Vod
                || req
                    .request_id
                    .as_deref()
                    .is_none_or(|id| id.trim().is_empty()))
        {
            return Err(vod_refusal_error(
                "vod_passive_policy_invalid",
                "passive retention requires VOD-only policy and request identity",
            ));
        }
        if req.vod_only && req.presentation != Presentation::Vod {
            return Err(vod_refusal_error(
                "vod_source_unsupported",
                "VOD-only service policy forbids rolling presentation",
            ));
        }
        let startup_create_at = Instant::now();
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
            let vod_lookup_at = Instant::now();
            // Read once here and handed down, so the fallback below decides
            // from the same snapshot the VOD attempt was admitted under.
            let vod_settings = self.vod_settings(req).await?;
            let live_recovery_enabled = vod_settings
                .as_ref()
                .is_some_and(|settings| settings.live_recovery);
            let vod = self
                .try_vod_session(
                    vod_settings,
                    req,
                    recovery
                        .map(|identity| {
                            identity.principal.local_user_id().ok_or_else(|| {
                                "sharing VOD requires typed viewer demand".to_owned()
                            })
                        })
                        .transpose()?,
                    user_name,
                    supersession_user,
                    replacement_deadline,
                    takeover.is_some(),
                    serving_admission,
                    retained_capture,
                )
                .await;
            tracing::info!(
                target: "plurxd::transcode",
                file_id = req.file_id,
                phase = "vod_create",
                elapsed_ms = vod_lookup_at.elapsed().as_millis() as u64,
                outcome = if vod.is_ok() { "ready" } else { "refused" },
                "playback startup phase completed"
            );
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
                        if !req.vod_only && live_recovery_enabled {
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
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&info.session_id),
            phase = "create",
            elapsed_ms = startup_create_at.elapsed().as_millis() as u64,
            "playback startup phase completed"
        );
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
        recovery: Option<&SessionRecoveryIdentity>,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        priority: Priority,
    ) -> Result<StartInfo, String> {
        let started = match req.kind {
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
                    req.candidate_context.as_deref(),
                    priority,
                    req.audio_claim.as_ref(),
                    req.audio_delivery.as_ref(),
                    req.sdr_master_codecs,
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
                    req.audio_delivery.as_ref(),
                    req.sdr_master_codecs,
                )
                .await
            }
        }?;
        let session = self.sessions.lock().await.get(&started.session_id).cloned();
        if let Some(session) = session {
            session
                .publication
                .lock()
                .await
                .bind_startup_transport(req.transport.as_deref());
        }
        Ok(started)
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

    /// Retain the accepted inputs or acquire one real bounded atomic snapshot.
    /// Revalidation can reject it, but cannot silently replace its choice.
    pub(super) async fn vod_preparation_snapshot(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Arc<plurx_core::store::PlaybackPlanningSnapshot>, String> {
        let context = req.candidate_context.as_ref();
        if context.is_some_and(|context| context.planning_binding.is_none()) {
            return Err(catalog_input_error(
                "selected candidate planning binding missing",
            ));
        }
        let snapshot = if let Some(snapshot) =
            context.and_then(|context| context.planning_snapshot.as_ref())
        {
            if context.and_then(|context| context.planning_binding.as_ref())
                != Some(&crate::media_pool::PlanningBinding::from_snapshot(snapshot))
            {
                return Err(catalog_input_error("accepted planning snapshot is unbound"));
            }
            Arc::clone(snapshot)
        } else {
            Arc::new(
                tokio::time::timeout_at(
                    crate::media_pool::create_stage_deadline(Duration::from_secs(2)),
                    self.store
                        .playback_planning_snapshot(req.file_id, &super::QUALITY_PLANNING_KEYS),
                )
                .await
                .map_err(|_| catalog_input_error("VOD planning snapshot deadline"))?
                .map_err(|error| catalog_input_error(error.to_string()))?
                .ok_or_else(|| catalog_input_error("VOD planning source missing"))?,
            )
        };
        if req.file_id != file.id
            || snapshot.file.id != file.id
            || snapshot.file.size != file.size
            || snapshot.file.mtime != file.mtime
            || !matches!((Self::plan_source_identity(file), Self::plan_source_identity(&snapshot.file)),
                (Ok(expected), Ok(actual)) if expected == actual)
        {
            return Err(catalog_input_error("VOD planning source changed"));
        }
        if context
            .and_then(|context| context.planning_binding.as_ref())
            .is_some_and(|binding| {
                *binding != crate::media_pool::PlanningBinding::from_snapshot(&snapshot)
            })
        {
            return Err(catalog_input_error("VOD planning binding changed"));
        }
        Ok(snapshot)
    }

    /// The same equality seam checked before the frozen Encoding is built.
    pub(super) fn validate_prepared_candidate_recipe(
        &self,
        plan: &ResolvedTranscode,
        presentation: super::Presentation,
        reorder_frames: bool,
        context: &CandidateExecutionContext,
    ) -> Result<[u8; 32], String> {
        let actual = self
            .candidate_recipe_digest(plan, presentation, reorder_frames)
            .map_err(|error| vod_refusal_error("candidate_recipe_unavailable", error))?;
        if actual != context.recipe_digest
            || plurx_core::playback::candidate::CandidateId::for_recipe_digest(actual)
                != context.candidate_id
        {
            return Err(vod_refusal_error(
                "candidate_recipe_changed",
                "the resolved source/route no longer matches the selected candidate",
            ));
        }
        Ok(actual)
    }

    /// Freeze an executable encoded recipe before any rendition is named.
    /// Copy remains index-driven; selecting burn pixels requires an encoder
    /// even when the incoming request otherwise asks for source quality.
    pub(super) async fn prepare_vod_encoding(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        Box::pin(self.prepare_vod_encoding_with_source(req, file, None)).await
    }

    pub(in crate::transcode) async fn prepare_source_vod_encoding(
        &self,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        evidence: &crate::transcode::source_preparation::SourceHeldProbeEvidence,
        proof: &plurx_core::sharing_source_sessions::SourceSessionWriteAuthority,
    ) -> Result<Arc<crate::vodencode::Encoding>, String> {
        // A burn's sidecar must be the one this Source operation extracted for
        // this exact track; nothing here reaches Local's shared burn cache.
        let burn_matches = match (prepared.request().subtitle_burn, evidence.burn()) {
            (None, None) => true,
            (Some(index), Some(burn)) => burn.subtitle_index() == index,
            _ => false,
        };
        if !prepared.matches_assignment(proof.assignment())
            || !burn_matches
            || !crate::transcode::source_actor::source_recipe_is_encoded(prepared.request())
        {
            return Err("Source encoding requires its exact prepared assignment and burn".into());
        }
        Box::pin(self.prepare_vod_encoding_with_source(
            prepared.request(),
            prepared.file(),
            Some((evidence, proof)),
        ))
        .await?
        .ok_or_else(|| "Source encoding recipe missing".to_owned())
    }

    async fn prepare_vod_encoding_with_source(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        source_evidence: Option<(
            &crate::transcode::source_preparation::SourceHeldProbeEvidence,
            &plurx_core::sharing_source_sessions::SourceSessionWriteAuthority,
        )>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        if req
            .continuous_media
            .as_ref()
            .is_some_and(|media| !media.valid_for(req))
        {
            return Err(vod_refusal_error(
                "vod_continuous_recipe_invalid",
                "the continuous media role is incompatible with this request",
            ));
        }
        if req.finite_bitrate_limit_bps.is_some_and(|limit| {
            !req.vod_only || !req.passive_vod || !(64_000..=1_000_000_000).contains(&limit)
        }) {
            return Err(vod_refusal_error(
                "vod_output_budget_refused",
                "invalid finite bitrate policy",
            ));
        }
        if matches!(req.kind, SessionKind::Copy { .. }) {
            match req.subtitle_burn {
                None => {
                    validate_finite_copy_rate(req, file)?;
                    return Ok(None);
                }
                // A copy whose burn track the store holds as having no cues
                // has nothing to burn: it stays the copy it would have been,
                // rather than a full re-encode to overlay nothing.
                // Local only: the Source burns what its own sidecar holds and
                // never reads Local's subtitle store to skip it.
                Some(index)
                    if source_evidence.is_none()
                        && self.burn_track_is_stored_empty(file, index).await =>
                {
                    tracing::info!(
                        target: "plurxd::transcode",
                        file_id = file.id,
                        subtitle_index = index,
                        "the burn track has no cues; serving the copy without an overlay"
                    );
                    validate_finite_copy_rate(req, file)?;
                    return Ok(None);
                }
                Some(_) => {}
            }
        }
        let note_phase = |phase: &'static str, started: std::time::Instant| {
            tracing::debug!(
                target: "plurxd::transcode",
                file_id = file.id,
                phase,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "encoded recipe preparation phase completed"
            );
        };
        let phase_started = std::time::Instant::now();
        let source = if source_evidence.is_some() {
            crate::fragment_index_cluster::open_source_playback_fence(file, None).await
        } else {
            crate::fragment_index_cluster::open_source_fence(file, None).await
        }
        .map_err(|error| {
            vod_refusal_error(
                "vod_source_rescan_required",
                format!("the source could not be held for encoded preparation: {error}"),
            )
        })?;
        if let Some((evidence, proof)) = source_evidence {
            if !evidence.matches(proof.assignment(), source.object_version()) {
                return Err(
                    "Source held-probe evidence differs from actual file/assignment".into(),
                );
            }
        }
        note_phase("source_fence", phase_started);
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
            SessionKind::Transcode { height } if height > 0 => {
                if req.candidate_context.is_some() {
                    height
                } else {
                    height.min(source_height)
                }
            }
            SessionKind::Copy { .. } => source_height,
            _ => {
                return Err(vod_refusal_error(
                    "vod_invalid_height",
                    "the requested height must be positive",
                ));
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
            // A Source recipe reads the filter listing its own operation
            // proved; Local keeps the process-wide preflight.
            let filters = match source_evidence.and_then(|(evidence, _)| evidence.burn()) {
                Some(artifacts) => {
                    if artifacts.bitmap() != burn.bitmap {
                        return Err(vod_refusal_error(
                            "vod_subtitle_track_missing",
                            "the Source burn sidecar differs from the selected track",
                        ));
                    }
                    artifacts.filters()
                }
                None if source_evidence.is_some() => {
                    return Err(vod_refusal_error(
                        "vod_subtitle_burn_unavailable",
                        "the Source preparation made no burn sidecar",
                    ));
                }
                None => crate::pipeprobe::burn_filters().await,
            };
            if let Some(reason) = filters.refusal(burn.bitmap) {
                return Err(unsupported_build_error(reason));
            }
        }
        let probe = if let Some((_, proof)) = source_evidence {
            self.store.source_index_probe_evidence(proof).await
        } else {
            self.store.get_file_probe_json(file.id).await
        }
        .map_err(|error| {
            start_infrastructure_error(format!("reading the stored source probe: {error}"))
        })?;
        let phase_started = std::time::Instant::now();
        let (held_probe, held_decode_facts) = if let Some((evidence, _)) = source_evidence {
            // Source's actual operation supplied this evidence; no Local cache
            // or stored-account probe can authorize the foreign descriptor.
            (evidence.document().to_owned(), None)
        } else {
            let held_plan_handle = source.handle.try_clone().map(Arc::new).map_err(|error| {
                vod_refusal_error(
                    "vod_source_rescan_required",
                    format!("the held source could not be retained for verification: {error}"),
                )
            })?;
            let collected = self.probe_vod_source_once(file, held_plan_handle).await?;
            match collected {
                Some(mut collected) => (std::mem::take(&mut collected.document), Some(collected)),
                // Platforms without a sealed probe keep their existing source
                // verification and stored-facts planning behavior.
                None => (
                    crate::ffmpeg::held_source_probe_json(&source.handle, VOD_START_HELD_PROBE)
                        .await
                        .map_err(|error| {
                            vod_refusal_error(
                                "vod_source_rescan_required",
                                format!(
                                "the held source could not be verified against its scan: {error}"
                            ),
                            )
                        })?,
                    None,
                ),
            }
        };
        note_phase("held_source_probe", phase_started);
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
        let mut grid = crate::vodencode::frame_grid(probe.as_deref()).ok_or_else(|| {
            vod_refusal_error(
                "vod_frame_cadence_unknown",
                "the source probe has no usable video cadence; rescan the file",
            )
        })?;
        let shared_audio_role = req
            .continuous_media
            .as_ref()
            .is_some_and(|media| media.role == ContinuousMediaRole::SharedAudio);
        let (mut encoder, mut grade) = if shared_audio_role {
            // This process maps no video. It needs neither a GPU permit nor a
            // video tone-map proof merely because its source contains HDR.
            (Encoder::Software, OutputGrade::Sdr)
        } else {
            self.encoder_and_grade_for(file, req.hdr10, target_height, subtitle_burn.is_some())
                .await?
        };
        // After the encoder and grade, deliberately. `encoder_and_grade_for`
        // can refuse this source outright (an unknown Dolby Vision profile, an
        // unproven Profile 5 renderer), and a refusal must not first start a
        // detached full-source extraction and answer "pending" while it runs.
        // A `Nothing` answer chooses the encoder and grade again without the
        // burn: the HTTP layer admits an HDR delivery whose burn track the
        // store holds as `empty`, and that session must keep its range.
        let source_sidecar = source_evidence.and_then(|(evidence, _)| evidence.burn());
        let (subtitle_burn, burn_file) = match subtitle_burn {
            // The Source's own sidecar, extracted by its owned preparation
            // from this same held object. Local's cache, its detached flight
            // and its stored-track shortcuts are never consulted for it.
            Some(burn) if source_evidence.is_some() => {
                let artifacts = source_sidecar.ok_or_else(|| {
                    vod_refusal_error(
                        "vod_subtitle_burn_unavailable",
                        "the Source preparation made no burn sidecar",
                    )
                })?;
                let handle = artifacts.sidecar().map_err(|error| {
                    start_infrastructure_error(format!(
                        "retaining the Source burn sidecar: {error}"
                    ))
                })?;
                (Some(burn), Some(handle))
            }
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
                    SESSION_START_CLASS,
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
        let software_threads = crate::vodencode::frozen_software_threads(
            &Workload::of(file, target_height),
            self.software_budget().await,
        );
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
        options = self.encoded_start_audio_options(req, file, options)?;
        if let Some(context) = req.candidate_context.as_ref() {
            options.normalized_geometry = context.normalized_geometry;
            if let Some(profile) = context.profile {
                options.auto_quality_rate_profile = Some(profile);
                options.video_bitrate_kbps = profile.video_bitrate_kbps();
                options.effective_rate_control = plurx_core::transcode::EffectiveRateControl::Vbr;
            }
        }
        // Catalog identity describes the muxed candidate. Verify that exact
        // plan before deriving the video-only or shared-AAC execution recipe.
        let phase_started = std::time::Instant::now();
        let catalog_plan = if let Some(context) = req
            .candidate_context
            .as_ref()
            .filter(|_| req.continuous_media.is_some())
        {
            let catalog_encoder = if shared_audio_role {
                self.encoder_and_grade_for(file, false, target_height, false)
                    .await?
                    .0
            } else {
                encoder
            };
            let mut catalog_options = self.live_lookup_options(
                self.rate_control_snapshot(),
                catalog_encoder,
                file,
                target_height,
                0.0,
                req.audio_index,
                None,
                Some(software_threads),
                OutputGrade::Sdr,
            );
            // The catalog row was resolved with the request's audio claim,
            // so its muxed recipe carries the same audio delivery.
            catalog_options = self.encoded_start_audio_options(req, file, catalog_options)?;
            catalog_options.normalized_geometry = context.normalized_geometry;
            if let Some(profile) = context.profile {
                catalog_options.auto_quality_rate_profile = Some(profile);
                catalog_options.video_bitrate_kbps = profile.video_bitrate_kbps();
                catalog_options.effective_rate_control =
                    plurx_core::transcode::EffectiveRateControl::Vbr;
            }
            Some(
                self.resolve_vod_movie_plan(
                    file,
                    &catalog_options,
                    catalog_encoder,
                    Arc::new(
                        source
                            .handle
                            .try_clone()
                            .map_err(|error| start_infrastructure_error(error.to_string()))?,
                    ),
                )
                .await?,
            )
        } else {
            None
        };
        note_phase("catalog_decoder_plan", phase_started);
        if let Some(media) = req.continuous_media.as_ref() {
            options.effective_rate_control = plurx_core::transcode::EffectiveRateControl::Vbr;
            if media.role == ContinuousMediaRole::SharedAudio {
                options.pipeline = Pipeline::Cpu;
                options.tone_map = ToneMap::None;
            }
            if media.role == ContinuousMediaRole::Video {
                options.video_sample_envelope =
                    plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50;
                options.normalized_geometry = true;
            }
        }
        constrain_finite_vod_rate(req, file, &mut options)?;
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
        let phase_started = std::time::Instant::now();
        let plan = if let Some(prepared) = held_decode_facts {
            self.resolve_vod_prepared_source(file, &options, encoder, held_plan_handle, prepared)
                .await?
        } else {
            self.resolve_vod_movie_plan(file, &options, encoder, held_plan_handle)
                .await?
        };
        note_phase("execution_decoder_plan", phase_started);
        // Encoding retains these execution options for publication and
        // diagnostics. Its graph must agree with the immutable plan.
        options.pipeline = plan.options().pipeline;
        if let Some(frame_rate) = plan
            .output_contract()
            .normalized_geometry()
            .and_then(|geometry| geometry.rate_profile.and(geometry.frame_rate))
        {
            grid = plurx_core::transcode::VodFrameGrid::new(
                frame_rate.numerator(),
                frame_rate.denominator(),
            )
            .ok_or_else(|| {
                vod_refusal_error(
                    "vod_frame_cadence_unknown",
                    "the normalized output cadence has no valid immutable grid",
                )
            })?;
        }
        // The VOD builder still derives its raster from the retained file.
        // Do not attach a held-facts experiment if these two rasters differ.
        let output = plan.output_contract();
        let same_raster = transcode::output_size(file, options.target_height)
            == output
                .effective_width()
                .zip(output.effective_height())
                .map(|(width, height)| (i64::from(width), i64::from(height)));
        let cadence = same_raster
            .then(|| transcode::Rational::new(grid.numerator, grid.denominator))
            .flatten();
        let plan = plan.with_sdr_avc_qualification(&self.caps, cadence, options.force_idr);
        let planning = self.vod_preparation_snapshot(req, file).await?;
        let reorder_frames = Self::vod_reorder_from_snapshot(&planning);
        if let Some(context) = req.candidate_context.as_ref() {
            // Continuous roles verify the muxed catalog plan; every other
            // candidate verifies the qualified execution plan itself.
            self.validate_prepared_candidate_recipe(
                catalog_plan.as_ref().unwrap_or(&plan),
                req.presentation,
                reorder_frames,
                context,
            )?;
        }
        let shared_audio = if req
            .continuous_media
            .as_ref()
            .is_some_and(|media| media.role == ContinuousMediaRole::SharedAudio)
        {
            Some(
                plurx_core::transcode::VodSharedAudioRecipe::from_plan(&plan).ok_or_else(|| {
                    vod_refusal_error(
                        "vod_continuous_audio_invalid",
                        "the resolved source has no bounded shared AAC recipe",
                    )
                })?,
            )
        } else {
            None
        };
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
        let phase_started = std::time::Instant::now();
        let engine = if let Some((evidence, _)) = source_evidence {
            evidence.engine()
        } else {
            crate::ffmpeg::EncodedEngine::capture(
                options
                    .subtitle_burn
                    .as_ref()
                    .is_some_and(|burn| !burn.bitmap)
                    .then_some(self.runtime_cache.as_path()),
            )
            .await
            .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?
        };
        note_phase("encoded_engine_capture", phase_started);
        if !source.unchanged() {
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                "the source changed while attesting the encoded engine; rescan it before playback",
            ));
        }
        let executable = crate::ffmpeg::EncodedExecutable::capture()
            .await
            .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
        if !executable.matches_macos_plan(&plan) {
            return Err(vod_refusal_error(
                "vod_engine_unattested",
                "the encoder differs from the frozen Mac processing plan; check compatibility again",
            ));
        }
        Ok(Some(Arc::new(crate::vodencode::Encoding {
            shared_audio,
            source_object_version,
            plan,
            resources,
            options,
            grid,
            reorder_frames,
            subtitle,
            subtitle_digest,
            ffmpeg_build: if let Some((evidence, _)) = source_evidence {
                evidence.build().into()
            } else {
                crate::ffmpeg::ffmpeg_build().await
            },
            executable,
            engine,
            admissions: self.admissions.clone(),
            store: Arc::clone(&self.store),
            nonpreemptive_trial: req.automatic && req.candidate_context.is_some(),
            // A video-only worker or shared soundtrack is not the muxed
            // catalog recipe whose production speed this proof measures.
            candidate_recipe: req
                .candidate_context
                .as_ref()
                .filter(|_| req.continuous_media.is_none())
                .map(|context| context.recipe_digest),
            production_proofs: Arc::clone(&self.candidate_production_proofs),
            active_production: std::sync::Mutex::new(
                crate::vodencode::ActiveProductionWindow::default(),
            ),
            speculative: std::sync::atomic::AtomicBool::new(false),
            queued: std::sync::Mutex::new(None),
            policy_retry: std::sync::atomic::AtomicBool::new(false),
            handoff_wait: std::sync::atomic::AtomicBool::new(false),
            last_refusal: std::sync::Mutex::new(None),
            handoff_claim: std::sync::Mutex::new(None),
            hooks: Box::new(crate::vodencode::NoopEncodingHooks),
        })))
    }

    async fn prepare_vod_companion(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Option<(SessionRequest, Arc<crate::vodencode::Encoding>)>, String> {
        let Some(media) = request
            .continuous_media
            .as_ref()
            .filter(|media| media.autonomous_companion.is_some())
        else {
            return Ok(None);
        };
        let context = media.companion_context.as_ref().ok_or_else(|| {
            vod_refusal_error(
                "vod_family_catalog_missing",
                "the autonomous companion has no restored worker catalog context",
            )
        })?;
        if media.autonomous_companion != Some(context.candidate.candidate_id) {
            return Err(vod_refusal_error(
                "vod_family_invalid",
                "the companion context does not match its requested catalog identity",
            ));
        }
        let mut companion = request.clone();
        let role = companion
            .continuous_media
            .as_mut()
            .expect("continuous video");
        role.autonomous_companion = None;
        role.companion_catalog = None;
        role.companion_context = None;
        role.family_descriptor = None;
        companion.kind = SessionKind::Transcode {
            height: context.height,
        };
        companion.candidate_context = Some(Box::new(context.candidate.clone()));
        let encoding = self
            .prepare_vod_encoding(&companion, file)
            .await?
            .ok_or_else(|| {
                vod_refusal_error("vod_family_invalid", "the companion has no video recipe")
            })?;
        Ok(Some((companion, encoding)))
    }

    async fn prepare_vod_soundtrack(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        video: Option<&Arc<crate::vodencode::Encoding>>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        if file.audio_streams.is_empty()
            || !request
                .continuous_media
                .as_ref()
                .is_some_and(|media| media.role == ContinuousMediaRole::Video)
        {
            return Ok(None);
        }
        let mut audio = request.clone();
        audio
            .continuous_media
            .as_mut()
            .expect("continuous video")
            .role = ContinuousMediaRole::SharedAudio;
        // Video has already validated its catalog context. AAC uses its own
        // CPU-only recipe and borrows only the selected video's end grid.
        audio.candidate_context = None;
        let role = audio
            .continuous_media
            .as_mut()
            .expect("shared soundtrack role");
        role.autonomous_companion = None;
        role.companion_catalog = None;
        role.companion_context = None;
        role.family_descriptor = None;
        let mut soundtrack = self.prepare_vod_encoding(&audio, file).await?;
        if let (Some(soundtrack), Some(video)) = (soundtrack.as_mut(), video) {
            Arc::get_mut(soundtrack)
                .expect("new soundtrack recipe")
                .grid = video.grid;
        }
        Ok(soundtrack)
    }

    /// Negotiation shares native encoded preparation and the exact copy
    /// prerequisite resolver, without allocating a media session or producer.
    pub(crate) async fn preview_compatibility_vod(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        if !req.vod_only
            || !req.passive_vod
            || req.presentation != Presentation::Vod
            || req
                .request_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
        {
            return Err(vod_refusal_error(
                "vod_passive_policy_invalid",
                "compatibility preview requires the server-owned passive VOD policy",
            ));
        }
        let Some(settings) = self.vod_settings(req).await? else {
            return Err(vod_refusal_error(
                "vod_disabled",
                "VOD session creation is disabled on this server",
            ));
        };
        let encoding = self.prepare_vod_encoding(req, file).await?;
        self.vod
            .preview_recipe(
                crate::vodserve::VodRecipeRequest {
                    measured_candidate: None,
                    retained_capture: crate::vodserve::RetainedOutputCapture::New,
                    request: req,
                    encoding,
                    // A passive compatibility preview is never a continuous
                    // family: it names no shared soundtrack or companion.
                    soundtrack: None,
                    companion: None,
                },
                file,
                &settings,
                None,
            )
            .await
    }

    /// Bind only the actual resolved route/digest, shared by ordinary creation
    /// and the first unpublished preparation. Catalogue numbers cannot mint it.
    async fn resolved_retained_candidate_binding(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        encoding: &Option<Arc<crate::vodencode::Encoding>>,
    ) -> Result<Option<crate::vodserve::RetainedCandidateBinding>, String> {
        let kind = encoding
            .as_ref()
            .map_or(req.kind, |encoding| SessionKind::Transcode {
                height: encoding.options.target_height,
            });
        Ok(
            if let Some(context) = req
                .candidate_context
                .as_ref()
                .filter(|_| req.continuous_media.is_none())
            {
                use plurx_core::playback::candidate::{CandidateId, CandidateRoute};
                let actual_grade = encoding.as_ref().map_or_else(
                    || super::manager_candidates::copy_candidate_grade(file),
                    |encoding| encoding.options.pipeline.output_grade(),
                );
                let (actual, route) = if let Some(encoding) = encoding {
                    (
                        self.candidate_recipe_digest(
                            &encoding.plan,
                            req.presentation,
                            encoding.reorder_frames,
                        )?,
                        CandidateRoute::Encode,
                    )
                } else if let SessionKind::Copy {
                    aac,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                } = req.kind
                {
                    let source =
                        crate::fragment_index_cluster::open_source_fence(file, None).await?;
                    let executable = crate::ffmpeg::EncodedExecutable::capture()
                        .await
                        .map_err(|error| error.to_string())?;
                    let engine = crate::ffmpeg::EncodedEngine::capture(None)
                        .await
                        .map_err(|error| error.to_string())?;
                    let raster = file
                        .width
                        .and_then(|value| u32::try_from(value).ok())
                        .zip(file.height.and_then(|value| u32::try_from(value).ok()))
                        .ok_or_else(|| {
                            vod_refusal_error(
                                "candidate_recipe_changed",
                                "copy geometry is unknown",
                            )
                        })?;
                    let actual = super::manager_candidates::copy_candidate_recipe_digest(
                        file,
                        req.audio_index,
                        file.audio_offset_ms,
                        req.subtitle_burn,
                        (aac, preserve_dolby_vision, convert_dolby_vision),
                        Some(source.object_version()),
                        Some(&executable.digest),
                        Some(&engine.digest),
                        raster,
                    );
                    if !source.unchanged() {
                        return Err(vod_refusal_error(
                            "candidate_recipe_changed",
                            "copy source changed during equality validation",
                        ));
                    }
                    (actual, CandidateRoute::Remux)
                } else {
                    return Err(vod_refusal_error(
                        "candidate_recipe_changed",
                        "no actual candidate route",
                    ));
                };
                if actual != context.recipe_digest
                    || CandidateId::for_recipe_digest(actual) != context.candidate_id
                    || actual_grade != context.grade
                {
                    return Err(vod_refusal_error(
                        "candidate_recipe_changed",
                        "actual execution differs from the accepted candidate",
                    ));
                }
                Some(crate::vodserve::RetainedCandidateBinding {
                    kind,
                    normalized_geometry: context.normalized_geometry,
                    profile: context.profile,
                    candidate_id: context.candidate_id,
                    recipe_digest: actual,
                    file_id: file.id,
                    audio_index: req.audio_index,
                    audio_offset_ms: file.audio_offset_ms,
                    subtitle_burn: req.subtitle_burn,
                    grade: context.grade,
                    route,
                })
            } else {
                None
            },
        )
    }

    /// The only public HLS presentation: immutable VOD or a typed refusal.
    #[allow(clippy::too_many_arguments)]
    async fn try_vod_session(
        &self,
        vod_settings: Option<crate::vodserve::VodSettings>,
        req: &SessionRequest,
        user_id: Option<i64>,
        user_name: &str,
        supersession_user: &str,
        replacement_deadline: Option<tokio::time::Instant>,
        is_takeover: bool,
        serving_admission: Option<ClusterServingAdmission>,
        retained_capture: crate::vodserve::RetainedOutputCapture,
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
        let Some(settings) = vod_settings else {
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
        let soundtrack = self
            .prepare_vod_soundtrack(req, &file, encoding.as_ref())
            .await?;
        let companion = self.prepare_vod_companion(req, &file).await?;
        // A continuous family role executes a video-only or shared-AAC split of
        // its muxed catalog candidate, already verified against that catalog
        // plan in `prepare_vod_encoding`. It is never one complete retained
        // candidate output: it binds no measured candidate and offers nothing
        // to the complete-output queue.
        let complete_candidate_output = req.continuous_media.is_none();
        let measured_candidate = self
            .resolved_retained_candidate_binding(req, &file, &encoding)
            .await?;
        // Complete-output queue publication is handed to the owned enqueue
        // worker once the session exists; it never runs on the start path.
        // Only when the Developer switch admits this kind: a play must not
        // queue a whole-title background job the operator never turned on.
        let output_enqueue = (complete_candidate_output
            && settings.output_preparation.admits(encoding.is_some())
            && matches!(
                &retained_capture,
                crate::vodserve::RetainedOutputCapture::New
            )
            && req
                .candidate_context
                .as_ref()
                .is_none_or(|context| context.retained_output.is_none()))
        .then(|| {
            (
                req.clone(),
                file.clone(),
                settings.clone(),
                encoding.clone(),
            )
        });
        let prepared = crate::vodserve::VodRecipeRequest {
            companion,
            measured_candidate,
            retained_capture: match retained_capture {
                crate::vodserve::RetainedOutputCapture::New => req
                    .candidate_context
                    .as_ref()
                    .and_then(|context| context.retained_output.clone())
                    .map_or(crate::vodserve::RetainedOutputCapture::New, |facts| {
                        crate::vodserve::RetainedOutputCapture::Restore(Some(facts))
                    }),
                existing => existing,
            },
            request: req,
            encoding,
            soundtrack,
        };
        // Cluster activation is make-before-break: the Store pointer CAS and
        // exact post-CAS terminal projection are the only operations allowed
        // to retire the authoritative predecessor. If provisional capacity is
        // unavailable, fail this replacement and leave the current player
        // intact. Legacy process-local callers retain their historical sweep.
        // A continuous parent is also make-before-break: group admission
        // can refuse after preparation, and must not end a healthy incumbent.
        // Its prepared/CAS owner or explicit client release retires that parent.
        if replacement_deadline.is_none() && req.continuous_media.is_none() {
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
        let local_viewer = crate::state::PlaybackViewerDemand {
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: user_id.ok_or_else(|| {
                    "Local VOD start requires its explicit viewer principal".to_owned()
                })?,
            },
            playback_id: req.playback_id.clone(),
        };
        let start = if let Some(admission) = serving_admission {
            self.vod
                .try_create_cluster_for_viewer(
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
                    local_viewer.clone(),
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
                .try_create_for_viewer(
                    prepared,
                    &file,
                    &settings,
                    attribution,
                    session_id,
                    local_viewer,
                )
                .await?
        };
        if let Some((encoder, grade, pipeline)) = codec_qualification {
            self.record_codec_qualification_session(encoder, grade, Some(pipeline));
        }
        // This snapshot follows the actual attachment; a candidate catalog or
        // a queued result cannot claim that this response owns retained bytes.
        let response_facts = self.vod.hls_facts(&start.session_id).await;
        if let Some((request, file, settings, encoding)) = output_enqueue {
            self.hand_off_output_enqueue(
                OutputEnqueue {
                    request,
                    file,
                    settings,
                    encoding,
                    session_id: start.session_id.clone(),
                    queued_at: Instant::now(),
                },
                response_facts.as_ref().map(|facts| &facts.response_owner),
            );
        }
        Ok(StartInfo {
            retained_output: response_facts
                .as_ref()
                .and_then(|facts| facts.response_owner.retained_output_facts()),
            audio_delivery: response_facts.and_then(|facts| facts.audio_delivery),
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
        // These values define one admission policy. Read them from one
        // snapshot rather than paying six serial linearizable Store reads on
        // every Play request (including a missing-index rolling fallback).
        let settings = self
            .store
            .get_settings(&[
                plurx_core::store::keys::VOD_PRESENTATION,
                plurx_core::store::keys::VOD_WORKING_SET_BYTES,
                plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS,
                plurx_core::store::keys::VOD_MATERIALIZE_BUDGET_SECS,
                plurx_core::store::keys::CACHE_MAX_GB,
                plurx_core::store::keys::VOD_BLOCKED_GET_CAP,
                // Not admission policy, but read on this same create path:
                // folding it in costs no extra Store read, and the session
                // freezes it beside its block budget (S-10 Developer switch).
                plurx_core::store::keys::PLAYBACK_SDR_MASTER_CODECS,
                // Read on every VOD create too, and once one statement at a
                // time (D4): the cluster index gate, the HEVC copy proof
                // switch, and the live-recovery fallback switch.
                plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE,
                plurx_core::store::keys::HEVC_UNVERIFIED_COPY,
                plurx_core::store::keys::VOD_LIVE_RECOVERY,
                plurx_core::store::keys::VOD_OUTPUT_PREPARATION,
            ])
            .await
            .map_err(|error| {
                start_infrastructure_error(format!("reading VOD serving settings: {error}"))
            })?;
        let read = |key: &str| settings.get(key);
        if read(plurx_core::store::keys::VOD_PRESENTATION).is_some_and(|value| value.trim() == "0")
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
        let working_set_bytes = match read(plurx_core::store::keys::VOD_WORKING_SET_BYTES) {
            Some(raw) => match raw.trim().parse::<u64>() {
                // The settings surface refuses a zero on the way in; one that
                // arrived by another route is still not a budget this can run
                // with, and "not configured" is the honest reading.
                Ok(0) | Err(_) => DEFAULT_WORKING_SET_BYTES,
                Ok(bytes) => bytes,
            },
            None => DEFAULT_WORKING_SET_BYTES,
        };
        let server_cap = match read(plurx_core::store::keys::VOD_BLOCK_BUDGET_SECS) {
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
        let materialize_secs = match read(plurx_core::store::keys::VOD_MATERIALIZE_BUDGET_SECS) {
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
        let completed_cache_bytes = match read(plurx_core::store::keys::CACHE_MAX_GB) {
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
        let blocked_get_cap = match read(plurx_core::store::keys::VOD_BLOCKED_GET_CAP) {
            Some(raw) => raw
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|cap| *cap > 0)
                .map(|cap| cap.min(MAX_BLOCKED_GET_CAP))
                .unwrap_or(DEFAULT_BLOCKED_GET_CAP),
            None => DEFAULT_BLOCKED_GET_CAP,
        };
        let switch = |key: &str, absent: bool| {
            plurx_core::store::stored_switch(read(key).map(String::as_str), absent)
        };
        Ok(Some(crate::vodserve::VodSettings {
            working_set_bytes,
            completed_cache_bytes,
            block_budget: Duration::from_secs_f64(block_secs),
            materialize_budget: Duration::from_secs_f64(materialize_secs),
            blocked_get_cap,
            // The create's own snapshot read when it carried one, so VOD and
            // rolling freeze the same value; else this batch's. Missing is
            // off: the pre-S-10 master with no SDR CODECS.
            sdr_master_codecs: req.sdr_master_codecs.unwrap_or_else(|| {
                plurx_core::store::stored_switch(
                    read(plurx_core::store::keys::PLAYBACK_SDR_MASTER_CODECS).map(String::as_str),
                    false,
                )
            }),
            index_cluster_cache: switch(plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE, false),
            hevc_unverified_copy: switch(plurx_core::store::keys::HEVC_UNVERIFIED_COPY, false),
            live_recovery: switch(plurx_core::store::keys::VOD_LIVE_RECOVERY, true),
            output_preparation: crate::vodserve::OutputPreparation::parse(
                read(plurx_core::store::keys::VOD_OUTPUT_PREPARATION).map(String::as_str),
            ),
            output_budget_bytes: crate::cachekeep::cache_budget(
                read(plurx_core::store::keys::CACHE_MAX_GB).map(String::as_str),
            )
            .unwrap_or(0),
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

    pub(crate) async fn vod_quality_schedule_before(
        &self,
        session_id: &str,
        owner_node_id: &str,
        request: &crate::vodserve::QualityScheduleRequest,
        deadline: Instant,
    ) -> Result<crate::vodserve::QualityScheduleResponse, String> {
        self.vod
            .quality_schedule_before(session_id, owner_node_id, request, deadline)
            .await
    }

    pub(crate) async fn vod_continuous_family_description_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodResponsePublication<Option<Vec<u8>>>> {
        self.vod
            .continuous_family_description_before(session_id, deadline)
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    pub(crate) async fn vod_continuous_master_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodResponsePublication<Option<Vec<u8>>>> {
        self.vod
            .continuous_master_before(session_id, deadline)
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    pub(crate) async fn vod_child_playlist_before(
        &self,
        session_id: &str,
        role: &str,
        rendition_id: &str,
        deadline: Instant,
    ) -> Option<VodResponsePublication<Option<Vec<u8>>>> {
        self.vod
            .child_playlist_before(session_id, role, rendition_id, deadline)
            .await
            .map(|publication| VodResponsePublication {
                result: publication.result,
                owner: MediaResponseOwner(MediaResponseOwnerKind::Vod(publication.owner)),
            })
    }

    pub(crate) async fn vod_child_segment_before(
        &self,
        session_id: &str,
        request: &crate::vodserve::ChildMediaRequest,
        deadline: Instant,
    ) -> Option<VodResponsePublication<Option<crate::vodserve::SegmentReady>>> {
        self.vod
            .child_segment_before(session_id, request, deadline)
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

    /// A peer's 204 is not capture evidence. Re-read the sealed reservation
    /// before the actor can offer it; a mixed-version peer leaving pending
    /// unchanged is refused and the existing reservation guard owns cleanup.
    pub(crate) async fn prepared_prime_is_ready(
        &self,
        preparation: &plurx_core::domain::MediaSessionPreparation,
        primed: bool,
    ) -> bool {
        if !primed {
            return false;
        }
        let Ok(response) = serde_json::from_str::<serde_json::Value>(&preparation.response_json)
        else {
            return false;
        };
        if response["prepared_output_capture_pending"] != true {
            return true;
        }
        let Ok(Some(route)) = self
            .store
            .media_session_route_by_incarnation(&preparation.incarnation_id)
            .await
        else {
            return false;
        };
        if !plurx_core::store::PreparedOutputSeal::matches_reservation(preparation, &route) {
            return false;
        }
        let Ok(remote) =
            serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(&route.recipe_json)
        else {
            return false;
        };
        let Some(user_id) = route.principal.local_user_id() else {
            return false;
        };
        self.store
            .seal_prepared_output(&plurx_core::store::PreparedOutputSeal {
                incarnation_id: route.incarnation_id,
                session_id: route.session_id,
                user_id,
                playback_id: route.playback_id,
                owner_node_id: route.owner_node_id,
                owner_epoch: route.owner_epoch,
                expected_recipe_json: route.recipe_json,
                expected_response_json: route.response_json,
                predecessor_incarnation_id: preparation.expected_predecessor_incarnation_id.clone(),
                predecessor_owner_node_id: preparation.expected_predecessor_owner_node_id.clone(),
                predecessor_owner_epoch: preparation.expected_predecessor_owner_epoch,
                deadline_ms: preparation.deadline_ms,
                now_ms: crate::media_sessions::unix_ms(),
                already_complete: true,
                retained_output: remote
                    .retained_output
                    .map(|proof| serde_json::to_value(proof).expect("private facts serialize")),
            })
            .await
            .is_ok_and(|sealed| sealed)
    }

    /// Prime a genuinely new unpublished reservation and seal its private output
    /// choice before admitting a client. Missing pending markers are strict recovery.
    pub(crate) fn vod_prepare_first_before<'a>(
        &'a self,
        recipe_json: &'a str,
        session_id: &'a str,
        user_id: i64,
        owner_node_id: &'a str,
        adoption: SessionAdoptionToken,
        deadline: Instant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let gate = Arc::clone(&adoption.gate);
            let work = async {
                let _first = Arc::clone(&gate.first_preparation).lock_owned().await;
                if gate.released.load(Acquire) {
                    return false;
                }
                let Ok(initial) =
                    serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(recipe_json)
                else {
                    return false;
                };
                let Ok(Some(route)) = self
                    .store
                    .media_session_route_by_incarnation(&initial.incarnation_id)
                    .await
                else {
                    return false;
                };
                let now = crate::media_sessions::unix_ms();
                if route.session_id != session_id
                    || route.principal.local_user_id() != Some(user_id)
                    || route.owner_node_id != owner_node_id
                    // prepare_media_session mints epoch one; takeover is recovery,
                    // never a first-capture reservation under this private entry.
                    || route.owner_epoch != 1
                    || route.state != "active"
                    || route.lease_expires_at_ms <= now
                    || route.publication_ready_at_ms
                        != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
                {
                    return false;
                }
                let Ok(response) = serde_json::from_str::<serde_json::Value>(&route.response_json)
                else {
                    return false;
                };
                let Ok(Some(ledger)) = self
                    .store
                    .staged_media_session_for_playback(&route.principal, &route.playback_id)
                    .await
                else {
                    return false;
                };
                if ledger.staged_incarnation_id != route.incarnation_id
                    || ledger.deadline_ms != route.lease_expires_at_ms
                {
                    return false;
                }
                let Ok(Some(predecessor)) = self
                    .store
                    .media_session_route_by_incarnation(&ledger.expected_predecessor_incarnation_id)
                    .await
                else {
                    return false;
                };
                if adoption.session_id != session_id || gate.released.load(Acquire) {
                    return false;
                }
                let Ok(current_recipe) = serde_json::from_str::<
                    crate::media_sessions::RemoteStartRequest,
                >(&route.recipe_json) else {
                    return false;
                };
                if initial
                    .retained_output
                    .as_ref()
                    .is_some_and(|issued| current_recipe.retained_output.as_ref() != Some(issued))
                {
                    return false;
                }
                let Ok(mut incoming) = serde_json::to_value(&initial) else {
                    return false;
                };
                let Ok(mut current) = serde_json::to_value(&current_recipe) else {
                    return false;
                };
                incoming
                    .as_object_mut()
                    .expect("remote object")
                    .remove("retained_output");
                current
                    .as_object_mut()
                    .expect("remote object")
                    .remove("retained_output");
                if incoming != current {
                    return false;
                }
                let pending = response["prepared_output_capture_pending"] == true;
                let complete = response["prepared_output_capture_complete"] == true;
                if !pending && !complete {
                    return self
                        .resurrect_vod_from_recipe_before(
                            &route.recipe_json,
                            session_id,
                            user_id,
                            adoption,
                            deadline,
                            VodRecipeAttachment::Recovery { speculative: true },
                        )
                        .await;
                }
                if let Some(facts) = self.vod.hls_facts(session_id).await {
                    if !self
                        .vod
                        .prepared_owner_matches(
                            session_id,
                            &facts.response_owner,
                            &route.incarnation_id,
                        )
                        .await
                    {
                        return false;
                    }
                }
                // A retry after an interrupted first attempt seals the existing actual
                // attachment, including None; it never upgrades/replaces that choice.
                if self.vod.hls_facts(session_id).await.is_none()
                    && !self
                        .resurrect_vod_from_recipe_before(
                            &route.recipe_json,
                            session_id,
                            user_id,
                            adoption,
                            deadline,
                            if pending {
                                VodRecipeAttachment::FirstPreparation
                            } else {
                                VodRecipeAttachment::Recovery { speculative: true }
                            },
                        )
                        .await
                {
                    return false;
                }
                let Some(facts) = self.vod.hls_facts(session_id).await else {
                    return false;
                };
                let mut cleanup = FirstPreparationCleanup {
                    vod: self.vod.clone(),
                    session_id: session_id.to_owned(),
                    owner: pending.then(|| facts.response_owner.clone()),
                };
                let Some(_owner_guard) = self
                    .vod
                    .lock_preparation_owner(
                        session_id,
                        &facts.response_owner,
                        &route.incarnation_id,
                    )
                    .await
                else {
                    return false;
                };
                let output = facts.response_owner.retained_output_facts();
                let seal = plurx_core::store::PreparedOutputSeal {
                    incarnation_id: route.incarnation_id,
                    session_id: session_id.to_owned(),
                    user_id,
                    playback_id: route.playback_id,
                    owner_node_id: route.owner_node_id,
                    owner_epoch: route.owner_epoch,
                    expected_recipe_json: route.recipe_json,
                    expected_response_json: route.response_json,
                    predecessor_incarnation_id: ledger.expected_predecessor_incarnation_id,
                    predecessor_owner_node_id: predecessor.owner_node_id,
                    predecessor_owner_epoch: predecessor.owner_epoch,
                    deadline_ms: route.lease_expires_at_ms,
                    now_ms: crate::media_sessions::unix_ms(),
                    already_complete: complete,
                    retained_output: output.map(|output| {
                        serde_json::to_value(output).expect("output facts serialize")
                    }),
                };
                if gate.released.load(Acquire)
                    || !self
                        .store
                        .seal_prepared_output(&seal)
                        .await
                        .is_ok_and(|sealed| sealed)
                    || gate.released.load(Acquire)
                {
                    return false;
                }
                cleanup.owner = None;
                true
            };
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work)
                .await
                .unwrap_or(false)
        })
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
    /// Resurrect a VOD session from its durable recipe. The future holds a
    /// complete VOD create, so it is constructed outside the caller's polling
    /// frame; built inline, unoptimized builds lay it out in every caller's
    /// stack frame beneath the HTTP segment path.
    #[inline(never)]
    pub(crate) fn vod_resurrect_before<'a>(
        &'a self,
        recipe_json: &'a str,
        session_id: &'a str,
        user_id: i64,
        adoption: SessionAdoptionToken,
        deadline: Instant,
        speculative: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(self.resurrect_vod_from_recipe_before(
            recipe_json,
            session_id,
            user_id,
            adoption,
            deadline,
            VodRecipeAttachment::Recovery { speculative },
        ))
    }

    async fn resurrect_vod_from_recipe_before(
        &self,
        recipe_json: &str,
        session_id: &str,
        user_id: i64,
        adoption: SessionAdoptionToken,
        deadline: Instant,
        purpose: VodRecipeAttachment,
    ) -> bool {
        let (speculative, first_capture) = match purpose {
            VodRecipeAttachment::Recovery { speculative } => (speculative, false),
            VodRecipeAttachment::FirstPreparation => (true, true),
        };
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
            let Ok(mut remote) =
                serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(recipe_json)
            else {
                return false;
            };
            if self
                .restore_candidate_context_with_deadline(
                    &mut remote,
                    tokio::time::Instant::from_std(deadline),
                )
                .await
                .is_err()
            {
                return false;
            }
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
            let Ok(soundtrack) = self
                .prepare_vod_soundtrack(&req, &file, encoding.as_ref())
                .await
            else {
                return false;
            };
            let Ok(companion) = self.prepare_vod_companion(&req, &file).await else {
                return false;
            };
            let measured_candidate = if first_capture {
                match self
                    .resolved_retained_candidate_binding(&req, &file, &encoding)
                    .await
                {
                    Ok(binding) => binding,
                    Err(_) => return false,
                }
            } else {
                None
            };
            if speculative {
                if let Some((_, encoding)) = companion.as_ref() {
                    encoding.mark_speculative();
                }
                if let Some(soundtrack) = soundtrack.as_ref() {
                    soundtrack.mark_speculative();
                }
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
                        companion,
                        measured_candidate,
                        retained_capture: if first_capture {
                            crate::vodserve::RetainedOutputCapture::New
                        } else {
                            crate::vodserve::RetainedOutputCapture::Restore(
                                remote.retained_output.clone(),
                            )
                        },
                        request: &req,
                        encoding,
                        soundtrack,
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

    /// Hand a started session's complete-output queue publication to the
    /// owned worker. Never waits: a full hand-off is reported and skipped,
    /// and the title's next start offers the same deduplicated job again.
    fn hand_off_output_enqueue(
        &self,
        work: OutputEnqueue,
        response_owner: Option<&crate::vodserve::ResponseOwner>,
    ) {
        if response_owner
            .and_then(|owner| owner.retained_output_facts())
            .is_some()
        {
            return;
        }
        let file_id = work.file.id;
        if let Err(error) = self.output_enqueue.sender.try_send(work) {
            let reason = match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => "queue_full",
                tokio::sync::mpsc::error::TrySendError::Closed(_) => "worker_stopped",
            };
            crate::telemetry::record_output_enqueue_drop(reason);
            tracing::info!(
                target: "plurxd::transcode",
                file_id,
                reason,
                "complete output preparation not handed off; the next start offers it again"
            );
        }
    }

    /// The single owner of post-start complete-output queue publication,
    /// spawned beside [`Self::vod_maintain_loop`]. Each publication keeps its
    /// own stage budget (the catalog restore's create-stage deadline and the
    /// store's own write bound) instead of borrowing the viewer's start
    /// budget, and every outcome is logged against its file and session.
    ///
    /// It stops on the daemon's `shutdown` token, both while idle and while a
    /// publication is in flight: an interrupted publication is the same
    /// deduplicated job the title's next start offers again, so dropping it
    /// at shutdown loses nothing durable.
    pub async fn output_enqueue_loop(
        self: Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        let Some(mut receiver) = self
            .output_enqueue
            .receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        loop {
            let work = tokio::select! {
                () = shutdown.cancelled() => break,
                work = receiver.recv() => match work {
                    Some(work) => work,
                    None => break,
                },
            };
            let started = Instant::now();
            let publication = async {
                match &work.encoding {
                    Some(encoding) => (
                        "encoded_output",
                        self.enqueue_encoded_output(
                            &work.request,
                            &work.file,
                            &work.settings,
                            encoding,
                        )
                        .await,
                    ),
                    None => (
                        "copy_output",
                        self.enqueue_copy_output(&work.request, &work.file, &work.settings)
                            .await,
                    ),
                }
            };
            let (kind, result) = tokio::select! {
                () = shutdown.cancelled() => {
                    tracing::info!(
                        target: "plurxd::transcode",
                        file_id = work.file.id,
                        session_id = %work.session_id,
                        "complete output preparation interrupted by shutdown"
                    );
                    break;
                }
                outcome = publication => outcome,
            };
            let waited_ms = started.duration_since(work.queued_at).as_millis() as u64;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(()) => tracing::debug!(
                    target: "plurxd::transcode",
                    file_id = work.file.id,
                    session_id = %work.session_id,
                    kind,
                    waited_ms,
                    elapsed_ms,
                    "complete output preparation queued"
                ),
                Err(error) => tracing::info!(
                    target: "plurxd::transcode",
                    file_id = work.file.id,
                    session_id = %work.session_id,
                    kind,
                    waited_ms,
                    elapsed_ms,
                    %error,
                    "complete output preparation not queued"
                ),
            }
        }
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
                    return Ok(Claimed::Mine(claim, Box::new(normalized)));
                }
                Step::Recover(session_id, persisted_target) => {
                    if let Some(info) = self.recover(&session_id).await {
                        if persisted_target.is_some_and(|height| height != info.target_height) {
                            return Err(format!(
                                "request {key} resolved to a target that differs from its session"
                            ));
                        }
                        tracing::debug!(target: "plurxd::transcode", session = %session_log_id(&session_id), request_id = key, "idempotent create: same session");
                        return Ok(Claimed::Recovered(Box::new(info)));
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
        let Some(reason) = request.reopen_reason else {
            return Err(invalid_reopen_error("unsupported reopen reason"));
        };
        // A typed cause (link/encode/decode/hold/authority) is advisory
        // evidence recorded at HTTP ingress, never a precondition for media.
        // Its reopen keeps the exact target the client asked for; a missing
        // full candidate or an incumbent this node no longer runs leaves the
        // cause unrecorded rather than refusing a stalled viewer.
        let typed = reason != ReopenReason::Stall;
        let requested_height = match request.kind {
            SessionKind::Transcode { height } => Some(height),
            SessionKind::Copy { .. } => None,
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
            let Some(previous) = sessions.get(previous_session_id) else {
                if typed {
                    // Remote, retired or already-replaced incumbent: the
                    // typed reopen proceeds as an ordinary start.
                    return Ok((request.clone(), requested_height));
                }
                return Err(invalid_reopen_error(
                    "the previous session is no longer running",
                ));
            };
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

        if typed || request.candidate_context.is_some() {
            return Ok((request.clone(), requested_height));
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

/// Both plain copies and copies with a known empty burn track share the same
/// source-rate proof. The empty-track optimization cannot bypass the ceiling.
pub(super) fn validate_finite_copy_rate(
    request: &SessionRequest,
    file: &plurx_core::domain::MediaFile,
) -> Result<(), String> {
    if request.finite_bitrate_limit_bps.is_some_and(|limit| {
        let extra_audio = if matches!(request.kind, SessionKind::Copy { aac: true, .. }) {
            320_000
        } else {
            0
        };
        file.bitrate.is_none_or(|bitrate| {
            bitrate <= 0
                || bitrate
                    .checked_add(extra_audio)
                    .is_none_or(|total| total > i64::from(limit))
        })
    }) {
        return Err(vod_refusal_error(
            "vod_output_budget_refused",
            "copy delivery cannot meet the finite bitrate ceiling",
        ));
    }
    Ok(())
}

/// Apply a service-owned ceiling before the native semantic recipe is frozen.
/// VBR's existing 1.5x video peak and the native audio rate share this budget.
pub(super) fn constrain_finite_vod_rate(
    request: &SessionRequest,
    file: &plurx_core::domain::MediaFile,
    options: &mut plurx_core::transcode::TranscodeOptions,
) -> Result<(), String> {
    let Some(limit) = request.finite_bitrate_limit_bps else {
        return Ok(());
    };
    if !request.vod_only
        || !request.passive_vod
        || !(64_000..=1_000_000_000).contains(&limit)
        || request.candidate_context.is_some()
    {
        return Err(vod_refusal_error(
            "vod_output_budget_refused",
            "invalid finite bitrate policy",
        ));
    }
    let audio_bps = if file.audio_streams.is_empty() {
        0
    } else {
        u64::from(options.audio_bitrate_kbps) * 1000
    };
    let video_kbps = u64::from(limit)
        .checked_sub(audio_bps)
        .map(|remaining| remaining / 1500)
        .filter(|rate| *rate >= 32)
        .ok_or_else(|| {
            vod_refusal_error(
                "vod_output_budget_refused",
                "the ceiling cannot contain the native audio and a valid video rate",
            )
        })?;
    options.video_bitrate_kbps = options.video_bitrate_kbps.min(video_kbps as u32);
    options.effective_rate_control = plurx_core::transcode::EffectiveRateControl::Vbr;
    Ok(())
}

#[cfg(test)]
mod retained_recovery_tests {
    use super::*;

    #[tokio::test]
    async fn cached_output_attachment_skips_preparation_but_cold_candidate_enqueues() {
        use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
        let (_temp, _serve, facts, _) =
            crate::vodserve::retained::test_post_attachment_output_facts().await;
        assert!(facts.response_owner.retained_output_facts().is_some());
        let work_root = crate::test_tempdir().expect("enqueue work root");
        let manager = TranscodeManager::new(
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store")),
            work_root.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let mut receiver = manager
            .output_enqueue
            .receiver
            .lock()
            .expect("queue owner")
            .take()
            .expect("actual bounded enqueue receiver");
        let digest = [42; 32];
        let candidate = QualityCandidate {
            id: CandidateId::for_recipe_digest(digest),
            recipe_digest: digest,
            route: CandidateRoute::Remux,
            normalized_geometry: false,
            width: 1280,
            height: 720,
            target_height: 720,
            average_bps: None,
            peak_bps: None,
            grade: OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: true,
            sustainable: true,
        };
        let request = SessionRequest {
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: Some(Box::new(TranscodeManager::candidate_context(&candidate))),
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            control_sequence: None,
            file_id: facts.file.id,
            playback_id: "enqueue-fixture".into(),
            request_id: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: None,
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        let make_work = |session_id: &str| OutputEnqueue {
            request: request.clone(),
            file: facts.file.clone(),
            settings: crate::vodserve::VodSettings {
                working_set_bytes: 8 << 30,
                completed_cache_bytes: 4 << 30,
                block_budget: Duration::from_secs(30),
                materialize_budget: Duration::from_secs(30),
                blocked_get_cap: 100,
                sdr_master_codecs: false,
                index_cluster_cache: false,
                hevc_unverified_copy: false,
                live_recovery: true,
                output_preparation: crate::vodserve::OutputPreparation::CopyAndEncoded,
                output_budget_bytes: 4 << 30,
            },
            encoding: None,
            session_id: session_id.into(),
            queued_at: Instant::now(),
        };
        manager.hand_off_output_enqueue(make_work("cached"), Some(&facts.response_owner));
        assert!(
            matches!(
                receiver.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "a verified actual cached response publishes no duplicate preparation"
        );
        // The same eligible candidate without actual retained attachment still
        // reaches the real owned worker hand-off, regardless of catalog hints.
        manager.hand_off_output_enqueue(make_work("cold"), None);
        let cold = receiver
            .try_recv()
            .expect("cold candidate offered to worker");
        assert_eq!(cold.session_id, "cold");
        assert_eq!(
            cold.request
                .candidate_context
                .as_ref()
                .expect("candidate")
                .candidate_id,
            candidate.id
        );
        assert_eq!(cold.file.id, facts.file.id);
        assert!(cold.settings.output_preparation.admits(false));
        manager.hand_off_output_enqueue(make_work("rolling-no-facts"), None);
        assert_eq!(
            receiver
                .try_recv()
                .expect("no-artifact fallback still offered")
                .session_id,
            "rolling-no-facts"
        );
    }

    #[tokio::test]
    async fn first_preparation_seals_actual_choice_and_replay_keeps_owner() {
        use plurx_core::store::{MediaSessionStore, UserStore};
        for absence in [false, true] {
            let (_temp, serve, _, actual_request) =
                crate::vodserve::retained::test_post_attachment_output_facts().await;
            let id = "00000000-0000-4000-8000-00000000cace";
            if absence {
                crate::vodserve::retained::test_make_attachment_uncaptured(&serve, id).await;
            }
            let initial_facts = serve.hls_facts(id).await.expect("actual current choice");
            let store =
                Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("capture store"));
            let user = store
                .create_user("capture", "hash", false)
                .await
                .expect("user");
            let principal =
                plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: user.id };
            let now = crate::media_sessions::unix_ms();
            let predecessor = uuid::Uuid::new_v4().to_string();
            let activation = plurx_core::domain::MediaSessionActivation {
                incarnation_id: predecessor.clone(),
                session_id: uuid::Uuid::new_v4().to_string(),
                principal: principal.clone(),
                playback_id: "first-capture".into(),
                recovery_epoch: "capture-epoch".into(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: true,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "node".into(),
                recipe_json: "{}".into(),
                response_json: "{}".into(),
                publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: 0,
                now_ms: now,
                lease_expires_at_ms: now + 60_000,
                expected_desired_revision: None,
            };
            assert!(store
                .activate_media_session(&activation)
                .await
                .expect("predecessor")
                .is_some());
            let incarnation = uuid::Uuid::new_v4().to_string();
            let fixture = crate::media_sessions::takeover_eligible_route(id, &incarnation);
            let mut recipe: crate::media_sessions::RemoteStartRequest =
                serde_json::from_str(&fixture.recipe_json).expect("typed recipe");
            recipe.principal = principal.clone();
            recipe.retained_output_receiver = Some(1);
            recipe.retained_output = None;
            recipe.request = actual_request;
            recipe.request.request_id = Some(incarnation.clone());
            assert_eq!(recipe.request.start_seconds, 70.841);
            serve.mark_prepared_incarnation(id, &incarnation).await;
            let recipe_json = serde_json::to_string(&recipe).expect("recipe");
            let preparation = plurx_core::domain::MediaSessionPreparation {
                quality_cancellation_key: None,
                incarnation_id: incarnation.clone(),
                session_id: id.into(),
                principal: principal.clone(),
                playback_id: "first-capture".into(),
                expected_predecessor_incarnation_id: predecessor.clone(),
                expected_predecessor_owner_node_id: "node".into(),
                expected_predecessor_owner_epoch: 1,
                request_fingerprint: "b".repeat(64),
                owner_node_id: "node".into(),
                recipe_json: recipe_json.clone(),
                response_json: r#"{"prepared_output_capture_pending":true}"#.into(),
                media_origin_ms: 0,
                now_ms: now,
                expected_desired_revision: None,
                deadline_ms: now + 30_000,
            };
            assert!(store
                .prepare_media_session(&preparation)
                .await
                .expect("reserve")
                .is_some());
            let work = crate::test_tempdir().expect("capture work");
            let mut manager = TranscodeManager::new(
                store.clone(),
                work.path().to_path_buf(),
                EncoderCaps::default(),
                Pipeline::Cpu,
            );
            manager.vod = Arc::clone(&serve);
            let manager = Arc::new(manager);
            let mut remote_state =
                crate::http::internal_media_sessions::output_capture_test_state();
            remote_state.store = store.clone();
            remote_state.transcode = Arc::clone(&manager);
            remote_state.node_id = "node".into();
            let deadline = Instant::now() + Duration::from_secs(5);
            // An old peer can return 204 without changing this actual reserved
            // row. That reply must never expose the pending successor.
            let stub_peer_reply = axum::http::StatusCode::NO_CONTENT;
            assert!(
                !manager
                    .prepared_prime_is_ready(
                        &preparation,
                        stub_peer_reply == axum::http::StatusCode::NO_CONTENT
                    )
                    .await,
                "204 with durable pending capture is refused"
            );
            let prepare_request = crate::media_sessions::RemotePrepareRequest {
                protocol_version: crate::media_pool::PROTOCOL_VERSION,
                incarnation_id: incarnation.clone(),
                session_id: id.into(),
                principal: principal.clone(),
                expected_owner_epoch: 1,
            };
            let response = crate::http::internal_media_sessions::prepare_authorized(
                remote_state,
                axum::body::Bytes::from(
                    serde_json::to_vec(&prepare_request).expect("remote preparation"),
                ),
            )
            .await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::NO_CONTENT,
                "remote success waits for actual durable capture"
            );
            assert!(
                manager.prepared_prime_is_ready(&preparation, true).await,
                "local and remote activation wait for the exact durable seal"
            );
            assert!(!manager.prepared_prime_is_ready(&preparation, false).await);
            let sealed = store
                .media_session_route_by_incarnation(&incarnation)
                .await
                .expect("read")
                .expect("sealed");
            let durable: crate::media_sessions::RemoteStartRequest =
                serde_json::from_str(&sealed.recipe_json).expect("sealed typed recipe");
            assert_eq!(
                durable.retained_output,
                initial_facts.response_owner.retained_output_facts()
            );
            assert_eq!(
                durable.retained_output.is_none(),
                absence,
                "actual private choice, never advisory promotion"
            );
            assert!(
                manager
                    .vod_prepare_first_before(
                        &recipe_json,
                        id,
                        user.id,
                        "node",
                        manager.session_adoption_token(id).expect("repeat adoption"),
                        deadline
                    )
                    .await
            );
            assert!(
                serve
                    .response_status_owner_is_current(id, &initial_facts.response_owner)
                    .await,
                "repeat leaves actual private attachment unchanged"
            );
            assert_eq!(
                store
                    .media_session_route_by_incarnation(&incarnation)
                    .await
                    .expect("read")
                    .expect("same"),
                sealed
            );
            let mut changed_request = recipe.clone();
            changed_request.request.start_seconds += 1.0;
            let changed_recipe = serde_json::to_string(&changed_request).expect("changed request");
            assert!(
                !manager
                    .vod_prepare_first_before(
                        &changed_recipe,
                        id,
                        user.id,
                        "node",
                        manager
                            .session_adoption_token(id)
                            .expect("changed request adoption"),
                        deadline
                    )
                    .await,
                "same incarnation cannot replay a different nonretained recipe"
            );
            let mut changed_receipt = durable.clone();
            changed_receipt.retained_output = Some(RetainedOutputFacts {
                artifact_id: uuid::Uuid::new_v4().to_string(),
                output_identity: "ab".repeat(32),
                average_bps: 8000,
                peak_bps: 12000,
            });
            let changed_receipt = serde_json::to_string(&changed_receipt).expect("changed receipt");
            assert!(
                !manager
                    .vod_prepare_first_before(
                        &changed_receipt,
                        id,
                        user.id,
                        "node",
                        manager
                            .session_adoption_token(id)
                            .expect("changed receipt adoption"),
                        deadline
                    )
                    .await
            );
            // This is the same owner wrapper invoked by local and authenticated remote
            // PREPARE. A foreign owner cannot return a successful preparation receipt.
            assert!(
                !manager
                    .vod_prepare_first_before(
                        &recipe_json,
                        id,
                        user.id,
                        "foreign",
                        manager
                            .session_adoption_token(id)
                            .expect("foreign adoption"),
                        deadline
                    )
                    .await
            );
            assert!(
                serve
                    .response_status_owner_is_current(id, &initial_facts.response_owner)
                    .await
            );
            if let Some(expected) = durable.retained_output.as_ref() {
                crate::vodserve::retained::test_assert_sealed_cached_delivery(&serve, id, expected)
                    .await;
            }
            let committed = store
                .commit_media_session_preparation(
                    &principal,
                    "first-capture",
                    &plurx_core::domain::MediaSessionPreparationCommitRequest {
                        staged_incarnation_id: incarnation.clone(),
                        expected_predecessor_owner_node_id: "node".into(),
                        expected_predecessor_owner_epoch: 1,
                        now_ms: crate::media_sessions::unix_ms(),
                        lease_expires_at_ms: now + 60_000,
                        control_receipt: None,
                        expected_desired_revision: None,
                    },
                )
                .await
                .expect("commit sealed preparation")
                .expect("committed");
            assert_eq!(
                committed.route.recipe_json, sealed.recipe_json,
                "handoff keeps captured recipe"
            );
            if let Some(expected) = durable.retained_output.as_ref() {
                crate::vodserve::retained::test_assert_sealed_cached_delivery(&serve, id, expected)
                    .await;
            }
            assert!(
                !manager
                    .vod_prepare_first_before(
                        &recipe_json,
                        id,
                        user.id,
                        "node",
                        manager
                            .session_adoption_token(id)
                            .expect("published adoption"),
                        deadline
                    )
                    .await,
                "published route cannot perform first capture"
            );
            assert!(
                serve
                    .response_status_owner_is_current(id, &initial_facts.response_owner)
                    .await,
                "refused published repeat cannot kill serving owner"
            );
            serve.end_for_owner(id, &initial_facts.response_owner).await;
        }
    }

    #[test]
    fn idempotent_recovery_preserves_captured_absence_and_exact_artifact_identity() {
        use crate::vodserve::RetainedOutputCapture;
        let original = RetainedOutputFacts {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            output_identity: "ab".repeat(32),
            average_bps: 8000,
            peak_bps: 12000,
        };
        let mut other = original.clone();
        other.artifact_id = uuid::Uuid::new_v4().to_string();
        assert!(recovered_retained_output_matches(
            &RetainedOutputCapture::Restore(None),
            None,
            None
        ));
        assert!(!recovered_retained_output_matches(
            &RetainedOutputCapture::Restore(None),
            None,
            Some(&original)
        ));
        assert!(!recovered_retained_output_matches(
            &RetainedOutputCapture::Restore(Some(original.clone())),
            None,
            None
        ));
        assert!(recovered_retained_output_matches(
            &RetainedOutputCapture::Restore(Some(original.clone())),
            None,
            Some(&original)
        ));
        assert!(!recovered_retained_output_matches(
            &RetainedOutputCapture::Restore(Some(original.clone())),
            None,
            Some(&other)
        ));
        assert!(!recovered_retained_output_matches(
            &RetainedOutputCapture::ReceiverUnavailable,
            None,
            Some(&original)
        ));
        assert!(
            recovered_retained_output_matches(&RetainedOutputCapture::New, None, Some(&original)),
            "new capable replay reads the incumbent without changing it"
        );
        assert!(
            !recovered_retained_output_matches(&RetainedOutputCapture::New, Some(&original), None),
            "a measured selection cannot silently downgrade to old captured None"
        );
        assert!(!recovered_retained_output_matches(
            &RetainedOutputCapture::New,
            Some(&original),
            Some(&other)
        ));
        assert!(recovered_retained_output_matches(
            &RetainedOutputCapture::New,
            Some(&original),
            Some(&original)
        ));
    }
}
