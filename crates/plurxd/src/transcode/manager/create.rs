use crate::vodserve::StagedProcessedPredecessor;

enum VodRecipeAttachment {
    Recovery {
        speculative: bool,
        bootstrap: bool,
    },
    FirstPreparation {
        owner: PreparedVodOwner,
        bootstrap: bool,
        processed_predecessor: Option<StagedProcessedPredecessor>,
        attachment_owner: Arc<std::sync::Mutex<Option<crate::vodserve::ResponseOwner>>>,
    },
}

/// Resource handoff belongs to a bounded transition from an unbound manual
/// incumbent, never to the fact that a cached encoded recipe was automatic.
fn prepared_manual_auto_bootstrap(
    predecessor: &crate::media_sessions::RemoteStartRequest,
    successor: &crate::media_sessions::RemoteStartRequest,
) -> bool {
    let crate::transcode::SessionKind::Transcode {
        height: incumbent_height,
    } = predecessor.request.kind
    else {
        return false;
    };
    let crate::transcode::SessionKind::Transcode { height } = successor.request.kind else {
        return false;
    };
    !predecessor.request.automatic
        && predecessor.candidate_id.is_none()
        && predecessor.decoder_caps.is_some()
        && successor.request.automatic
        && predecessor.request.file_id == successor.request.file_id
        && predecessor.request.playback_id == successor.request.playback_id
        && predecessor.principal == successor.principal
        && predecessor.request.presentation == crate::transcode::Presentation::Vod
        && successor.request.presentation == crate::transcode::Presentation::Vod
        && height > 0
        && height <= incumbent_height
        && successor.candidate_catalog.as_ref().is_some_and(|catalog| {
            catalog.candidate.identity_matches()
                && catalog.candidate.route
                    == plurx_core::playback::candidate::CandidateRoute::Encode
                && i64::from(catalog.candidate.height) == height
                && successor.candidate_id == Some(catalog.candidate.id)
        })
}

/// A completed cache is not a purpose grant. Only an authorized canonical
/// Auto downshift, or the existing bounded manual-to-Auto bootstrap, can keep
/// its reader while the incumbent waits. Same-rung trials and upgrades cannot.
fn prepared_cached_recovery_recipe_matches(
    predecessor: &crate::media_sessions::RemoteStartRequest,
    successor: &crate::media_sessions::RemoteStartRequest,
) -> bool {
    let SessionKind::Transcode { height: old_height } = predecessor.request.kind else {
        return false;
    };
    let SessionKind::Transcode { height } = successor.request.kind else {
        return false;
    };
    let Some(catalog) = successor.candidate_catalog.as_ref() else {
        return false;
    };
    let downward = predecessor.request.automatic
        && height > 0
        && height < old_height
        && predecessor
            .candidate_catalog
            .as_ref()
            .is_some_and(|previous| {
                previous.candidate.identity_matches()
                    && predecessor.candidate_id == Some(previous.candidate.id)
                    && i64::from(previous.candidate.height) == old_height
            });
    (downward || prepared_manual_auto_bootstrap(predecessor, successor))
        && predecessor.principal == successor.principal
        && predecessor.request.file_id == successor.request.file_id
        && predecessor.request.playback_id == successor.request.playback_id
        && predecessor.request.presentation == Presentation::Vod
        && successor.request.presentation == Presentation::Vod
        && successor.request.automatic
        && predecessor.decoder_caps.is_some()
        && predecessor.decoder_caps == successor.decoder_caps
        && crate::playback_control::GradeIntent::from_request(&predecessor.request)
            == crate::playback_control::GradeIntent::from_request(&successor.request)
        && catalog.candidate.identity_matches()
        && catalog.candidate.decoder_compatible
        && catalog.candidate.route == plurx_core::playback::candidate::CandidateRoute::Encode
        && i64::from(catalog.candidate.height) == height
        && successor.candidate_id == Some(catalog.candidate.id)
}

/// Cancellation after actual capture still retires only that exact private owner.
struct FirstPreparationCleanup {
    vod: Arc<crate::vodserve::VodServe>,
    session_id: String,
    owner: Arc<std::sync::Mutex<Option<crate::vodserve::ResponseOwner>>>,
}
impl Drop for FirstPreparationCleanup {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.lock().expect("first preparation owner").take() {
            let vod = self.vod.clone();
            let session_id = self.session_id.clone();
            tokio::spawn(async move {
                vod.end_for_owner(&session_id, &owner).await;
                #[cfg(all(test, target_os = "linux"))]
                TranscodeManager::processed_preparation_test_boundary(
                    &session_id,
                    "cleanup-complete",
                )
                .await;
            });
        }
    }
}

use super::*;

/// Bounded identity-only facts for ordering private seal and caller admission.
/// These observations never participate in capture or publication authority.
fn trace_prepared_capture(
    seal: &plurx_core::store::PreparedOutputSeal,
    owner_current: Option<bool>,
    ready: bool,
) {
    if !tracing::enabled!(target: "plurxd::transcode", tracing::Level::DEBUG) {
        return;
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    static START: OnceLock<Instant> = OnceLock::new();
    let sequence = SEQUENCE
        .try_update(Relaxed, Relaxed, |value| Some(value.saturating_add(1)))
        .expect("saturating sequence always updates");
    let monotonic_us = START
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros()
        .min(u64::MAX as u128) as u64;
    let digest = |value: &[u8]| format!("{:x}", Sha256::digest(value));
    let mut recipe: serde_json::Value =
        serde_json::from_str(&seal.expected_recipe_json).expect("validated private recipe");
    recipe
        .as_object_mut()
        .expect("validated private object")
        .insert(
            "retained_output".into(),
            seal.retained_output
                .clone()
                .unwrap_or(serde_json::Value::Null),
        );
    let proof = seal.retained_output.as_ref();
    let facts = serde_json::json!({
        "sequence": sequence,
        "monotonic_us": monotonic_us,
        "session_sha256": digest(seal.session_id.as_bytes()),
        "incarnation_sha256": digest(seal.incarnation_id.as_bytes()),
        "owner_sha256": digest(seal.owner_node_id.as_bytes()),
        "predecessor_sha256": digest(seal.predecessor_incarnation_id.as_bytes()),
        "recipe_canonical_sha256": digest(&serde_json::to_vec(&recipe).expect("private JSON serializes")),
        "file_binding_sha256": digest(&serde_json::to_vec(&recipe["request"]["file_id"]).expect("private JSON serializes")),
        "artifact_sha256": proof.and_then(|proof| proof["artifact_id"].as_str()).map(|id| digest(id.as_bytes())),
        "output_identity_sha256": proof.and_then(|proof| proof["output_identity"].as_str()).map(|id| digest(id.as_bytes())),
        "owner_epoch": seal.owner_epoch,
        "deadline_ms": seal.deadline_ms,
        "capture_first": !seal.already_complete,
        "retained_some": proof.is_some(),
        "sealed_complete": ready,
        "owner_checked": owner_current.is_some(),
        "owner_current": owner_current.unwrap_or(false),
        "ready": ready,
    });
    if owner_current.is_some() {
        tracing::debug!(target: "plurxd::transcode", capture_facts = %facts,
            "Prepared retained output capture sealed");
    } else {
        tracing::debug!(target: "plurxd::transcode", capture_facts = %facts,
            "Prepared retained output caller ready");
    }
}

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

    /// The dispatch revalidation: re-read the planning inputs and refuse a
    /// candidate whose source or settings changed since it was accepted.
    /// Returns that fresh read (for a candidate-route request; `None` when
    /// there is no binding to check) so the VOD attempt that follows in the
    /// same task can take its source row and HWACCEL preference from it
    /// instead of reading them again.
    pub(crate) async fn validate_candidate_planning_binding(
        &self,
        request: &SessionRequest,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Option<plurx_core::store::PlaybackPlanningSnapshot>, String> {
        let Some(binding) = request
            .candidate_context
            .as_ref()
            .and_then(|context| context.planning_binding.as_ref())
        else {
            return Ok(None);
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
        Ok(Some(snapshot))
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
        req.dv_processing_route()
            .map_err(|error| vod_refusal_error("vod_processed_contract_invalid", error))?;
        // A durable/remote destination is descriptive, not a local lease.
        if req.dv_processing_destination.is_some()
            && !req
                .candidate_context
                .as_ref()
                .is_some_and(|context| context.selected_candidate.processed_dv)
        {
            return Err(vod_refusal_error(
                "candidate_recipe_unavailable",
                "processed destination requires fresh admitted owner qualification",
            ));
        }
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
                        return Err(vod_refusal_error(
                            "retained_artifact_unavailable",
                            "the recovered presentation does not own the exact issued retained artifact",
                        ));
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

        let revalidated = self
            .validate_candidate_planning_binding(req, replacement_deadline)
            .await?;
        crate::media_pool::mark_create_phase("worker");

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
            let live_recovery_enabled = match vod_settings.as_ref() {
                Some(settings) => settings.live_recovery,
                // VOD is switched off, so its settings snapshot is absent and
                // the rolling engine is the only presentation left. It still
                // answers to the operator's live-recovery switch.
                None => self.live_hls_recovery_enabled().await?,
            };
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
                    revalidated.as_ref(),
                    recovery,
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
            crate::media_pool::mark_create_phase("vod");
            match vod {
                Ok(info) => info,
                Err(error) => {
                    // One list, not two. `VOD_REFUSAL_CODES` (through
                    // `from_refusal`) decides both whether a code may fall
                    // back and which reason it is counted under, so a new
                    // code cannot produce sessions the engine serves and
                    // nothing attributes — nor fail playback unclassified.
                    let refusal_detail = vod_refusal(&error).map(|(_, detail)| detail.to_owned());
                    let live_recovery_reason = vod_refusal(&error)
                        .and_then(|(code, _)| LiveRecoveryReason::from_refusal(code));
                    if let Some(reason) = live_recovery_reason {
                        if !req.vod_only && live_recovery_enabled {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                file_id = req.file_id,
                                refusal = reason.label(),
                                detail = refusal_detail.as_deref().unwrap_or_default(),
                                "VOD prerequisite unavailable; using temporary live-HLS recovery"
                            );
                            self.hooks.get().before_vod_fallback_start().await;
                            // The fallback is a second execution of this
                            // request, a whole VOD attempt after the dispatch
                            // revalidation. A candidate-route rolling start
                            // reuses the snapshot its plan was accepted from
                            // (`rolling_start_inputs`), so the source and
                            // settings are revalidated again here: a catalog
                            // never authorizes stale execution. A legacy
                            // route carries no binding and its rolling start
                            // reads the source itself.
                            self.validate_candidate_planning_binding(req, replacement_deadline)
                                .await?;
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
                            crate::media_pool::mark_create_phase("rolling");
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
        // The rolling engine has no processing graph: a processed candidate
        // can only be served by the prepared VOD root it was qualified with.
        // An Auto enhancement degrades to the ordinary request it enhanced,
        // with the reason; a client-named processed candidate is refused
        // (`rolling_start_inputs` refuses it again, whatever the caller).
        let degraded_request;
        let (req, degraded_processing) = match req
            .candidate_context
            .as_ref()
            .filter(|context| context.selected_candidate.processed_dv)
        {
            Some(context) => match context.ordinary_request.as_deref() {
                Some(ordinary) if ordinary.file_id == req.file_id => {
                    let reason = "the rolling engine cannot serve a processed candidate";
                    tracing::warn!(
                        target: "plurxd::transcode",
                        file_id = req.file_id,
                        reason,
                        "processed candidate degraded for live-HLS recovery"
                    );
                    // The same dispatch, under the same incarnation.
                    degraded_request = SessionRequest {
                        request_id: req.request_id.clone(),
                        ..ordinary.clone()
                    };
                    (&degraded_request, Some(reason.to_owned()))
                }
                _ => {
                    return Err(vod_refusal_error(
                        "candidate_recipe_unavailable",
                        "a processed candidate cannot be served by the rolling engine",
                    ));
                }
            },
            None => (req, None),
        };
        let mut started = match req.kind {
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
                dual_layer_as_copied,
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
                        dual_layer_as_copied,
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
        started.degraded_processing = degraded_processing;
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
        if context.selected_candidate.processed_dv {
            return Err(vod_refusal_error(
                "candidate_recipe_changed",
                "processed HDR10 requires its admitted executable, not ordinary plan equality",
            ));
        }
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

    pub(crate) fn vod_dv_preferences_from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> plurx_core::transcode::dv_processing::DvPreferences {
        use plurx_core::store::stored_switch;
        plurx_core::transcode::dv_processing::DvPreferences {
            hdr_processing: stored_switch(
                snapshot
                    .settings
                    .get(keys::DV_HDR_PROCESSING)
                    .map(String::as_str),
                false,
            ),
            fel_reencode: stored_switch(
                snapshot
                    .settings
                    .get(keys::DV_FEL_REENCODE)
                    .map(String::as_str),
                false,
            ),
            conversion_permitted: stored_switch(
                snapshot.settings.get(keys::DV_CONVERT).map(String::as_str),
                true,
            ),
            planning_input_generation: snapshot.generation,
        }
    }

    pub(crate) fn vod_dv_processing_from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        request: &SessionRequest,
    ) -> Result<plurx_core::transcode::dv_processing::DvSelection, String> {
        use plurx_core::transcode::dv_processing::{
            dv_processing_preflight, DvFallbackReason, DvSelection,
        };
        let (existing, destination, allowed) = request.dv_processing_route()?;
        let reason = dv_processing_preflight(
            existing,
            allowed && request.subtitle_burn.is_none() && request.continuous_media.is_none(),
            destination,
            Self::vod_dv_preferences_from_snapshot(snapshot),
            &snapshot.file.dolby_vision,
        )
        .map_err(|error| vod_refusal_error("vod_dv_planning_invalid", error.to_string()))?
        // Preflight passed and the settings allow processing: preparation
        // runs the first window and selects processing from its result.
        .unwrap_or(DvFallbackReason::AwaitingFirstWindow);
        Ok(DvSelection::KeepExisting(reason))
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn display_target_policy(
        enabled: bool,
        display: Option<&plurx_core::playback::caps::DisplayCaps>,
        probe: Option<&str>,
    ) -> Result<
        (
            Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
            Option<String>,
        ),
        String,
    > {
        Self::display_target_policy_with_source(enabled, display, probe, None, None)
    }

    #[cfg(any(target_os = "linux", test))]
    fn display_target_policy_with_source(
        enabled: bool,
        display: Option<&plurx_core::playback::caps::DisplayCaps>,
        probe: Option<&str>,
        held_mastering: Option<plurx_core::transcode::dv_processing::DvSourceMastering>,
        held_minimum: Option<u32>,
    ) -> Result<
        (
            Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
            Option<String>,
        ),
        String,
    > {
        use plurx_core::transcode::dv_processing::{DvDisplayTarget, DvDisplayTargetChoice};
        if !enabled {
            return Ok((None, None));
        }
        if display.is_some_and(|display| !display.hdr && !display.dolby_vision) {
            return match DvDisplayTarget::for_sdr_title(
                probe.and_then(plurx_core::scan::dv_title::DvTitleStats::from_probe_json),
            )
            .map_err(|e| e.to_string())?
            {
                DvDisplayTargetChoice::Target(target) => {
                    let minimum = if target.needs_source_floor() {
                        let document: serde_json::Value =
                            serde_json::from_str(probe.ok_or("complete-title probe unavailable")?)
                                .map_err(|error| error.to_string())?;
                        let selected = crate::decode_facts::first_playable_video_index(&document);
                        document
                            .get("streams")
                            .and_then(serde_json::Value::as_array)
                            .and_then(|streams| {
                                streams.iter().find(|stream| {
                                    selected.is_some_and(|index| {
                                        stream.get("index").and_then(serde_json::Value::as_u64)
                                            == Some(u64::from(index))
                                    })
                                })
                            })
                            .map(plurx_core::transcode::dv_processing::source_mastering_minimum)
                            .transpose()?
                            .flatten()
                            .or(held_minimum)
                    } else {
                        None
                    };
                    let target = target
                        .resolve_sdr_floor(minimum)
                        .map_err(|error| error.to_string())?;
                    Ok((Some(target), Some("SDR: per-shot Dolby display mapping for 100-nit BT.709 gamma 2.4 limited output; rendered live, with the title's creative trims where its RPU carries them".into())))
                }
                DvDisplayTargetChoice::Master { reason } => Ok((
                    None,
                    Some(
                        reason
                            .reason()
                            .replace("master grade sent", "ordinary SDR tone mapping used"),
                    ),
                )),
            };
        }
        let Some(bucket) = display.and_then(|display| display.hdr10_target_bucket()) else {
            return Ok((None, Some("your display did not report a supported HDR peak (400–4999 nits), so the master grade is sent".into())));
        };
        match DvDisplayTarget::for_title(
            bucket,
            probe.and_then(plurx_core::scan::dv_title::DvTitleStats::from_probe_json),
        )
        .map_err(|e| e.to_string())?
        {
            DvDisplayTargetChoice::Target(target) => {
                // Resolve the effective source floor BEFORE the private context,
                // prepared lease and rendition identity freeze this policy.
                let source = if target.needs_source_mastering() {
                    let document: serde_json::Value =
                        serde_json::from_str(probe.ok_or("complete-title probe unavailable")?)
                            .map_err(|error| error.to_string())?;
                    let selected = crate::decode_facts::first_playable_video_index(&document);
                    let stream = document
                        .get("streams")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|streams| {
                            streams.iter().find(|stream| {
                                selected.is_some_and(|index| {
                                    stream.get("index").and_then(serde_json::Value::as_u64)
                                        == Some(u64::from(index))
                                })
                            })
                        });
                    stream
                        .map(plurx_core::transcode::dv_processing::source_mastering_metadata)
                        .transpose()?
                        .flatten()
                        .or(held_mastering)
                } else {
                    None
                };
                let target = target
                    .resolve_source(source)
                    .map_err(|error| error.to_string())?;
                Ok((Some(target), Some(format!("HDR10-E: per-shot Dolby display mapping for {bucket} nits; rendered live with the title's creative trims where its RPU carries them, and no retained bucket cache"))))
            }
            DvDisplayTargetChoice::Master { reason } => Ok((None, Some(reason.reason()))),
        }
    }

    #[cfg(target_os = "linux")]
    async fn display_target_policy_for_source(
        &self,
        enabled: bool,
        display: Option<&plurx_core::playback::caps::DisplayCaps>,
        file: &plurx_core::domain::MediaFile,
        probe: Option<&str>,
    ) -> Result<
        (
            Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
            Option<String>,
        ),
        String,
    > {
        use plurx_core::transcode::dv_processing::{DvDisplayTarget, DvDisplayTargetChoice};
        let sdr = display.is_some_and(|display| !display.hdr && !display.dolby_vision);
        let proposal = if !enabled {
            None
        } else if sdr {
            Some(
                DvDisplayTarget::for_sdr_title(
                    probe.and_then(plurx_core::scan::dv_title::DvTitleStats::from_probe_json),
                )
                .map_err(|error| error.to_string())?,
            )
        } else {
            display
                .and_then(|display| display.hdr10_target_bucket())
                .map(|bucket| {
                    DvDisplayTarget::for_title(
                        bucket,
                        probe.and_then(plurx_core::scan::dv_title::DvTitleStats::from_probe_json),
                    )
                    .map_err(|error| error.to_string())
                })
                .transpose()?
        };
        let needs_source = match proposal {
            Some(DvDisplayTargetChoice::Target(target)) if target.is_sdr() => {
                target.needs_source_floor()
            }
            Some(DvDisplayTargetChoice::Target(target)) => target.needs_source_mastering(),
            _ => false,
        };
        if !needs_source {
            return Self::display_target_policy(enabled, display, probe);
        }
        let stored = probe.ok_or("complete-title probe unavailable")?;
        let document: serde_json::Value =
            serde_json::from_str(stored).map_err(|error| error.to_string())?;
        let selected = crate::decode_facts::first_playable_video_index(&document);
        let stream = document
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .and_then(|streams| {
                streams.iter().find(|stream| {
                    selected.is_some_and(|index| {
                        stream.get("index").and_then(serde_json::Value::as_u64)
                            == Some(u64::from(index))
                    })
                })
            });
        if sdr {
            let minimum = stream
                .map(plurx_core::transcode::dv_processing::source_mastering_minimum)
                .transpose()?
                .flatten();
            if minimum.is_some() {
                return Self::display_target_policy(enabled, display, probe);
            }
            let minimum = self.held_minimum_for_policy(file, stored).await?;
            return Self::display_target_policy_with_source(enabled, display, probe, None, minimum);
        }
        let mastering = stream
            .map(plurx_core::transcode::dv_processing::source_mastering_metadata)
            .transpose()?
            .flatten();
        if mastering.is_some() {
            return Self::display_target_policy(enabled, display, probe);
        }
        let mastering = self.held_mastering_for_policy(file, stored).await?;
        Self::display_target_policy_with_source(enabled, display, probe, mastering, None)
    }

    /// Freeze an executable encoded recipe before any rendition is named.
    /// Copy remains index-driven; selecting burn pixels requires an encoder
    /// even when the incoming request otherwise asks for source quality.
    pub(super) async fn prepare_vod_encoding(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        self.prepare_vod_encoding_with_preference(req, file, None)
            .await
    }

    /// Whether this node can serve and measure the reconstructed 8.1 with the
    /// route's qualified recipe: radeonsi VA-API with its Main10 graph proved.
    /// The recipe and the per-title verdicts are measured on that encoder only.
    #[cfg(target_os = "linux")]
    pub(crate) fn fel_qualification_capable(&self) -> bool {
        self.caps.vaapi
            && self.hdr10_passthrough_vaapi
            && self.caps.vaapi_hdr_policy
                == Some(plurx_core::transcode::VaapiHdrPolicy::MesaRadeonsiVbaq)
    }

    /// The title's FEL qualification verdict for this renderer and recipe:
    /// `Ok` admits the reconstruction; every other answer is the reason the
    /// play keeps the plain conversion. A missing verdict asks for the title
    /// to be measured.
    #[cfg(target_os = "linux")]
    async fn reconstruction_qualified(
        &self,
        planning: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> Result<(), String> {
        if !self.fel_qualification_capable() {
            return Err(Self::fel_recipe_unqualified().into());
        }
        let tools = crate::vodencode::dv_runtime::ToolSet::capture()
            .await?
            .ok_or("the processing helper is unavailable on this node")?;
        let identity = self.fel_identity(&tools.backend_digest()).await?;
        self.fel_route_decision(
            &planning.file,
            planning.probe_json.as_deref(),
            &tools.title_scanner_digest(),
            &identity,
        )
        .await
    }

    /// The route's answer for one title from its stored verdict for
    /// `identity`: served, declined with the verdict's reason, or (no
    /// verdict) measured: the title's qualification is requested and the play
    /// keeps the plain conversion meanwhile.
    #[cfg(target_os = "linux")]
    pub(crate) async fn fel_route_decision(
        &self,
        file: &plurx_core::domain::MediaFile,
        probe_json: Option<&str>,
        scanner: &str,
        identity: &str,
    ) -> Result<(), String> {
        use plurx_core::transcode::dv_fel_qualification as q;
        let state = q::verdict_state(probe_json, file.size, file.mtime, identity);
        match Self::fel_gate(true, state) {
            FelGate::Serve => Ok(()),
            FelGate::Decline(reason) => Err(reason),
            FelGate::Measure => {
                self.request_fel_qualification(file.id, scanner, identity)
                    .await;
                Err(Self::fel_being_measured().into())
            }
        }
    }

    /// The encoder a FEL verdict is measured on: the FFmpeg executable's
    /// digest, the VA driver's description from the startup probe (vendor,
    /// driver version, GPU) and the render device with its PCI identity.
    #[cfg(target_os = "linux")]
    pub(crate) async fn fel_encoder_identity(&self) -> Result<String, String> {
        let driver = self
            .caps
            .vaapi_driver
            .as_deref()
            .ok_or("this node's VA driver was not identified by its startup probe")?;
        let ffmpeg = crate::ffmpeg::EncodedExecutable::capture().await?;
        let path = plurx_core::transcode::vaapi_device();
        let device = self
            .caps
            .graphics_devices
            .iter()
            .find(|device| device.render_node.as_deref() == Some(path.as_str()))
            .map_or_else(
                || path.clone(),
                |device| {
                    format!(
                        "{path} {} {}:{}",
                        device.id, device.vendor_id, device.device_id
                    )
                },
            );
        Ok(
            plurx_core::transcode::dv_fel_qualification::encoder_identity(
                &ffmpeg.digest,
                driver,
                &device,
            ),
        )
    }

    /// The full key of this node's FEL verdicts: renderer, recipe, encoder.
    #[cfg(target_os = "linux")]
    pub(crate) async fn fel_identity(&self, scanner: &str) -> Result<String, String> {
        Ok(plurx_core::transcode::dv_fel_qualification::identity(
            scanner,
            plurx_core::transcode::RECONSTRUCTED_P81_RECIPE,
            &self.fel_encoder_identity().await?,
        ))
    }

    /// The route's decision from this node's capability and the title's
    /// verdict for this renderer and recipe. Only an admitting verdict serves
    /// the reconstruction; a missing one is measured and, until then, the play
    /// keeps the plain conversion.
    #[cfg(target_os = "linux")]
    pub(crate) fn fel_gate(
        capable: bool,
        state: plurx_core::transcode::dv_fel_qualification::VerdictState,
    ) -> FelGate {
        use plurx_core::transcode::dv_fel_qualification::VerdictState;
        if !capable {
            return FelGate::Decline(Self::fel_recipe_unqualified().into());
        }
        match state {
            VerdictState::Current(verdict) if verdict.admitted => FelGate::Serve,
            VerdictState::Current(verdict) => FelGate::Decline(verdict.refusal()),
            VerdictState::Missing => FelGate::Measure,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn fel_recipe_unqualified() -> &'static str {
        "no reconstruction recipe is qualified for this server's encoder \
         (the route is measured on radeonsi VA-API only)"
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn fel_being_measured() -> &'static str {
        "whether this title's enhancement layer adds more than re-encoding loses \
         is being measured on this server's GPU"
    }

    /// Rebind the title's scan demand so the title job runs again and
    /// measures it. At most once per title per six hours on this node: a node
    /// that cannot measure (no capable GPU) may pick the job up, and a play
    /// must not turn that into repeated full-title rescans.
    #[cfg(target_os = "linux")]
    pub(crate) async fn request_fel_qualification(
        &self,
        file_id: i64,
        scanner: &str,
        identity: &str,
    ) {
        {
            let mut requests = self
                .fel_requalification_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = std::time::Instant::now();
            requests.retain(|_, at| now.duration_since(*at) < FEL_REQUALIFICATION_INTERVAL);
            if requests.contains_key(&file_id) {
                return;
            }
            requests.insert(file_id, now);
        }
        let binding =
            plurx_core::transcode::dv_fel_qualification::requalify_binding(scanner, identity);
        match self
            .store
            .request_dv_fel_requalification(file_id, &binding)
            .await
        {
            Ok(true) => {
                tracing::info!(target: "plurxd::dv", file_id, "FEL qualification requested")
            }
            Ok(false) => {
                tracing::debug!(target: "plurxd::dv", file_id, "FEL qualification has no title demand")
            }
            Err(error) => {
                tracing::warn!(target: "plurxd::dv", file_id, %error, "FEL qualification request failed")
            }
        }
    }

    /// The node's reconstruction slot, preemptibly, and the GPU and CPU a
    /// qualification window graph is debited, as a claim on the title job's
    /// own admission (`extend_fragment_admission`). `preempt` is the
    /// qualification's cancellation: a live reconstructed 8.1 triggers it to
    /// take the slot.
    #[cfg(target_os = "linux")]
    pub(crate) async fn admit_fel_qualification(
        &self,
        admission: &crate::transcode::FragmentAdmission,
        preempt: tokio_util::sync::CancellationToken,
        decode: crate::dv_segment::RendererDecode,
    ) -> Result<
        (
            crate::transcode::QualificationSlot,
            crate::dv_segment::RendererDecode,
        ),
        String,
    > {
        if decode != crate::dv_segment::RendererDecode::Gpu {
            return Err(
                "this node has not proved a VA-API HEVC Main10 decode; the qualification \
                 measures the GPU-decoded route only"
                    .into(),
            );
        }
        let resources = crate::admission::TranscodeResourceEstimate {
            hardware_slot: true,
            cpu_threads: crate::dv_segment::streaming_helper_cpu_threads(
                plurx_core::transcode::dv_processing::DvProcessingMode::Fel,
                decode,
            ) + crate::source_probe::fel_qualification::CONSUMER_THREADS,
            decoder_threads: Some(crate::dv_segment::STREAMING_DECODER_THREADS),
        };
        let slot = self
            .try_qualification_slot(preempt)
            .ok_or_else(Self::reconstruction_slot_busy)?;
        self.extend_fragment_admission(admission, &resources, Some(1))
            .await?;
        Ok((slot, decode))
    }

    /// The reconstruction slot for a FEL qualification, registered as
    /// preemptible; `None` when anything holds it.
    #[cfg(target_os = "linux")]
    pub(crate) fn try_qualification_slot(
        &self,
        preempt: tokio_util::sync::CancellationToken,
    ) -> Option<crate::transcode::QualificationSlot> {
        let mut holder = self
            .reconstruction_qualification
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let permit = Arc::clone(&self.processed_reconstruction_slots)
            .try_acquire_owned()
            .ok()?;
        *holder = Some(preempt);
        Some(crate::transcode::QualificationSlot {
            permit: Some(permit),
            holder: Arc::clone(&self.reconstruction_qualification),
        })
    }

    /// The renderer's decode placement, which sizes its CPU debit. It decodes
    /// on the GPU when it is handed the VA-API render device (whenever the
    /// device exists) and that device proved an HEVC Main10 decode
    /// (`ffmpeg::has_hevc_main10_vaapi_decode`). The completed window remains
    /// the authority: one that opened its decoders in software under a GPU
    /// admission is refused.
    #[cfg(target_os = "linux")]
    pub(crate) fn processed_renderer_decode(
        device_present: bool,
        main10_vaapi_decode: bool,
    ) -> crate::dv_segment::RendererDecode {
        if device_present && main10_vaapi_decode {
            crate::dv_segment::RendererDecode::Gpu
        } else {
            crate::dv_segment::RendererDecode::Software
        }
    }

    /// What this node would spend on, and has free for, a live FEL
    /// reconstruction, read from the same inputs the preparation uses.
    #[cfg(target_os = "linux")]
    pub(crate) async fn live_reconstruction_facts(&self) -> LiveReconstructionFacts {
        let device_present = std::path::Path::new(&plurx_core::transcode::vaapi_device()).exists();
        let main10_vaapi_decode = crate::ffmpeg::has_hevc_main10_vaapi_decode().await;
        let decode = Self::processed_renderer_decode(device_present, main10_vaapi_decode);
        let renderer_threads = crate::dv_segment::streaming_helper_cpu_threads(
            plurx_core::transcode::dv_processing::DvProcessingMode::Fel,
            decode,
        );
        LiveReconstructionFacts {
            device_present,
            main10_vaapi_decode,
            gpu_decode: decode == crate::dv_segment::RendererDecode::Gpu,
            renderer_threads,
            // The encoder's share with a soundtrack, as preparation debits it.
            encoder_threads: 5,
            software_budget: self.software_budget().await,
            held_by_qualification: self
                .reconstruction_qualification
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
            slots: LIVE_RECONSTRUCTION_SLOTS,
            slots_in_use: LIVE_RECONSTRUCTION_SLOTS
                .saturating_sub(self.processed_reconstruction_slots.available_permits()),
        }
    }

    /// The worker's live reconstruction slot for a reconstructed Profile 8.1
    /// graph; `None` for any other destination (HDR10-E and SDR processing
    /// keep their own admission, unchanged). A slot held by a FEL
    /// qualification is taken from it: the qualification is told to stop and
    /// the play waits up to [`QUALIFICATION_SLOT_YIELD`] for the permit. A
    /// slot held by another live play is the decline reason, and the play
    /// falls back to its ordinary route.
    #[cfg(target_os = "linux")]
    pub(crate) async fn acquire_reconstruction_slot(
        &self,
        destination: plurx_core::transcode::dv_processing::DvDestination,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, String> {
        if destination != plurx_core::transcode::dv_processing::DvDestination::Profile81 {
            return Ok(None);
        }
        let qualification = {
            let holder = self
                .reconstruction_qualification
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Ok(permit) = Arc::clone(&self.processed_reconstruction_slots).try_acquire_owned()
            {
                return Ok(Some(permit));
            }
            holder.clone()
        };
        let Some(qualification) = qualification else {
            return Err(Self::reconstruction_slot_busy());
        };
        qualification.cancel();
        match tokio::time::timeout(
            QUALIFICATION_SLOT_YIELD,
            Arc::clone(&self.processed_reconstruction_slots).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(Some(permit)),
            _ => Err(Self::reconstruction_slot_held_by_qualification()),
        }
    }

    /// Why a play could not take the slot from a FEL qualification.
    #[cfg(target_os = "linux")]
    pub(crate) fn reconstruction_slot_held_by_qualification() -> String {
        format!(
            "this worker's reconstruction slot is held by a FEL qualification measurement \
             that did not yield within {} s",
            QUALIFICATION_SLOT_YIELD.as_secs()
        )
    }

    /// Why a second live FEL reconstruction on this worker was declined.
    #[cfg(target_os = "linux")]
    pub(crate) fn reconstruction_slot_busy() -> String {
        format!(
            "this worker already runs its {} live FEL reconstruction{}; \
             more wait until concurrent reconstruction is measured",
            LIVE_RECONSTRUCTION_SLOTS,
            if LIVE_RECONSTRUCTION_SLOTS == 1 {
                ""
            } else {
                "s"
            }
        )
    }

    /// Called only after ingress has acquired its durable create and checked
    /// predecessor admission. Catalog/restore paths never call this producer.
    pub(crate) async fn prepare_processed_hdr10(
        &self,
        request: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        hdr10_intent: bool,
    ) -> Result<
        Option<(
            SessionRequest,
            plurx_core::playback::candidate::QualityCandidate,
        )>,
        String,
    > {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (request, recovery, hdr10_intent);
            Ok(None)
        }
        #[cfg(target_os = "linux")]
        {
            // Two processed destinations. A Profile 7 copy converted to 8.1
            // for a Dolby Vision display becomes the reconstructed 8.1 (the
            // FEL rebuilt into the picture, a new RPU) under FEL re-encode.
            // Any other HDR10 delivery becomes HDR10-E under Dolby Vision
            // processing.
            use plurx_core::transcode::dv_processing::DvDestination;
            let Some(destination) = Self::processed_qualification_destination(request)? else {
                return Ok(None);
            };
            let profile81 = destination == DvDestination::Profile81;
            // Silent only where no processed destination is what this play
            // is: a native Dolby Vision copy (the display applies the RPU),
            // an SDR output, or the Developer-tab setting off. Everything
            // after the setting check names its reason in the plan notes.
            if request.dv_processing_destination.is_none()
                && matches!(
                    request.kind,
                    SessionKind::Copy {
                        preserve_dolby_vision: true,
                        convert_dolby_vision: false,
                        ..
                    }
                )
            {
                return Ok(None);
            }
            let Some(original) = request.candidate_context.as_ref() else {
                if request.dv_processing_destination.is_some() {
                    return Err(
                        "processed destination requires fresh admitted owner qualification".into(),
                    );
                }
                return Ok(None);
            };
            // Compatible HDR10 copy does not set the legacy re-encode flag.
            // Its accepted destination still permits preference-driven DV
            // processing, subject to the actual completed decoder contract.
            if request.dv_processing_destination.is_none()
                && !profile81
                && destination != DvDestination::Sdr
                && !hdr10_intent
                && original.selected_candidate.grade != OutputGrade::Hdr10
            {
                return Ok(None);
            }
            let Some(planning) = self
                .validate_candidate_planning_binding(request, None)
                .await?
            else {
                return Ok(None);
            };
            let preferences = Self::vod_dv_preferences_from_snapshot(&planning);
            let enabled = if profile81 {
                preferences.fel_reencode
            } else {
                preferences.hdr_processing
            };
            if !enabled {
                return if request.dv_processing_destination.is_some() {
                    Err("the selected processed destination is no longer enabled".into())
                } else {
                    Ok(None)
                };
            }
            if profile81 && !preferences.conversion_permitted {
                return Err("Dolby Vision conversion is not permitted".into());
            }
            if profile81
                && (planning.file.dolby_vision.profile != Some(7)
                    || planning.file.dolby_vision.el_present == Some(false))
            {
                return Err(
                    "FEL re-encode needs a Profile 7 source with its enhancement layer".into(),
                );
            }
            if !profile81 && !matches!(planning.file.dolby_vision.profile, Some(5 | 7 | 8)) {
                return if request.dv_processing_destination.is_some() {
                    Err(
                        "the processed destination needs supported Dolby Vision source metadata"
                            .into(),
                    )
                } else {
                    Ok(None)
                };
            }
            // The setting is on for a DV source: from here every decline is
            // reported with the ordinary start it falls back to.
            if request.continuous_media.is_some() {
                return Err("the continuous adaptive route has no processed rendition yet".into());
            }
            if request.subtitle_burn.is_some() {
                return Err("burned-in subtitles are not drawn into processed HDR10 yet".into());
            }
            // Reconstruction is the better picture only for a title whose EL
            // adds more than the re-encode loses: its measured verdict decides.
            if profile81 {
                self.reconstruction_qualified(&planning).await?;
            }
            let Some(caps) = original.canonical_caps.as_ref() else {
                return Err("the client did not report its playback capabilities".into());
            };
            // The converted 8.1 copy this replaces already established the
            // client's Dolby Vision HLS signalling; HDR10-E is new HEVC.
            if !profile81
                && !caps
                    .hls_hevc_sample_entries
                    .as_ref()
                    .is_some_and(|entries| entries.iter().any(|entry| entry == "hvc1"))
                || !caps.containers.iter().any(|container| container == "mp4")
                || !caps.transports.iter().any(|transport| transport == "hls")
            {
                return Err(
                    "the client does not report HLS fMP4 HEVC (hvc1), which processed output needs"
                        .into(),
                );
            }
            let Ok(_preparation_slot) =
                Arc::clone(&self.processed_preparation_slot).try_acquire_owned()
            else {
                return Err("another processed preparation already holds this node's slot".into());
            };
            // Qualification is an optional enhancement of a start that is
            // already resolved. It spends at most half of the create budget
            // left now, so admissibility, catalog, placement, the durable
            // writes and the dispatch that consumes its result keep theirs.
            let deadline = crate::media_pool::create_enhancement_deadline(Duration::from_secs(8));
            if tokio::time::Instant::now() >= deadline {
                return Err("no startup budget remains for processed qualification".into());
            }
            let mut file = planning.file.clone();
            file.audio_offset_ms = if file.audio_streams.is_empty() {
                0
            } else {
                request.audio_offset_ms.clamp(-15_000, 15_000)
            };
            // The processed rendition is the selected rendition: same height,
            // at any resolution the source has.
            let source_height = file
                .height
                .filter(|height| u32::try_from(*height).is_ok_and(|value| value > 0))
                .ok_or("the source has no valid picture height to process")?;
            let height =
                source_height.min(i64::from(original.selected_candidate.target_height)) & !1;
            if height < 32 {
                return Err("the source has no picture height to process".into());
            }
            let target_enabled = !profile81
                && plurx_core::store::stored_switch(
                    planning
                        .settings
                        .get(keys::DV_DISPLAY_TARGET)
                        .map(String::as_str),
                    false,
                );
            let (display_target, display_target_note) = tokio::time::timeout_at(
                deadline,
                self.display_target_policy_for_source(
                    target_enabled,
                    caps.display.as_ref(),
                    &file,
                    planning.probe_json.as_deref(),
                ),
            )
            .await
            .map_err(|_| {
                "display-target source verification exceeded the preparation budget".to_owned()
            })??;
            if destination == DvDestination::Sdr
                && !display_target.is_some_and(|target| target.is_sdr())
            {
                return Err(display_target_note.unwrap_or_else(|| "SDR display targeting needs a complete eligible L1 scan and the display-target setting".into()));
            }
            let (mut trial, projected) =
                Self::processed_qualification_input(&planning, request, height)?;
            trial.request_id = Some(recovery.incarnation_id.clone());
            let context = trial
                .candidate_context
                .as_mut()
                .ok_or("processed trial context missing")?;
            context.prepared_processing = None;
            context.display_target = display_target;
            context.display_target_note = display_target_note.clone();
            context.retained_output = None;
            context.normalized_geometry = false;
            context.profile = None;
            context.planning_snapshot = Some(Arc::new(planning.clone()));
            let reuse_scope = original
                .catalog_query
                .as_ref()
                .and_then(|query| self.processed_hint_key(&planning, query));
            let reused = if let Some(scope) = reuse_scope {
                self.vod
                    .capture_processed_root(
                        request,
                        &recovery.supersession_scope(),
                        scope,
                        deadline.into_std(),
                    )
                    .await
                    .filter(|root| root.encoding.options.target_height == height
                        && matches!(&root.encoding.dv_processing,
                            plurx_core::transcode::dv_processing::DvSelection::Selected(plan) if plan.destination() == destination)
                        && root.encoding.dv_runtime.as_ref().is_some_and(|runtime| runtime.display_target == display_target))
            } else {
                None
            };
            let mut root = match reused {
                Some(root) => root,
                None => {
                    // Closed graphs retain this bounded allowance until physical cleanup.
                    let Ok(instance_allowance) =
                        Arc::clone(&self.processed_instance_slots).try_acquire_owned()
                    else {
                        return Err("this node's processed graph instances are all in use".into());
                    };
                    let mut credit = None;
                    let Some(encoding) = self
                        .prepare_vod_encoding_with_source(
                            &trial,
                            &file,
                            None,
                            Some(super::planning_hwaccel_preference(&planning)),
                            Some((&mut credit, deadline)),
                            None,
                        )
                        .await?
                    else {
                        return Err("the processed trial froze no encoded recipe".into());
                    };
                    let Some(permit) = credit else {
                        return Err(
                            "the processed trial kept no admitted preparation credit".into()
                        );
                    };
                    crate::vodserve::PreparedRootAdmission {
                        reuse_scope,
                        reused_parent: None,
                        instance_allowance: Some(Arc::new(instance_allowance)),
                        instance_nonce: Some(*uuid::Uuid::new_v4().as_bytes()),
                        encoding,
                        permit,
                    }
                }
            };
            // Exact-root reuse retains already admitted capacity, not a claim
            // that this producer can sustain a new graph. Idle/slow/complete
            // roots need no extra private windows or throughput qualification.
            // Attachment rechecks the exact live parent and reservation.
            let encoding = &mut root.encoding;
            let Some(runtime) = encoding.dv_runtime.as_ref().cloned() else {
                return Err("the processed trial has no processing runtime".into());
            };
            let binding = crate::media_pool::PlanningBinding::from_snapshot(&planning);
            if !matches!(&encoding.dv_processing,
                plurx_core::transcode::dv_processing::DvSelection::Selected(plan) if plan.destination() == destination)
                || encoding.options.target_height != height
            {
                return Err(
                    "the processed plan disagrees with its requested destination or height".into(),
                );
            }
            let digest = encoding.processed_candidate_digest(&file, &binding)?;
            if original.selected_candidate.processed_dv && digest != original.recipe_digest {
                return Err(vod_refusal_error(
                    "candidate_recipe_changed",
                    "processed candidate inputs changed during qualification",
                ));
            }
            if root.reused_parent.is_none() {
                Arc::get_mut(encoding)
                    .ok_or("processed trial executable escaped before qualification")?
                    .candidate_recipe = Some(digest);
            }
            if root.reused_parent.is_none() {
                let grid = encoding.grid;
                let plan = grid.plan(file.duration_ms.ok_or("processed duration missing")?, 0);
                let first = runtime
                    .prepared
                    .lock()
                    .map_err(|_| "processed first window lock poisoned")?
                    .as_ref()
                    .map(|(index, _)| *index)
                    .ok_or("processed first window missing")?;
                let first_entry = plan.entry(first).ok_or("processed first entry missing")?;
                let end_ms = |entry: &plurx_core::segplan::PlanEntry| {
                    ((entry.start_ticks + entry.duration_ticks) * 1000 / u64::from(grid.numerator))
                        as i64
                };
                encoding.reset_active_production(0, true);
                encoding.note_active_segment(0, first, end_ms(first_entry));
                // The first verified window was produced during preparation.
                // Processing is the selected delivery from here: no extra
                // trial windows and no speed threshold decide it. Measured
                // throughput is reported (Developer tab), never a gate.
                if !matches!(
                    &encoding.dv_processing,
                    plurx_core::transcode::dv_processing::DvSelection::Selected(_)
                ) {
                    return Err("the processed trial selected no processing plan".into());
                }
            }
            let encoding = &root.encoding;
            if !encoding
                .engine
                .is_current_with_executable(&encoding.executable)
                .await
            {
                return Err("the processed executable changed during qualification".into());
            }
            if !runtime.source.unchanged() {
                return Err("the source changed during processed qualification".into());
            }
            let facts = Self::quality_facts_from_snapshot(&planning)
                .ok_or("processed client facts unavailable")?;
            let candidate = super::manager_candidates::prepared_processed_candidate(
                digest,
                &encoding.plan,
                &encoding.options,
                caps,
                &facts,
                destination,
                projected.audio_delivery.as_ref(),
            )?;
            trial.audio_delivery = encoding.options.audio.clone();
            let mut context = Self::candidate_context(&candidate);
            context.display_target = display_target;
            context.display_target_note = display_target_note;
            context.catalog_query = original.catalog_query.clone();
            context.canonical_caps = Some(caps.clone());
            context.planning_binding = Some(binding);
            context.planning_snapshot = Some(Arc::new(planning));
            context.owner_node_id = self.cache_location().map(|(_, node)| node.to_owned());
            // The processed candidate enhances a start that was already
            // resolved. Dispatch falls back to that start, with the reason,
            // if the prepared graph cannot be consumed; a client that named
            // the processed candidate itself has no such fallback.
            context.ordinary_request =
                (!original.selected_candidate.processed_dv).then(|| Box::new(request.clone()));
            trial.candidate_context = Some(Box::new(context));
            trial.hdr10 = destination != DvDestination::Sdr;
            let lease_binding = trial.processed_preparation_binding(recovery)?;
            trial
                .candidate_context
                .as_mut()
                .ok_or("processed candidate context missing")?
                .prepared_processing = Some(PreparedCandidateOnce::new(
                lease_binding,
                // Consumable for as long as this create may still dispatch:
                // the same stage deadline that bounds its worker placement.
                crate::media_pool::create_stage_deadline(crate::media_sessions::START_DEADLINE)
                    .into_std(),
                root,
            ));
            Ok(Some((trial, candidate)))
        }
    }

    /// [`Self::prepare_vod_encoding`] with the HWACCEL preference already in
    /// hand (`Some`, from the dispatch revalidation read) or read here
    /// (`None`), at every point the encoder is chosen.
    pub(super) async fn prepare_vod_encoding_with_preference(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        hwaccel: Option<&str>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        self.prepare_vod_encoding_noting(req, file, hwaccel, None)
            .await
    }

    /// [`Self::prepare_vod_encoding_with_preference`] that also reports why an
    /// ordinary start's processing was declined (`decline`), so the session's
    /// enhancement detail names the real reason instead of a generic one.
    pub(super) async fn prepare_vod_encoding_noting(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        hwaccel: Option<&str>,
        decline: Option<&mut Option<String>>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        let encoding = Box::pin(
            self.prepare_vod_encoding_with_source(req, file, None, hwaccel, None, decline),
        )
        .await?;
        if let Some(encoding) = encoding.as_ref() {
            self.refuse_unadmittable_encoding(encoding).await?;
        }
        Ok(encoding)
    }

    /// A frozen plan this node's software budget can never admit is refused
    /// here, typed, rather than accepted and left to wait out the whole
    /// materialize deadline for a permit no release can grant.
    async fn refuse_unadmittable_encoding(
        &self,
        encoding: &crate::vodencode::Encoding,
    ) -> Result<(), String> {
        let software_budget = self.software_budget().await;
        match encoding.unadmittable_software_floor(software_budget) {
            Some(floor) => Err(vod_refusal_error(
                "vod_transcode_unavailable",
                format!(
                    "this encode needs {floor} CPU threads and this server's software \
                     encoder budget is {software_budget}"
                ),
            )),
            None => Ok(()),
        }
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
        let encoding = Box::pin(self.prepare_vod_encoding_with_source(
            prepared.request(),
            prepared.file(),
            Some((evidence, proof)),
            None,
            None,
            None,
        ))
        .await?
        .ok_or_else(|| "Source encoding recipe missing".to_owned())?;
        self.refuse_unadmittable_encoding(&encoding).await?;
        Ok(encoding)
    }

    async fn prepare_vod_encoding_with_source(
        &self,
        req: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        source_evidence: Option<(
            &crate::transcode::source_preparation::SourceHeldProbeEvidence,
            &plurx_core::sharing_source_sessions::SourceSessionWriteAuthority,
        )>,
        hwaccel: Option<&str>,
        mut prepared_admission: Option<(
            &mut Option<crate::vodencode::EncodePermit>,
            tokio::time::Instant,
        )>,
        decline: Option<&mut Option<String>>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        #[cfg(target_os = "linux")]
        let mut decline = decline;
        #[cfg(not(target_os = "linux"))]
        let _ = decline;
        let processed_trial = prepared_admission.is_some();
        req.dv_processing_route()?;
        if req.dv_processing_destination.is_some() && !processed_trial {
            return Err(vod_refusal_error(
                "candidate_recipe_unavailable",
                "processed destination requires fresh admitted owner qualification",
            ));
        }
        #[cfg(target_os = "linux")]
        let processing_deadline = prepared_admission.as_ref().map(|(_, deadline)| *deadline);
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
        let planning = self.vod_preparation_snapshot(req, file).await?;
        let dv_processing = Self::vod_dv_processing_from_snapshot(&planning, req)?;
        if processed_trial {
            if let plurx_core::transcode::dv_processing::DvSelection::KeepExisting(reason) =
                &dv_processing
            {
                if *reason
                    != plurx_core::transcode::dv_processing::DvFallbackReason::AwaitingFirstWindow
                {
                    return Err(format!("processed preflight refused: {reason:?}"));
                }
            }
        }
        #[cfg(target_os = "linux")]
        let dv_tools = if matches!(
            &dv_processing,
            plurx_core::transcode::dv_processing::DvSelection::KeepExisting(
                plurx_core::transcode::dv_processing::DvFallbackReason::AwaitingFirstWindow
            )
        ) && (req.candidate_context.is_none() || processed_trial)
        {
            match crate::vodencode::dv_runtime::ToolSet::capture().await {
                Ok(Some(tools)) => Some(tools),
                // A processed trial exists only to qualify processing: with no
                // backend it has nothing to qualify, and says so.
                Ok(None) if processed_trial => {
                    return Err("no Dolby Vision processing backend is installed".into());
                }
                Ok(None) => None,
                Err(reason) if processed_trial => {
                    return Err(format!("DV backend unavailable: {reason}"));
                }
                Err(reason) => {
                    tracing::warn!(target: "plurxd::transcode", %reason, "DV backend unavailable; retaining ordinary delivery");
                    if let Some(decline) = decline.as_deref_mut() {
                        decline.get_or_insert(reason);
                    }
                    None
                }
            }
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let copy_attempt = matches!(req.kind, SessionKind::Copy { .. })
            && req.subtitle_burn.is_none()
            && dv_tools.is_some();
        #[cfg(not(target_os = "linux"))]
        let copy_attempt = false;
        if matches!(req.kind, SessionKind::Copy { .. }) {
            match req.subtitle_burn {
                None if !copy_attempt => {
                    validate_finite_copy_rate(req, file)?;
                    return Ok(None);
                }
                None => {
                    validate_finite_copy_rate(req, file)?;
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
        tracing::debug!(
            target: "plurxd::transcode",
            file_id = file.id,
            planning_generation = planning.generation,
            selection = ?dv_processing,
            "worker selected DV processing or retained existing delivery"
        );
        let result = async {
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
        let needs_legacy_reporter =
            source_evidence.is_none() && self.decode_probe_identity.is_none();
        let legacy_clock_reporter = if needs_legacy_reporter {
            Some(
                crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::ffprobe_bin())
                    .await
                    .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?,
            )
        } else {
            None
        };
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
                    crate::ffmpeg::held_source_probe_json_bound(
                        &source.handle,
                        VOD_START_HELD_PROBE,
                        legacy_clock_reporter
                            .as_ref()
                            .expect("legacy held reporter captured"),
                    )
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
        let clock_reporter = if let Some((evidence, _)) = source_evidence {
            evidence.clock_reporter().clone()
        } else if let Some(probe) = self.decode_probe_identity.as_ref() {
            let captured =
                crate::ffmpeg::EncodedExecutable::capture_at(probe.executable().to_owned())
                    .await
                    .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
            if captured.digest != probe.content_digest() {
                return Err(vod_refusal_error(
                    "vod_engine_unattested",
                    "the held source timing reporter changed",
                ));
            }
            captured
        } else {
            legacy_clock_reporter.expect("native fallback captures its reporter before the query")
        };
        if !clock_reporter.is_current().await {
            return Err(vod_refusal_error(
                "vod_engine_unattested",
                "the held source timing reporter changed",
            ));
        }
        let source_clock = if let Some((evidence, _)) = source_evidence {
            evidence.source_clock()
        } else {
            crate::transcode::source_preparation::held_source_clock(&held_probe)
                .map_err(|error| vod_refusal_error("vod_source_rescan_required", error))?
        };
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
            self.encoder_and_grade_preferring(
                file,
                req.hdr10,
                target_height,
                subtitle_burn.is_some(),
                hwaccel,
            )
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
                            .encoder_and_grade_preferring(
                                file,
                                req.hdr10,
                                target_height,
                                false,
                                hwaccel,
                            )
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
        options.output_codec =
            Self::selected_output_codec(req.candidate_context.as_deref(), encoder);
        // Catalog identity describes the muxed candidate. Verify that exact
        // plan before deriving the video-only or shared-AAC execution recipe.
        let phase_started = std::time::Instant::now();
        let catalog_plan = if let Some(context) = req
            .candidate_context
            .as_ref()
            .filter(|_| req.continuous_media.is_some())
        {
            let catalog_encoder = if shared_audio_role {
                self.encoder_and_grade_preferring(file, false, target_height, false, hwaccel)
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
            catalog_options.output_codec =
                Self::selected_output_codec(Some(context), catalog_encoder);
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
        // The engine retained by Encoding is also the font authority for this plan.
        // Local finite VOD has no shared Source evidence, but owns the same capture.
        let held_font_digest = engine.font_digest();
        let phase_started = std::time::Instant::now();
        #[cfg(target_os = "linux")]
        let source_offsets = Arc::new(tokio::sync::Semaphore::new(1));
        #[cfg(target_os = "linux")]
        let source_frame_probe = if let Some(prepared) = held_decode_facts.as_ref().filter(|_| dv_tools.is_some()) {
            let selected = prepared.absolute_video_stream().ok_or("held video stream unavailable")?;
            self.source_frame_probe(file, Arc::new(source.handle.try_clone().map_err(|error| error.to_string())?), Arc::clone(&source_offsets), prepared, selected)?
        } else { None };
        let plan = if let Some(prepared) = held_decode_facts {
            self.resolve_vod_prepared_source(
                file,
                &options,
                encoder,
                held_plan_handle,
                prepared,
                held_font_digest,
            )
            .await?
        } else {
            self.resolve_vod_movie_plan_with_fonts(
                file,
                &options,
                encoder,
                held_plan_handle,
                held_font_digest,
            )
            .await?
        };
        note_phase("execution_decoder_plan", phase_started);
        // Encoding retains these execution options for publication and
        // diagnostics. Its graph must agree with the immutable plan.
        options.pipeline = plan.options().pipeline;
        options.strict_dolby = plan.options().strict_dolby.clone();
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
        let plan = plan.with_sdr_avc_qualification(&self.caps, cadence, options.force_idr).with_vaapi_hdr_policy(&self.caps);
        let reorder_frames = Self::vod_reorder_from_snapshot(&planning);
        if let Some(context) = req.candidate_context.as_ref().filter(|_| !processed_trial) {
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
        if !source.unchanged() {
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                "the source changed while attesting the encoded engine; rescan it before playback",
            ));
        }
        let executable = crate::ffmpeg::EncodedExecutable::capture()
            .await
            .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
        let executable = executable
            .bind_linux_plan(&plan)
            .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
        if !executable.matches_macos_plan(&plan) {
            return Err(vod_refusal_error(
                "vod_engine_unattested",
                "the encoder differs from the frozen Mac processing plan; check compatibility again",
            ));
        }
        let mut engine = engine;
        engine
            .bind_clock_pair(&clock_reporter, &executable)
            .await
            .map_err(|error| vod_refusal_error("vod_engine_unattested", error))?;
        #[cfg(target_os = "linux")]
        let mut prepared_credit: Option<crate::vodencode::EncodePermit> = None;
        #[cfg(not(target_os = "linux"))]
        let prepared_credit: Option<crate::vodencode::EncodePermit> = None;
        #[cfg(target_os = "linux")]
        let (plan, options, resources, dv_processing, dv_runtime) = {
            let mut retained = (plan, options, resources, dv_processing, None);
            if let Some(tools) = dv_tools {
                let attempt = async {
                    use crate::vodencode::dv_runtime::{RuntimeRecipe, failure_episode_key, episode_failed};
                    use plurx_core::transcode::dv_processing::DvSelection;
                    let (_, destination, _) = req.dv_processing_route()?;
                    // Every start, not only one the ingress offered: the title's
                    // FEL verdict decides whether it is reconstructed.
                    if destination == plurx_core::transcode::dv_processing::DvDestination::Profile81 {
                        self.reconstruction_qualified(&planning).await?;
                    }
                    let preferences = Self::vod_dv_preferences_from_snapshot(&planning);
                    let failure_episode = failure_episode_key(&req.playback_id, &source_object_version,
                        &tools, preferences, destination)?;
                    if episode_failed(&failure_episode) {
                        return Err("DV processing previously failed for this playback/source/settings/backend".into());
                    }
                    let document: serde_json::Value = serde_json::from_str(&held_probe).map_err(|e| e.to_string())?;
                    let stream = document.get("streams").and_then(serde_json::Value::as_array)
                        .and_then(|streams| streams.iter().find(|stream| stream.get("index")
                            .and_then(serde_json::Value::as_u64) == Some(u64::from(retained.0.decode().input_video_stream()))))
                        .ok_or("held video stream unavailable")?;
                    let clock = stream.get("time_base").and_then(serde_json::Value::as_str)
                        .and_then(|clock| clock.split_once('/')).ok_or("held source has no rational time base")?;
                    let source_tick = (clock.0.parse::<u32>().map_err(|e| e.to_string())?,
                        clock.1.parse::<u32>().map_err(|e| e.to_string())?);
                    let source_origin = crate::transcode::source_preparation::held_dv_source_origin(
                        stream, source_tick, source_clock)?;
                    let dimension = |name| stream.get(name).and_then(serde_json::Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()).ok_or("held native raster unavailable");
                    let native_bl = (dimension("width")?, dimension("height")?);
                    let source = Arc::new(source);
                    let mode = if file.dolby_vision.profile == Some(7) && file.dolby_vision.el_present != Some(false) {
                        plurx_core::transcode::dv_processing::DvProcessingMode::Fel
                    } else { plurx_core::transcode::dv_processing::DvProcessingMode::BaseRpu };
                    // One live FEL reconstruction per worker. A second play falls
                    // back to its ordinary route with this reason.
                    let reconstruction_slot = self.acquire_reconstruction_slot(destination).await?;
                    let preparation_budget = self.vod_settings(req).await?
                        .ok_or("DV processing requires the VOD preparation budget")?.block_budget;
                    let mut processed_options = retained.1.clone();
                    processed_options.pipeline = self.live_lookup_options(self.rate_control_snapshot(),
                        Encoder::Software, file, target_height, 0.0, req.audio_index, None, Some(1),
                        OutputGrade::Hdr10).pipeline;
                    processed_options.software_threads = Some(1);
                    // The reconstructed 8.1 replaces a copy of the disc's base
                    // layer at full rate, so it is not held to the 2160p
                    // transcode rung: min(source average, 60 Mb/s).
                    if destination == plurx_core::transcode::dv_processing::DvDestination::Profile81 {
                        processed_options.video_bitrate_kbps =
                            plurx_core::transcode::reconstructed_p81_video_kbps(file);
                    }
                    let held = source.handle.try_clone().map(Arc::new).map_err(|e| e.to_string())?;
                    let processed_plan = self.resolve_vod_movie_plan(file, &processed_options, Encoder::Software, held).await?;
                    let worker_encoder = self.encoder_for_preference(planning.settings.get(keys::HWACCEL).map(String::as_str).unwrap_or(""));
                    let processed_encoder = crate::vodencode::dv_runtime::completed_window_encoder(
                        worker_encoder, self.hdr10_passthrough_vaapi, &processed_plan, grid);
                    if source_frame_probe.as_ref().is_some_and(|probe| probe.absolute_stream() != retained.0.decode().input_video_stream()) {
                        return Err("source frame metadata selected stream differs from the held decoder plan".into());
                    }
                    // The renderer is handed the VA-API render device whenever it
                    // exists; it decodes there when the device decodes HEVC Main10,
                    // proved once per process. That decides the CPU debit.
                    let renderer_decode = Self::processed_renderer_decode(
                        std::path::Path::new(&plurx_core::transcode::vaapi_device()).exists(),
                        crate::ffmpeg::has_hevc_main10_vaapi_decode().await,
                    );
                    let source_mastering = plurx_core::transcode::dv_processing::source_mastering_metadata(stream);
                    let source_mastering_minimum = plurx_core::transcode::dv_processing::source_mastering_minimum(stream);
                    let display_target = req.candidate_context.as_ref().filter(|_| processed_trial).and_then(|context| context.display_target);
                    // Recheck against the exact stored document compared with
                    // this held source. A title scan replacement cannot reuse
                    // an older live policy or silently change a later window.
                    if let Some(target) = display_target {
                        let stats = probe.as_deref().and_then(plurx_core::scan::dv_title::DvTitleStats::from_probe_json);
                        let selected = if target.is_sdr() { plurx_core::transcode::dv_processing::DvDisplayTarget::for_sdr_title(stats) } else { plurx_core::transcode::dv_processing::DvDisplayTarget::for_title(target.nits(), stats) }
                            .map_err(|error| error.to_string())?;
                        let selected = match selected {
                            plurx_core::transcode::dv_processing::DvDisplayTargetChoice::Target(proposal) => {
                                let proposal = if proposal.is_sdr() {
                                    let minimum = if proposal.needs_source_floor() {
                                        crate::vodencode::dv_runtime::held_source_minimum(source_mastering_minimum.clone(), source_frame_probe.as_deref(), None).await?
                                    } else { None };
                                    proposal.resolve_sdr_floor(minimum).map_err(|error| error.to_string())?
                                } else {
                                    let source = if proposal.needs_source_mastering() {
                                        crate::vodencode::dv_runtime::held_source_mastering(source_mastering.clone(), source_frame_probe.as_deref(), None).await?
                                    } else { None };
                                    proposal.resolve_source(source).map_err(|error| error.to_string())?
                                };
                                plurx_core::transcode::dv_processing::DvDisplayTargetChoice::Target(proposal)
                            }
                            other => other,
                        };
                        if selected != plurx_core::transcode::dv_processing::DvDisplayTargetChoice::Target(target) {
                            return Err("complete-title display-target scan changed during preparation".into());
                        }
                    }
                    let runtime = RuntimeRecipe { tools, source, source_offsets, source_frame_probe,
                        source_tick, source_origin, native_bl, source_clock, preferences,
                        display_target,
                        display: req.candidate_context.as_ref().and_then(|context| context.canonical_caps.as_ref()).and_then(|caps| caps.display.clone()),
                        display_target_note: req.candidate_context.as_ref().and_then(|context| context.display_target_note.clone()),
                        source_mastering, source_mastering_minimum,
                        destination, mode, decode: renderer_decode, reconstruction_slot, encoder: processed_encoder,
                        vaapi_hdr_policy: (processed_encoder == Encoder::Vaapi).then_some(self.caps.vaapi_hdr_policy).flatten(),
                        runtime_cache: self.runtime_cache.clone(),
                        shader_cache: Arc::new(tempfile::Builder::new().prefix("dv-shader-cache-")
                            .tempdir_in(&self.runtime_cache).map_err(|e| e.to_string())?),
                        prepared: std::sync::Mutex::new(None), failure_episode, preparation_budget,
                        report_revision: std::sync::atomic::AtomicU64::new(0),
                        published_receipt: std::sync::Mutex::new(None) };
                    let helper_threads = crate::dv_segment::streaming_helper_cpu_threads(mode, renderer_decode);
                    let processed_resources = TranscodeResourceEstimate { hardware_slot: true,
                        cpu_threads: helper_threads + if processed_plan.options().input_has_audio { 5 } else { 3 },
                        decoder_threads: Some(crate::dv_segment::STREAMING_DECODER_THREADS) };
                    let admission_deadline = crate::media_pool::create_stage_deadline(crate::admission::QUEUE_WAIT);
                    let admission_deadline = processing_deadline.map_or(admission_deadline, |absolute| admission_deadline.min(absolute));
                    let software_budget = self.software_budget().await;
                    let bundle = crate::vodencode::admit_frozen_preparation(&self.admissions,
                        self.max_hw_sessions().await, software_budget, &processed_resources,
                        Some(1), admission_deadline.into_std()).await
                        .map_err(|unadmittable| if unadmittable {
                            format!("DV processing needs {} CPU threads ({helper_threads} renderer, {} encoder) \
                                 and this server's software encoder budget is {software_budget}",
                                processed_resources.cpu_threads, processed_resources.cpu_threads - helper_threads)
                        } else {
                            format!("DV processing needs a GPU slot and {} CPU threads that other work holds",
                                processed_resources.cpu_threads)
                        })?;
                    let segment_plan = grid.plan(file.duration_ms.ok_or("held source duration unavailable")?, 0);
                    let at = ((req.start_seconds.max(0.0) * f64::from(grid.numerator)) as u64
                        / grid.segment_ticks()).min(segment_plan.entries.len().saturating_sub(1) as u64) as usize;
                    let entry = segment_plan.entries.get(at).ok_or("no requested source interval")?;
                    let (credit, admission) = if processed_trial {
                        let credit = crate::vodencode::EncodePermit::from(bundle);
                        let worker = credit.clone().try_claim_worker().ok_or("DV preparation credit already claimed")?;
                        (Some(credit), crate::dv_segment::SegmentAdmission::Retained(worker))
                    } else { (None, crate::dv_segment::SegmentAdmission::Owned(bundle)) };
                    let (verified, selected) = match runtime.execute_until(file, &processed_plan, &processed_options, grid,
                        entry, &executable.path, admission,
                        Arc::new(crate::prodrun::ProducerSlot::new()), tokio_util::sync::CancellationToken::new(), None, processing_deadline).await {
                            // This awaited owner still holds the frozen graph's
                            // admission. Dropping it never reaches this branch;
                            // normal cancellation makes no backend claim.
                            Err(cause) => { runtime.refuse_episode(); return Err(cause); },
                            Ok(crate::vodencode::dv_runtime::WindowOutcome::Completed(window)) => window,
                            Ok(crate::vodencode::dv_runtime::WindowOutcome::Cancelled) => return Err("processed preparation cancelled".into()),
                        };
                    runtime.prepared.lock().map_err(|_| "DV prepared window lock poisoned")?
                        .replace((entry.index, verified));
                    if selected.destination() != destination {
                        return Err("completed processing plan destination changed".into());
                    }
                    Ok::<_, String>(((processed_plan, processed_options, processed_resources,
                        DvSelection::Selected(Box::new(selected)), Some(Arc::new(runtime))), credit))
                }.await;
                match attempt {
                    Ok((selected, credit)) => { retained = selected; prepared_credit = credit; },
                    // The trial's whole purpose was that window: its refusal
                    // is the qualification's answer, reported with the
                    // ordinary start, not an ordinary recipe nobody asked for.
                    Err(reason) if processed_trial => {
                        return Err(format!("processed window refused: {reason}"));
                    }
                    Err(reason) => {
                        retained.3 = plurx_core::transcode::dv_processing::DvSelection::KeepExisting(
                            plurx_core::transcode::dv_processing::DvFallbackReason::BackendFailure);
                        tracing::warn!(target: "plurxd::transcode", %reason,
                            "DV window refused; retaining ordinary delivery");
                        // The reason, not the generic fallback class, is what
                        // the session's enhancement detail shows.
                        if let Some(decline) = decline.as_deref_mut() {
                            decline.get_or_insert(reason);
                        }
                    }
                }
            }
            if copy_attempt && retained.4.is_none() { return Ok(None); }
            retained
        };
        let encoding = Arc::new(crate::vodencode::Encoding {
            #[cfg(target_os = "linux")]
            planning_metadata: None,
            source_clock,
            shared_audio,
            source_object_version,
            dv_processing,
            #[cfg(target_os = "linux")]
            dv_runtime,
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
                .filter(|_| req.continuous_media.is_none() && !processed_trial)
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
        });
        if let Some((sink, _)) = prepared_admission.as_mut() { **sink = prepared_credit; }
        Ok(Some(encoding))
        }.await;
        if copy_attempt && !processed_trial {
            match result {
                Err(reason) => {
                    tracing::warn!(target: "plurxd::transcode", %reason, "DV copy processing refused; retaining source fallback");
                    Ok(None)
                }
                value => value,
            }
        } else {
            result
        }
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
                    root_admission: None,
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
        if let Some(destination) = req.dv_processing_destination {
            req.dv_processing_route()?;
            if encoding.as_ref().is_none_or(|encoding|
                !matches!(req.kind, SessionKind::Transcode { height } if height == encoding.options.target_height)
                    || !matches!(&encoding.dv_processing,
                        plurx_core::transcode::dv_processing::DvSelection::Selected(plan) if plan.destination() == destination))
            {
                return Err(vod_refusal_error("candidate_recipe_changed", "retained processed destination or height changed"));
            }
        }
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
                    |encoding| encoding.delivered_output_grade(),
                );
                let (actual, route) = if let Some(encoding) = encoding {
                    (
                        if context.selected_candidate.processed_dv {
                            encoding.processed_candidate_digest(
                                file,
                                context
                                    .planning_binding
                                    .as_ref()
                                    .ok_or("processed planning binding missing")?,
                            )?
                        } else {
                            self.candidate_recipe_digest(
                                &encoding.plan,
                                req.presentation,
                                encoding.reorder_frames,
                            )?
                        },
                        CandidateRoute::Encode,
                    )
                } else if let SessionKind::Copy {
                    aac,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                    ..
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

    /// Consume the prepared processing graph a processed candidate was
    /// qualified with, or say exactly why it cannot be: the typed refusal
    /// `try_vod_session` reports, or degrades from, with the ordinary request.
    async fn admit_prepared_processing(
        &self,
        req: &SessionRequest,
        context: &CandidateExecutionContext,
        recovery: Option<&SessionRecoveryIdentity>,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<crate::vodserve::PreparedRootAdmission, String> {
        let recovery = recovery.ok_or("processed candidate requires typed recovery identity")?;
        let prepared = context
            .prepared_processing
            .as_ref()
            .ok_or_else(|| {
                vod_refusal_error(
                    "candidate_recipe_unavailable",
                    "processed candidate requires fresh admitted owner qualification",
                )
            })?
            .take(
                req.processed_preparation_binding(recovery)
                    .map_err(|error| vod_refusal_error("candidate_recipe_changed", error))?,
            )
            .map_err(|error| vod_refusal_error("candidate_recipe_unavailable", error))?;
        self.validate_processed_root(req, context, file, &prepared)
            .await?;
        Ok(prepared)
    }

    /// Physical window controls consume the same one-shot owner lease and
    /// source/recipe validation as dispatch, without starting a registry driver.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) async fn admit_processed_window_control(
        &self,
        request: &SessionRequest,
        recovery: &SessionRecoveryIdentity,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<crate::vodserve::PreparedRootAdmission, String> {
        let context = request
            .candidate_context
            .as_ref()
            .ok_or("processed context missing")?;
        self.admit_prepared_processing(request, context, Some(recovery), file)
            .await
    }

    /// Rebuild descriptive inputs only under a verified first-preparation
    /// expectation, borrowing custody from its exact process-local predecessor.
    /// This does not relax generic serialized catalog restoration.
    async fn restore_staged_processed_root(
        &self,
        remote: &mut crate::media_sessions::RemoteStartRequest,
        predecessor: &StagedProcessedPredecessor,
        deadline: Instant,
    ) -> Result<crate::vodserve::PreparedRootAdmission, String> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (remote, predecessor, deadline);
            Err("processed staged roots require the local processing owner".into())
        }
        #[cfg(target_os = "linux")]
        {
            let req = &remote.request;
            let catalog = remote
                .candidate_catalog
                .as_ref()
                .ok_or("processed catalog missing")?;
            let selected = &catalog.candidate;
            if remote.incarnation_id != predecessor.successor_incarnation_id
                || req.request_id.as_deref() != Some(remote.incarnation_id.as_str())
                || remote.principal.local_user_id().is_none()
                || req.previous_session_id.is_some()
                || req.presentation != Presentation::Vod
                || req.continuous_media.is_some()
                || req.subtitle_burn.is_some()
                || req.dv_processing_destination.is_none()
                || remote.candidate_id != Some(selected.id)
                || !selected.identity_matches()
            {
                return Err("processed staged identity changed".into());
            }
            let current = self
                .store
                .media_session_route_by_incarnation(&predecessor.incarnation_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or("processed predecessor missing")?;
            if current.session_id != predecessor.session_id
                || current.owner_node_id != predecessor.owner_node_id
                || current.owner_epoch != predecessor.owner_epoch
                || current.principal != remote.principal
                || current.playback_id != req.playback_id
                || current.state != "active"
                || current.lease_expires_at_ms <= crate::media_sessions::unix_ms()
                || self.cache_location().map(|(_, node)| node)
                    != Some(predecessor.owner_node_id.as_str())
            {
                return Err("processed predecessor owner changed".into());
            }
            let (seed, root) = self
                .vod
                .capture_staged_processed_root(
                    req,
                    predecessor,
                    &principal_supersession_scope(&remote.principal),
                    deadline,
                )
                .await
                .ok_or("exact processed predecessor unavailable")?;
            let planning = self
                .store
                .playback_planning_snapshot(req.file_id, &QUALITY_PLANNING_KEYS)
                .await
                .map_err(|error| error.to_string())?
                .ok_or("processed source missing")?;
            let binding = crate::media_pool::PlanningBinding::from_snapshot(&planning);
            if binding != seed.binding
                || binding != catalog.binding
                || planning.file.size != remote.source_size
                || planning.file.mtime != remote.source_mtime
                || selected.id != seed.selected.id
                || selected.recipe_digest != seed.selected.recipe_digest
            {
                return Err("processed staged source or recipe changed".into());
            }
            let mut query = crate::media_pool::QualityCatalogRequest {
                file_id: req.file_id,
                source_size: remote.source_size,
                source_mtime: remote.source_mtime,
                caps: catalog.caps.clone(),
                audio_index: req.audio_index,
                audio_offset_ms: req.audio_offset_ms,
                audio_claim: req.audio_claim.clone(),
                audio_delivery: req.audio_delivery.clone(),
                subtitle_burn: req.subtitle_burn,
                presentation: req.presentation,
                // Only original reviewed Copy provenance crosses from the seed.
                copy_contract: seed.query.copy_contract,
            };
            query.validate().map_err(|error| format!("{error:?}"))?;
            if let Some(decoder) = remote.decoder_caps.as_ref() {
                query.caps.video = decoder.device_caps().video;
                query.validate().map_err(|error| format!("{error:?}"))?;
            }
            if query.caps.legacy_blanket_dolby_vision != seed.caps.legacy_blanket_dolby_vision
                || serde_json::to_value(&query.caps).map_err(|error| error.to_string())?
                    != serde_json::to_value(&seed.caps).map_err(|error| error.to_string())?
            {
                return Err("processed staged decoder scope changed".into());
            }
            let projected = Self::processed_catalog_query(&planning, &query)
                .ok_or("processed staged audio changed")?;
            let scope = self
                .processed_hint_key(&planning, &query)
                .ok_or("processed staged scope unavailable")?;
            if root.reuse_scope != Some(scope)
                || self.processed_hint_key(&planning, &seed.query) != Some(scope)
                || req.audio_claim != projected.audio_claim
                || projected.audio_index != req.audio_index
                || projected.audio_offset_ms != req.audio_offset_ms
            {
                return Err("processed staged immutable scope changed".into());
            }
            let mut file = planning.file.clone();
            file.audio_offset_ms = projected.audio_offset_ms;
            let mut context = Self::candidate_context(selected);
            context.catalog_query = Some(query.clone());
            context.canonical_caps = Some(query.caps);
            context.planning_binding = Some(binding);
            context.planning_snapshot = Some(Arc::new(planning));
            context.owner_node_id = Some(predecessor.owner_node_id.clone());
            self.validate_processed_root(req, &context, &file, &root)
                .await?;
            remote.request.candidate_context = Some(Box::new(context));
            Ok(root)
        }
    }

    /// Shared correctness validation for fresh one-shot qualification and an
    /// exact staged predecessor. Catalog speed/ranking observations do not
    /// decide whether already admitted physical capacity can be retained.
    async fn validate_processed_root(
        &self,
        req: &SessionRequest,
        context: &CandidateExecutionContext,
        file: &plurx_core::domain::MediaFile,
        prepared: &crate::vodserve::PreparedRootAdmission,
    ) -> Result<(), String> {
        let (_, destination, _) = req.dv_processing_route()?;
        let binding = context
            .planning_binding
            .as_ref()
            .ok_or("processed planning binding missing")?;
        let actual = prepared
            .encoding
            .processed_candidate_digest(file, binding)
            .map_err(|error| vod_refusal_error("candidate_recipe_changed", error))?;
        #[cfg(target_os = "linux")]
        {
            let snapshot = context
                .planning_snapshot
                .as_ref()
                .ok_or("processed planning snapshot missing")?;
            let query = context
                .catalog_query
                .as_ref()
                .ok_or("processed original query missing")?;
            let projected = Self::processed_catalog_query(snapshot, query)
                .ok_or("processed audio claim changed")?;
            let facts = Self::quality_facts_from_snapshot(snapshot)
                .ok_or("processed client facts unavailable")?;
            let caps = context
                .canonical_caps
                .as_ref()
                .ok_or("processed client caps missing")?;
            if Self::processed_catalog_destination(snapshot, query) != Some(destination)
                || !super::manager_candidates::processed_client_compatible(
                    caps,
                    &prepared.encoding.plan,
                    &facts,
                    destination,
                )
                || projected
                    .audio_delivery
                    .as_ref()
                    .zip(prepared.encoding.options.audio.as_ref())
                    .is_none_or(|(expected, actual)| {
                        expected.byte_identity() != actual.byte_identity()
                    })
                || req
                    .audio_delivery
                    .as_ref()
                    .zip(prepared.encoding.options.audio.as_ref())
                    .is_none_or(|(expected, actual)| {
                        expected.byte_identity() != actual.byte_identity()
                    })
            {
                return Err(vod_refusal_error(
                    "candidate_recipe_changed",
                    "processed decoder or encoded audio contract changed",
                ));
            }
        }
        if actual != context.recipe_digest
            || !matches!(&prepared.encoding.dv_processing,
                plurx_core::transcode::dv_processing::DvSelection::Selected(plan) if plan.destination() == destination)
            || !matches!(req.kind, SessionKind::Transcode { height } if height == prepared.encoding.options.target_height)
            || plurx_core::playback::candidate::CandidateId::for_recipe_digest(actual)
                != context.candidate_id
            || context.owner_node_id.as_deref() != self.cache_location().map(|(_, node)| node)
            || req.continuous_media.is_some()
            || req.subtitle_burn.is_some()
            || req.hdr10
                != (destination != plurx_core::transcode::dv_processing::DvDestination::Sdr)
            || context.normalized_geometry != prepared.encoding.options.normalized_geometry
            || context.profile != prepared.encoding.options.auto_quality_rate_profile
            || !context.selected_candidate.decoder_compatible
            || !context.selected_candidate.processed_dv
            || context.selected_candidate.route
                != plurx_core::playback::candidate::CandidateRoute::Encode
            || context.selected_candidate.planned_codec.as_deref() != Some("hevc")
            || context.selected_candidate.grade != prepared.encoding.delivered_output_grade()
            || context.selected_candidate.width
                != prepared
                    .encoding
                    .plan
                    .output_contract()
                    .effective_width()
                    .unwrap_or(0)
            || context.selected_candidate.height
                != prepared
                    .encoding
                    .plan
                    .output_contract()
                    .effective_height()
                    .unwrap_or(0)
            || context.selected_candidate.id != context.candidate_id
            || context.selected_candidate.recipe_digest != actual
            || i64::from(context.selected_candidate.target_height)
                != prepared.encoding.options.target_height
            || !prepared
                .encoding
                .engine
                .is_current_with_executable(&prepared.encoding.executable)
                .await
        {
            return Err(vod_refusal_error(
                "candidate_recipe_changed",
                "admitted processed executable changed",
            ));
        }
        #[cfg(target_os = "linux")]
        if prepared
            .encoding
            .dv_runtime
            .as_ref()
            .is_none_or(|runtime| !runtime.source.unchanged())
        {
            return Err(vod_refusal_error(
                "candidate_recipe_changed",
                "processed source changed",
            ));
        }
        Ok(())
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
        // The dispatch revalidation's fresh read, on a candidate route. The
        // source row and HWACCEL preference come from it: it was taken in
        // this task, moments ago, and a second read would answer the same.
        revalidated: Option<&plurx_core::store::PlaybackPlanningSnapshot>,
        recovery: Option<&SessionRecoveryIdentity>,
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
        // The source row and, from the same statement, its item's title (the
        // session's attribution): the dispatch revalidation's read on a
        // candidate route, otherwise one snapshot read with no settings keys
        // where `get_file` used to be.
        let (mut file, source_item_title) = match revalidated {
            Some(snapshot) if snapshot.file.id == req.file_id => {
                (snapshot.file.clone(), snapshot.item_title.clone())
            }
            _ => {
                let snapshot = self
                    .store
                    .playback_planning_snapshot(req.file_id, &[])
                    .await
                    .map_err(|error| {
                        start_infrastructure_error(format!("reading the source file: {error}"))
                    })?
                    .ok_or_else(|| "the file no longer exists".to_owned())?;
                (snapshot.file, snapshot.item_title)
            }
        };
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            req.audio_offset_ms.clamp(-15_000, 15_000)
        };
        // A processed Auto candidate is an enhancement of the ordinary request
        // the create had already resolved. When its prepared graph cannot be
        // consumed, that ordinary request is served and the reason travels
        // with the start; only a client that named the processed candidate
        // itself is refused.
        let mut degraded_processing = None;
        let degraded_request;
        let (req, root_admission) = match req
            .candidate_context
            .as_ref()
            .filter(|context| context.selected_candidate.processed_dv)
        {
            Some(context) => match self
                .admit_prepared_processing(req, context, recovery, &file)
                .await
            {
                Ok(prepared) => (req, Some(prepared)),
                Err(error) => match context.ordinary_request.as_deref() {
                    Some(ordinary) if ordinary.file_id == req.file_id => {
                        let reason = vod_refusal(&error)
                            .map_or_else(|| error.clone(), |(_, detail)| detail.to_owned());
                        tracing::warn!(
                            target: "plurxd::transcode",
                            file_id = req.file_id,
                            %reason,
                            "processed candidate could not be consumed; serving the ordinary request"
                        );
                        degraded_processing = Some(reason);
                        // The same dispatch, under the same incarnation.
                        degraded_request = SessionRequest {
                            request_id: req.request_id.clone(),
                            ..ordinary.clone()
                        };
                        (&degraded_request, None)
                    }
                    _ => return Err(error),
                },
            },
            None => (req, None),
        };
        let mut processing_note = None;
        let encoding = match root_admission.as_ref() {
            Some(prepared) => Some(Arc::clone(&prepared.encoding)),
            None => {
                self.prepare_vod_encoding_noting(
                    req,
                    &file,
                    revalidated.map(super::planning_hwaccel_preference),
                    Some(&mut processing_note),
                )
                .await?
            }
        };
        let processed_dv_profile = encoding
            .as_ref()
            .filter(|encoding| encoding.preserves_processed_dv())
            .map(|_| 8);
        let processed_fel = encoding
            .as_ref()
            .and_then(|encoding| encoding.processed_fel());
        let target_height = encoding
            .as_ref()
            .map_or(file.height.unwrap_or(0), |encoding| {
                encoding.options.target_height
            });
        let grade = encoding.as_ref().map_or(OutputGrade::Sdr, |encoding| {
            encoding.delivered_output_grade()
        });
        let kind = encoding
            .as_ref()
            .map_or(req.kind, |encoding| SessionKind::Transcode {
                height: encoding.options.target_height,
            });
        let encoder = encoding
            .as_ref()
            .map_or("vod", |encoding| encoding.delivered_encoder_label());
        let codec_qualification = encoding.as_ref().map(|encoding| {
            (
                encoding.plan.encoder(),
                encoding.delivered_output_grade(),
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
            && root_admission.is_none()
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
            root_admission,
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
        let item_title = source_item_title.unwrap_or_else(|| format!("#{}", file.item_id));
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
                        || is_start_infrastructure_error(&error)
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
            processed_dv_profile,
            processed_fel,
            degraded_processing,
            processing_note,
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
    pub(crate) async fn vod_settings(
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
        self.vod_playlist_with_acquisition_start(session_id, None)
            .await
    }

    pub(crate) async fn vod_playlist_with_acquisition_start(
        &self,
        session_id: &str,
        acquisition_start_ms: Option<i64>,
    ) -> Option<VodResponsePublication<Vec<u8>>> {
        self.vod
            .playlist_with_acquisition_start(session_id, acquisition_start_ms)
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
        let seal = plurx_core::store::PreparedOutputSeal {
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
        };
        // The capture already sealed this exact output; confirm it still
        // holds under the same predicate with a consistent read rather than
        // replaying the complete seal as a no-op replicated UPDATE.
        let ready = self
            .store
            .prepared_output_sealed(&seal)
            .await
            .is_ok_and(|sealed| sealed);
        trace_prepared_capture(&seal, None, ready);
        ready
    }

    /// No public metadata can mint this proof: the exact private output must
    /// already be sealed into the durable reservation and attached locally.
    pub(crate) async fn prepared_cached_recovery_owner(
        &self,
        preparation: &plurx_core::domain::MediaSessionPreparation,
        predecessor: &crate::media_sessions::RemoteStartRequest,
    ) -> Option<(crate::vodserve::ResponseOwner, bool)> {
        let route = self
            .store
            .media_session_route_by_incarnation(&preparation.incarnation_id)
            .await
            .ok()??;
        if !plurx_core::store::PreparedOutputSeal::matches_reservation(preparation, &route)
            || route.principal != predecessor.principal
            || preparation.principal != predecessor.principal
        {
            return None;
        }
        let successor =
            serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(&route.recipe_json)
                .ok()?;
        if !prepared_cached_recovery_recipe_matches(predecessor, &successor) {
            return None;
        }
        let output = successor.retained_output.as_ref()?;
        let user_id = route.principal.local_user_id()?;
        let seal = plurx_core::store::PreparedOutputSeal {
            incarnation_id: route.incarnation_id.clone(),
            session_id: route.session_id.clone(),
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
            retained_output: Some(serde_json::to_value(output).ok()?),
        };
        if !self.store.prepared_output_sealed(&seal).await.ok()? {
            return None;
        }
        let owner = self
            .vod
            .cached_prepared_reader_owner(&route.session_id, &route.incarnation_id, output)
            .await?;
        Some((
            owner,
            prepared_manual_auto_bootstrap(predecessor, &successor),
        ))
    }

    pub(crate) fn prepared_cached_recovery_is_live(
        &self,
        preparation: &plurx_core::domain::MediaSessionPreparation,
        owner: &crate::vodserve::ResponseOwner,
    ) -> Option<bool> {
        self.vod.cached_prepared_reader_is_live(
            &preparation.session_id,
            &preparation.incarnation_id,
            owner,
        )
    }

    /// Prime a genuinely new unpublished reservation and seal its private output
    /// choice before admitting a client. Missing pending markers are strict recovery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn vod_prepare_first_before<'a>(
        &'a self,
        recipe_json: &'a str,
        session_id: &'a str,
        user_id: i64,
        owner: PreparedVodOwner,
        adoption: SessionAdoptionToken,
        expected_predecessor_owner_epoch: Option<i64>,
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
                    || route.owner_node_id != owner.node_id
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
                // The epoch the preparation was admitted under, when the caller
                // knows it (local prime, or an origin that sent it). A takeover
                // since staging cannot commit this successor, so refuse before
                // attaching rather than encode for it. Absent (an older origin),
                // the predecessor's current epoch is all there is to fence on.
                let predecessor_owner_epoch =
                    expected_predecessor_owner_epoch.unwrap_or(predecessor.owner_epoch);
                if predecessor.owner_epoch != predecessor_owner_epoch {
                    return false;
                }
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

                // Derive purpose from the exact durable predecessor admitted by
                // the preparation ledger, on both local and remote first primes.
                // An Auto incumbent's voluntary proposals remain nonpreemptive.
                let bootstrap = serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(
                    &predecessor.recipe_json,
                )
                .is_ok_and(|previous| prepared_manual_auto_bootstrap(&previous, &current_recipe));
                let pending = response["prepared_output_capture_pending"] == true;
                let complete = response["prepared_output_capture_complete"] == true;
                // Older callers without an admitted epoch keep ordinary
                // recovery, but cannot acquire this new private authority.
                let processed_predecessor = (pending
                    && initial.incarnation_id == route.incarnation_id
                    && initial.principal == route.principal
                    && initial.request.playback_id == route.playback_id
                    && initial.request.request_id.as_deref()
                        == Some(route.incarnation_id.as_str())
                    && expected_predecessor_owner_epoch == Some(predecessor.owner_epoch)
                    && predecessor.owner_node_id == owner.node_id
                    && self.cache_location().map(|(_, node)| node) == Some(owner.node_id.as_str())
                    && predecessor.principal == route.principal
                    && predecessor.playback_id == route.playback_id
                    && predecessor.state == "active"
                    && predecessor.lease_expires_at_ms > now)
                    .then(|| StagedProcessedPredecessor {
                        session_id: predecessor.session_id.clone(),
                        incarnation_id: predecessor.incarnation_id.clone(),
                        owner_node_id: predecessor.owner_node_id.clone(),
                        owner_epoch: predecessor.owner_epoch,
                        successor_session_id: session_id.to_owned(),
                        successor_incarnation_id: route.incarnation_id.clone(),
                    });
                if !pending && !complete {
                    return self
                        .resurrect_vod_from_recipe_before(
                            &route.recipe_json,
                            session_id,
                            user_id,
                            adoption,
                            deadline,
                            VodRecipeAttachment::Recovery {
                                speculative: true,
                                bootstrap,
                            },
                        )
                        .await;
                }
                let existing_facts = self.vod.hls_facts(session_id).await;
                if let Some(facts) = existing_facts.as_ref() {
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
                let cleanup = FirstPreparationCleanup {
                    vod: self.vod.clone(),
                    session_id: session_id.to_owned(),
                    owner: Arc::new(std::sync::Mutex::new(
                        existing_facts
                            .as_ref()
                            .filter(|_| pending)
                            .map(|facts| facts.response_owner.clone()),
                    )),
                };
                self.hooks
                    .get()
                    .processed_preparation_boundary(session_id, "replay-owner")
                    .await;
                // A retry after an interrupted first attempt seals the existing actual
                // attachment, including None; it never upgrades/replaces that choice.
                if existing_facts.is_none()
                    && !self
                        .resurrect_vod_from_recipe_before(
                            &route.recipe_json,
                            session_id,
                            user_id,
                            adoption,
                            deadline,
                            if pending {
                                VodRecipeAttachment::FirstPreparation {
                                    owner: owner.clone(),
                                    bootstrap,
                                    processed_predecessor,
                                    attachment_owner: Arc::clone(&cleanup.owner),
                                }
                            } else {
                                VodRecipeAttachment::Recovery {
                                    speculative: true,
                                    bootstrap,
                                }
                            },
                        )
                        .await
                {
                    return false;
                }
                let Some(facts) = (match existing_facts {
                    Some(facts) => Some(facts),
                    None => self.vod.hls_facts(session_id).await,
                }) else {
                    return false;
                };
                self.hooks
                    .get()
                    .processed_preparation_boundary(session_id, "facts")
                    .await;
                // Replay retains the initially checked owner; a new attachment
                // installs its exact owner synchronously at registry commit.
                // Never acquire cleanup authority from a later by-id lookup.
                let captured_owner = cleanup
                    .owner
                    .lock()
                    .expect("first preparation owner")
                    .clone();
                let preparation_owner = captured_owner.as_ref().unwrap_or(&facts.response_owner);
                let Some(_owner_guard) = self
                    .vod
                    .lock_preparation_owner(session_id, preparation_owner, &route.incarnation_id)
                    .await
                else {
                    return false;
                };
                // Replayed first primes can already own an attachment. Refresh
                // its exact consumer purpose after checking the same owner.
                if !self
                    .vod
                    .mark_prepared_incarnation_for_owner(
                        session_id,
                        preparation_owner,
                        &route.incarnation_id,
                        bootstrap,
                    )
                    .await
                {
                    return false;
                }
                let output = preparation_owner.retained_output_facts();
                self.hooks
                    .get()
                    .processed_preparation_boundary(session_id, "before-seal")
                    .await;
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
                    predecessor_owner_epoch,
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
                trace_prepared_capture(&seal, Some(true), true);
                self.hooks
                    .get()
                    .processed_preparation_boundary(session_id, "sealed")
                    .await;
                cleanup
                    .owner
                    .lock()
                    .expect("first preparation owner")
                    .take();
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
            VodRecipeAttachment::Recovery {
                speculative,
                bootstrap: false,
            },
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
        let (speculative, first_owner, bootstrap, processed_predecessor, attachment_owner) =
            match purpose {
                VodRecipeAttachment::Recovery {
                    speculative,
                    bootstrap,
                } => (speculative, None, bootstrap, None, None),
                VodRecipeAttachment::FirstPreparation {
                    owner,
                    bootstrap,
                    processed_predecessor,
                    attachment_owner,
                } => (
                    true,
                    Some(owner),
                    bootstrap,
                    processed_predecessor,
                    Some(attachment_owner),
                ),
            };
        let first_capture = first_owner.is_some();
        // Recovery also needs an exact commit receipt: a by-id read after
        // create can observe an unrelated replacement before marking/enqueue.
        let attachment_owner =
            attachment_owner.unwrap_or_else(|| Arc::new(std::sync::Mutex::new(None)));
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
            let processed = remote
                .candidate_catalog
                .as_ref()
                .is_some_and(|catalog| catalog.candidate.processed_dv);
            let root_admission = if processed {
                let Some(predecessor) = processed_predecessor.as_ref() else {
                    return false;
                };
                match self
                    .restore_staged_processed_root(&mut remote, predecessor, deadline)
                    .await
                {
                    Ok(root) => Some(root),
                    Err(_) => return false,
                }
            } else {
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
                None
            };
            // Only the first unattached pending attempt evaluates current source
            // authority. Replay/recovery never refresh an immutable choice.
            let eligible =
                if let Some(owner) = first_owner.as_ref().filter(|_| root_admission.is_none()) {
                    if let Some(state) = owner
                        .source_authority
                        .as_ref()
                        .filter(|state| state.node_id == owner.node_id)
                    {
                        crate::media_pool::prepared_vod_owner(
                            state,
                            &remote,
                            tokio::time::Instant::from_std(deadline),
                        )
                        .await
                    } else {
                        None
                    }
                } else {
                    None
                };
            let mut req = remote.request;
            let fresh_reuse = first_owner.as_ref().is_some_and(|owner| {
                req.candidate_context.as_mut().is_some_and(|context| {
                    Self::bind_prepared_candidate_owner(context, &owner.node_id, eligible.as_ref())
                })
            });
            if req.presentation != Presentation::Vod {
                return false;
            }
            let Ok(Some(settings)) = self.vod_settings(&req).await else {
                return false;
            };
            let mut file = if root_admission.is_some() {
                let Some(snapshot) = req
                    .candidate_context
                    .as_ref()
                    .and_then(|context| context.planning_snapshot.as_ref())
                else {
                    return false;
                };
                snapshot.file.clone()
            } else {
                let Ok(Some(file)) = self.store.get_file(req.file_id).await else {
                    return false;
                };
                file
            };
            file.audio_offset_ms = if file.audio_streams.is_empty() {
                0
            } else {
                req.audio_offset_ms.clamp(-15_000, 15_000)
            };
            let encoding = match root_admission.as_ref() {
                Some(root) => Some(Arc::clone(&root.encoding)),
                None => match self.prepare_vod_encoding(&req, &file).await {
                    Ok(encoding) => encoding,
                    Err(_) => return false,
                },
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
            let measured_candidate = if first_capture && root_admission.is_none() {
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
                if root_admission.is_none() {
                    if let Some(encoding) = encoding.as_ref() {
                        encoding.mark_speculative();
                    }
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
            // First local preparations create the same complete output as a
            // normal start. Give the existing owned publication worker the
            // identical work after actual attachment; recovery never queues.
            let output_enqueue = first_owner
                .as_ref()
                .filter(|_| root_admission.is_none())
                .map(|owner| {
                    (
                        OutputEnqueue {
                            request: req.clone(),
                            file: file.clone(),
                            settings: settings.clone(),
                            encoding: encoding.clone(),
                            session_id: session_id.to_owned(),
                            queued_at: Instant::now(),
                        },
                        owner.clone(),
                    )
                });
            match self
                .vod
                .try_create_before_release(
                    crate::vodserve::VodRecipeRequest {
                        root_admission,
                        companion,
                        measured_candidate,
                        retained_capture: if fresh_reuse {
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
                    )
                    .with_attachment_owner(Some(attachment_owner.as_ref())),
                )
                .await
            {
                Ok(_) => {
                    self.hooks
                        .get()
                        .processed_preparation_boundary(session_id, "attached")
                        .await;
                    let Some(captured_owner) = attachment_owner
                        .lock()
                        .expect("first preparation owner")
                        .clone()
                    else {
                        return false;
                    };
                    if speculative {
                        if !self
                            .vod
                            .mark_prepared_incarnation_for_owner(
                                session_id,
                                &captured_owner,
                                &remote.incarnation_id,
                                bootstrap,
                            )
                            .await
                        {
                            return false;
                        }
                    } else if !self
                        .vod
                        .response_owner_is_live(session_id, &captured_owner)
                        .await
                    {
                        return false;
                    }
                    if let Some((work, owner)) = output_enqueue {
                        if !release_gate.released.load(Acquire) {
                            self.hand_off_prepared_output_enqueue(work, &owner, &captured_owner);
                        }
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

    /// A first prepared attachment already restored the exact source/caps/
    /// recipe context. Its actual local attachment owner may publish work even
    /// when an unrelated catalog sweep did not finish. Fresh retained reuse
    /// remains separately fenced by `bind_prepared_candidate_owner`.
    fn hand_off_prepared_output_enqueue(
        &self,
        mut work: OutputEnqueue,
        owner: &PreparedVodOwner,
        response_owner: &crate::vodserve::ResponseOwner,
    ) {
        if work.request.presentation != Presentation::Vod
            || work.request.continuous_media.is_some()
            || !work
                .settings
                .output_preparation
                .admits(work.encoding.is_some())
            || owner
                .source_authority
                .as_ref()
                .is_none_or(|state| state.node_id != owner.node_id)
            || self.cache_location().map(|(_, node)| node) != Some(owner.node_id.as_str())
        {
            return;
        }
        let Some(context) = work.request.candidate_context.as_mut() else {
            return;
        };
        if context.retained_output.is_some()
            || context.canonical_caps.is_none()
            || context.planning_binding.is_none()
            || context.candidate_id != context.selected_candidate.id
            || context.recipe_digest != context.selected_candidate.recipe_digest
            || !context.selected_candidate.identity_matches()
            || !context.selected_candidate.decoder_compatible
            || context.selected_candidate.width == 0
            || context.selected_candidate.height == 0
            || context.selected_candidate.target_height == 0
            || context
                .owner_node_id
                .as_deref()
                .is_some_and(|node| node != owner.node_id)
        {
            return;
        }
        // Queue-only owner binding: byte reuse remains untouched. The worker
        // independently restores current source/settings and the exact recipe
        // before it constructs a durable encoded-output job.
        context.owner_node_id = Some(owner.node_id.clone());
        self.hand_off_output_enqueue(work, Some(response_owner));
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
mod retained_recovery_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "processed_reuse_tests.rs"]
mod processed_reuse_tests;

#[cfg(test)]
mod display_target_policy_tests {
    use super::*;
    use plurx_core::{
        playback::caps::{DisplayCaps, DisplayPeakSource},
        scan::dv_title::*,
    };
    #[test]
    fn explicit_sdr_capability_uses_100_without_converting_unknown_hdr_peak_to_sdr() {
        use plurx_core::playback::caps::DisplayCaps;
        use plurx_core::scan::dv_title::{DvTitleStats, TitleConstant, SCHEMA};
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 2,
            valid_l1_frames: 2,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(0),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        // Use the existing probe writer's actual key, not a synthetic shortcut.
        let document = serde_json::json!({PROBE_FIELD:stats}).to_string();
        let display = DisplayCaps {
            hdr: false,
            dolby_vision: false,
            ..Default::default()
        };
        let (target, note) =
            TranscodeManager::display_target_policy(true, Some(&display), Some(&document))
                .expect("SDR");
        assert!(target.expect("SDR target").is_sdr());
        assert!(note.expect("note").contains("gamma 2.4"));
        for display in [
            None,
            Some(DisplayCaps {
                hdr: true,
                ..Default::default()
            }),
        ] {
            assert!(TranscodeManager::display_target_policy(
                true,
                display.as_ref(),
                Some(&document)
            )
            .expect("unknown")
            .0
            .is_none());
        }
        assert!(
            TranscodeManager::display_target_policy(true, Some(&display), None)
                .expect("missing scan")
                .0
                .is_none()
        );
        assert!(
            TranscodeManager::display_target_policy(false, Some(&display), Some(&document))
                .expect("disabled")
                .0
                .is_none()
        );
    }

    #[test]
    fn source_mastering_resolves_before_the_live_target_is_frozen() {
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 1,
            valid_l1_frames: 1,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Consistent(MasteringPrimaries {
                index: 0,
                custom_xy: None,
            }),
        };
        let record = serde_json::json!({"side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000","green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000","white_point_x":"15635/50000","white_point_y":"16450/50000",
            "max_luminance":"4000/1","min_luminance":"17/10000"});
        let mut unrelated = record.clone();
        unrelated["min_luminance"] = "999/10000".into();
        let document = serde_json::json!({PROBE_FIELD:stats,"streams":[
            {"index":0,"codec_type":"video","disposition":{"attached_pic":1},"side_data_list":[unrelated.clone()]},
            {"index":1,"codec_type":"audio","side_data_list":[unrelated]},
            {"index":7,"codec_type":"video","side_data_list":[record]}]});
        let display = DisplayCaps {
            hdr: true,
            max_nits: Some(1600),
            ..Default::default()
        };
        let select = |document: &serde_json::Value| {
            TranscodeManager::display_target_policy(
                true,
                Some(&display),
                Some(&document.to_string()),
            )
        };
        let frozen = select(&document)
            .expect("selected source mastering")
            .0
            .expect("target");
        assert_eq!(frozen.endpoint_args(), ["17", "4000"]);
        assert_eq!(
            serde_json::to_value(frozen).expect("frozen identity")["mastering"]["min_1e4_nits"],
            17
        );
        let missing = serde_json::json!({PROBE_FIELD:stats});
        let fallback = select(&missing)
            .expect("known L9 and renderer floor")
            .0
            .expect("target");
        assert_eq!(fallback.endpoint_args(), ["50", "4000"]);
        assert_ne!(
            serde_json::to_value(frozen).expect("source identity"),
            serde_json::to_value(fallback).expect("fallback identity")
        );
        let mut unknown = missing.clone();
        unknown[PROBE_FIELD]["mastering_primaries"] =
            serde_json::to_value(TitleConstant::<MasteringPrimaries>::Unknown).expect("unknown");
        assert!(select(&unknown).is_err());
        for field in ["mastering_primaries", "mastering_luminance"] {
            let mut contradictory = document.clone();
            contradictory[PROBE_FIELD][field] = serde_json::json!({"state":"inconsistent"});
            assert!(select(&contradictory).is_err(), "{field}");
        }
        let mut unsupported = document;
        unsupported["streams"][2]["side_data_list"][0]["min_luminance"] = "10001/10000".into();
        assert!(select(&unsupported).is_err());
    }

    #[test]
    fn sdr_frame_minimum_freezes_floor_without_hdr_primaries_or_title_override() {
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 3,
            valid_l1_frames: 3,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let display = DisplayCaps::default();
        let document = serde_json::json!({"frames":[{"stream_index":7,"side_data_list":[{"side_data_type":"Mastering display metadata","min_luminance":"1/10000"}]}]});
        let minimum = plurx_core::transcode::dv_processing::source_mastering_minimum_from_frames(
            &serde_json::Value::Null,
            &document,
            7,
        )
        .expect("validated minimum-only frame fact");
        let probe = serde_json::json!({PROBE_FIELD:stats}).to_string();
        let target = TranscodeManager::display_target_policy_with_source(
            true,
            Some(&display),
            Some(&probe),
            None,
            minimum,
        )
        .expect("policy")
        .0
        .expect("target");
        assert!(target.is_sdr());
        assert_eq!(target.resolved_sdr_floor().expect("frozen floor"), 1);
        assert!(
            target.resolve_sdr_floor(Some(50)).is_err(),
            "changed held floor cannot reuse frozen policy"
        );
        let absent = TranscodeManager::display_target_policy_with_source(
            true,
            Some(&display),
            Some(&probe),
            None,
            None,
        )
        .expect("absent source minimum")
        .0
        .expect("target");
        assert_eq!(
            absent.resolved_sdr_floor().expect("established default"),
            50
        );
        let known = DvTitleStats {
            mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            }),
            ..stats
        };
        let probe = serde_json::json!({PROBE_FIELD:known}).to_string();
        let target = TranscodeManager::display_target_policy_with_source(
            true,
            Some(&display),
            Some(&probe),
            None,
            minimum,
        )
        .expect("title floor retained")
        .0
        .expect("target");
        assert_eq!(target.resolved_sdr_floor().expect("known floor"), 50);
        let inconsistent = DvTitleStats {
            mastering_luminance: TitleConstant::Inconsistent,
            ..stats
        };
        let probe = serde_json::json!({PROBE_FIELD:inconsistent}).to_string();
        assert!(
            TranscodeManager::display_target_policy_with_source(
                true,
                Some(&display),
                Some(&probe),
                None,
                minimum
            )
            .is_err(),
            "source minimum never waives inconsistent title L6"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn master_and_known_l6_sdr_policy_do_not_open_missing_source() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("policy setup");
        let manager = TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let library = store
            .create_library(&NewLibrary {
                name: "policy source".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "policy source".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let missing = root.path().join("source-does-not-exist.mkv");
        let file = store
            .upsert_file(
                item,
                missing.to_str().expect("path"),
                1,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("file");
        let file = store.get_file(file).await.expect("lookup").expect("file");
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 3,
            valid_l1_frames: 3,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_primaries: TitleConstant::Unknown,
            mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            }),
        };
        let display = DisplayCaps::default();
        let probe = |stats| {
            serde_json::json!({PROBE_FIELD:stats,"streams":[{"index":7,"codec_type":"video","side_data_list":[{"side_data_type":"Mastering display metadata","min_luminance":"malformed but irrelevant with known L6"}]}]}).to_string()
        };
        let document = probe(stats);
        let target = manager
            .display_target_policy_for_source(true, Some(&display), &file, Some(&document))
            .await
            .expect("known title floor must not open source")
            .0
            .expect("target");
        assert_eq!(target.resolved_sdr_floor().expect("floor"), 50);
        assert_eq!(
            manager
                .display_target_policy_for_source(false, Some(&display), &file, Some(&document))
                .await
                .expect("disabled/master must not open source"),
            (None, None)
        );
        let document = probe(DvTitleStats {
            complete: false,
            ..stats
        });
        assert!(manager
            .display_target_policy_for_source(true, Some(&display), &file, Some(&document))
            .await
            .expect("unavailable-title/master must not open source")
            .0
            .is_none());
        let document = probe(DvTitleStats {
            mastering_luminance: TitleConstant::Inconsistent,
            ..stats
        });
        assert!(manager
            .display_target_policy_for_source(true, Some(&display), &file, Some(&document))
            .await
            .expect_err("inconsistent title refuses before source read")
            .contains("inconsistent"));
        assert!(!missing.exists(), "no fixture source was materialized");
    }

    #[test]
    fn frame_mastering_fallback_freezes_hdr_policy_without_inventing_title_l9() {
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 3,
            valid_l1_frames: 3,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        };
        let display = DisplayCaps {
            hdr: true,
            max_nits: Some(1000),
            peak_source: Some(DisplayPeakSource::Platform),
            ..Default::default()
        };
        let frame = serde_json::json!({"stream_index":7,"side_data_list":[{
            "side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000","green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000","white_point_x":"15635/50000","white_point_y":"16450/50000",
            "min_luminance":"1/10000","max_luminance":"10000000/10000"}]});
        let source = plurx_core::transcode::dv_processing::source_mastering_from_frames(
            &serde_json::Value::Null,
            &serde_json::json!({"frames":[frame]}),
            7,
        )
        .expect("validated frame metadata");
        let probe = serde_json::json!({PROBE_FIELD:stats}).to_string();
        assert!(
            TranscodeManager::display_target_policy(true, Some(&display), Some(&probe)).is_err(),
            "unknown L9 still requires real source metadata"
        );
        let target = TranscodeManager::display_target_policy_with_source(
            true,
            Some(&display),
            Some(&probe),
            source,
            None,
        )
        .expect("source-backed policy")
        .0
        .expect("target");
        assert_eq!(target.mastering().min_1e4_nits, 1);
        assert_eq!(
            target.output_metadata().expect("frozen declarations"),
            plurx_core::fmp4::Hdr10OutputMetadata::new(
                [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450],
                1000,
                1,
                1000,
            )
            .expect("exact expected source-backed declarations")
        );
        let known_floor = DvTitleStats {
            mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            }),
            ..stats
        };
        let probe = serde_json::json!({PROBE_FIELD:known_floor}).to_string();
        let target = TranscodeManager::display_target_policy_with_source(
            true,
            Some(&display),
            Some(&probe),
            source,
            None,
        )
        .expect("title floor retained")
        .0
        .expect("target");
        assert_eq!(target.mastering().min_1e4_nits, 50);
        let inconsistent = DvTitleStats {
            mastering_primaries: TitleConstant::Inconsistent,
            ..stats
        };
        let probe = serde_json::json!({PROBE_FIELD:inconsistent}).to_string();
        assert!(
            TranscodeManager::display_target_policy_with_source(
                true,
                Some(&display),
                Some(&probe),
                source,
                None
            )
            .is_err(),
            "frame metadata never waives inconsistent title facts"
        );
    }

    #[test]
    fn display_target_declines_are_named_and_elision_is_master() {
        let stats = DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 2,
            valid_l1_frames: 2,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Consistent(MasteringPrimaries {
                index: 0,
                custom_xy: None,
            }),
        };
        let probe = |stats| serde_json::json!({PROBE_FIELD:stats}).to_string();
        let high = probe(stats);
        let display = DisplayCaps {
            hdr: true,
            max_nits: Some(1999),
            peak_source: Some(DisplayPeakSource::Platform),
            ..Default::default()
        };
        let select = |enabled, display, probe| {
            TranscodeManager::display_target_policy(enabled, display, probe).expect("policy")
        };
        assert_eq!(
            select(false, Some(&display), Some(high.as_str())),
            (None, None)
        );
        let (target, note) = select(true, Some(&display), Some(high.as_str()));
        assert_eq!(target.expect("target").nits(), 1600);
        assert!(note.expect("note").contains("rendered live"));
        let viewer = DisplayCaps {
            peak_source: Some(DisplayPeakSource::Viewer),
            ..display.clone()
        };
        assert_eq!(select(true, Some(&viewer), Some(high.as_str())).0, target);
        for missing in [None, Some("{}"), Some("{broken")] {
            let (target, note) = select(true, Some(&display), missing);
            assert!(target.is_none());
            assert!(note.expect("decline").contains("scan unavailable"));
        }
        let incomplete = probe(DvTitleStats {
            complete: false,
            ..stats
        });
        let (target, note) = select(true, Some(&display), Some(incomplete.as_str()));
        assert!(target.is_none());
        assert!(note.is_some());
        let fits = probe(DvTitleStats {
            max_l1_pq: Some(0),
            ..stats
        });
        let (target, note) = select(true, Some(&display), Some(fits.as_str()));
        assert!(target.is_none());
        assert!(note.expect("elision").contains("already fits a 1600-nit"));
        let (target, note) = select(true, None, Some(high.as_str()));
        assert!(target.is_none());
        assert!(note.expect("unknown").contains("master grade"));
    }
}
