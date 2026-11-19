use super::*;

impl VodServe {
    /// An accepted control snapshot, never the diagnostic capability string.
    pub(crate) async fn client_platform_for_session(
        &self,
        session_id: &str,
    ) -> Option<crate::playback_control::ClientPlatform> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let platform = session.control.lock().ok()?.client_platform();
        platform
    }
}

impl VodServe {
    #[cfg(test)]
    pub(crate) async fn source_control_observation_for_test(
        &self,
        session_id: &str,
    ) -> Option<(
        Option<crate::playback_control::PlaybackDemandSnapshot>,
        Instant,
    )> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let touched = *session.last_touch.lock().expect("touch lock");
        Some((session.last_control_snapshot.clone(), touched))
    }

    /// Ask `session_id`'s own incumbent encoded rendition to give its encoder
    /// permit to this viewer's processed upgrade trial, through the same
    /// predecessor-yield handoff a staged successor uses.
    ///
    /// `None`, and nothing changed, unless every guard holds: the session is
    /// live here, its rendition is encoded and holds a permit, only that
    /// viewer's playback reads it, the trial's `wanted` would fit once that
    /// permit returned, and the published runway already covers `reserve_ms`.
    /// Another viewer's work is never asked: their renditions are not this
    /// session's.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn claim_incumbent_permit(
        &self,
        session_id: &str,
        incarnation: &str,
        wanted: crate::admission::TranscodeResourceEstimate,
        hardware_limit: usize,
        software_budget: usize,
        reserve_ms: i64,
    ) -> Option<IncumbentPermitClaim> {
        let (rendition, playback_id) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            (
                Arc::clone(session.live_rendition()?),
                session.playback_id.clone(),
            )
        };
        let encoding = rendition.recipe.encoding.as_ref()?;
        if rendition.closed.load(Relaxed)
            || !crate::prodexec::holds_permit(rendition.slot.belief().await, true)
            || !encoding.admissions.live_fits_after_release(
                hardware_limit,
                software_budget,
                &wanted,
                &encoding.resources(),
            )
        {
            return None;
        }
        let claim = Arc::new(TrialClaim {
            playback_id,
            incarnation: incarnation.to_owned(),
            reserve_ms,
            admitted: Arc::new(AtomicBool::new(false)),
            token: StdMutex::new(None),
            ready: Notify::new(),
            closed: AtomicBool::new(false),
            staged: AtomicBool::new(false),
        });
        if !trial_claim_bound(&self.shared, &rendition, &claim).await
            || !rendition.request_trial_handoff(&claim, wanted)
        {
            return None;
        }
        rendition.kick();
        Some(IncumbentPermitClaim {
            claim,
            incumbent: Arc::downgrade(&rendition),
            admissions: encoding.admissions.clone(),
        })
    }

    /// Where the viewer of `session_id` is now on the source timeline: the
    /// latest newly accepted control demand, advanced by that observation's
    /// age the way the delivery driver reads it
    /// ([`crate::playback_control::rolling_estimated_position_ms`]). `None`
    /// before any exchange was accepted, or for a session this engine does
    /// not hold.
    pub(crate) async fn accepted_playhead_estimate_ms(&self, session_id: &str) -> Option<i64> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let demand = session.last_control_snapshot.as_ref()?;
        let observed = session.control_observed_at?;
        Some(crate::playback_control::rolling_estimated_position_ms(
            demand,
            observed.elapsed(),
        ))
    }

    /// Apply one fenced control exchange without conflating a replay or stale
    /// request with a media-object touch. Only a newly accepted non-terminal
    /// sequence moves the existing five-minute VOD activity clock. A fresh
    /// `demand=end` instead tombstones the attachment under this same lifecycle
    /// gate and retains its exact response for an idempotent retry.
    #[cfg(test)]
    pub(crate) async fn control(
        &self,
        control: crate::playback_control::LocalControlRequest<'_>,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        self.control_with_terminal(control, i64::MAX, None, None)
            .await
    }

    pub(crate) async fn control_with_terminal(
        &self,
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
        self.control_with_authority(
            control,
            deadline_unix_ms,
            terminal_committer,
            preparation_admission,
            None,
        )
        .await
    }

    pub(crate) async fn control_source(
        &self,
        authority: &mut crate::transcode::source_actor::control::SourceControlAuthority<'_>,
        control: crate::playback_control::LocalControlRequest<'_>,
        deadline_unix_ms: i64,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        self.control_with_authority(control, deadline_unix_ms, None, None, Some(authority))
            .await
    }

    async fn control_with_authority(
        &self,
        control: crate::playback_control::LocalControlRequest<'_>,
        deadline_unix_ms: i64,
        terminal_committer: Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
        preparation_admission: Option<
            Arc<dyn crate::playback_control::PreparationSettlementAdmission>,
        >,
        mut source: Option<
            &mut crate::transcode::source_actor::control::SourceControlAuthority<'_>,
        >,
    ) -> Option<
        Result<
            crate::playback_control::LocalControlResult,
            crate::playback_control::ControlStateError,
        >,
    > {
        // Resolve registry ownership before durable I/O, then keep this
        // session's gate held from the authority read through sequence
        // acceptance and any terminal detach. `end` and idle reap take the
        // same per-session gate, so neither can cross the linearization point.
        let lifecycle = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(control.session_id)?;
            Arc::clone(&session.lifecycle)
        };
        let lifecycle_guard = lifecycle.lock().await;

        // A client may lose the successful terminal response. The tombstone
        // closes every mutation except replay of the exact accepted identity
        // and sequence; no Store read is needed to recover that immutable
        // owner-local result.
        let terminal_replay = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(control.session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) {
                return None;
            }
            if session.tombstone.is_some() {
                if session.control_end_snapshot.as_ref() != Some(&control.snapshot) {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                }
                let replay = session.control.lock().expect("control lock").replay_exact(
                    control.generation,
                    control.owner_epoch,
                    control.client_instance_id,
                    control.sequence,
                    control.snapshot.platform(),
                );
                let Some((_, accepted_sequence, action, platform, action_suppressed)) = replay
                else {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                };
                let Some(stored_result) = session.control_end.as_ref() else {
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                };
                if stored_result
                    .terminal_commit
                    .as_ref()
                    .is_some_and(|commit| commit.is_expired())
                {
                    // Preserve only the owner/sequence tombstone after the
                    // bounded response-recovery window. The retained Store
                    // handle, response, and retry closure are no longer useful.
                    session.control_end = None;
                    return Some(Err(
                        crate::playback_control::ControlStateError::SessionEnded,
                    ));
                }
                #[cfg(target_os = "linux")]
                let replay_slot = if let Some(slot) = stored_result.finite_client_slot.as_ref() {
                    let Some(bytes) = stored_result.requested_clone_heap_bytes() else {
                        return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                    };
                    match slot.reserve_related(bytes) {
                        Ok(copy) => Some(copy),
                        Err(_) => {
                            return Some(Err(
                                crate::playback_control::ControlStateError::Unavailable,
                            ))
                        }
                    }
                } else {
                    None
                };
                let mut result = stored_result.clone();
                #[cfg(target_os = "linux")]
                if let Some(slot) = replay_slot {
                    result.finite_client_slot = Some(slot);
                }
                debug_assert_eq!(result.accepted_sequence, accepted_sequence);
                debug_assert_eq!(result.action, action);
                debug_assert_eq!(result.platform, platform);
                debug_assert_eq!(result.action_suppressed, action_suppressed);
                result.disposition = crate::playback_control::ControlDisposition::Replay;
                // One viewer action is one measurement: a stored result
                // carries the observation of the exchange that produced it,
                // and replaying it must not replay that.
                result.selection = crate::playback_control::SelectionObservation::default();
                let Some(cleanup) = session.terminal_cleanup.as_ref().map(Arc::clone) else {
                    return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                };
                Some((result, cleanup))
            } else {
                None
            }
        };
        if let Some((result, cleanup)) = terminal_replay {
            self.shared.hooks.get().before_terminal_replay_join().await;
            cleanup.wait().await;
            if let Some(commit) = &result.terminal_commit {
                // Reader detach is the visibility fence for VOD End. Every
                // retry, including one replacing a cancelled original HTTP
                // waiter, must cross the session-owned cleanup first.
                commit.retry();
            }
            return Some(Ok(result));
        }

        if let Some(authority) = source.as_deref_mut() {
            if let Err(error) = authority.refresh(&control).await {
                return Some(Err(error));
            }
        } else if let Err(error) = crate::playback_control::verify_authority(
            self.shared.store.as_ref(),
            control.session_id,
            control.generation,
            control.owner_node_id,
            control.owner_epoch,
        )
        .await
        {
            return Some(Err(error));
        }

        let ending_status =
            if control.snapshot.demand == crate::playback_control::PlaybackDemand::End {
                self.status(control.session_id).await
            } else {
                None
            };
        enum AppliedControl {
            End {
                result: crate::playback_control::LocalControlResult,
                terminal_commit: Option<Box<crate::playback_control::TerminalCommitReceipt>>,
                cleanup: Arc<TerminalCleanup>,
                rendition: Arc<Rendition>,
                file_id: i64,
                height: i64,
                kind: SessionKind,
            },
            Live {
                disposition: crate::playback_control::ControlDisposition,
                accepted_sequence: u64,
                action: crate::playback_control::ControlAction,
                action_suppressed: bool,
                preparation_directive: Option<Box<crate::playback_control::PreparationDirective>>,
                platform: crate::playback_control::ClientPlatform,
                selection: crate::playback_control::SelectionObservation,
                lease_expires_at_unix_ms: i64,
                marker_prewarm: Option<Box<MarkerPrewarmControl>>,
                #[cfg(target_os = "linux")]
                finite_client_slot: Option<
                    Arc<plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientSlot>,
                >,
                pending_companion: Option<Box<(String, u64)>>,
            },
        }
        let outcome = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(control.session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle) || session.tombstone.is_some() {
                return None;
            }
            // Acquire the async reader lock before the sequence fence. From
            // acceptance through anchor publication there is then no await:
            // cancelling an HTTP response cannot leave an accepted command
            // whose exact replay silently skips its playback destination.
            let control_rendition = session.live_rendition().map(Arc::clone)?;
            let mut readers = control_rendition.readers.lock().await;
            if control.snapshot.demand != crate::playback_control::PlaybackDemand::End
                && !readers.contains_key(control.session_id)
            {
                return Some(Err(crate::playback_control::ControlStateError::Unavailable));
            }
            if crate::media_sessions::unix_ms() >= deadline_unix_ms {
                return Some(Err(crate::playback_control::ControlStateError::Unavailable));
            }
            // Private readers share parent control authority, but use their
            // own sample clocks. Lock every graph before accepting a sequence
            // so cancellation cannot publish a command to only some children.
            let mut child_groups: Vec<(Arc<Rendition>, Vec<String>)> = Vec::new();
            let mut root_children = Vec::new();
            if control.snapshot.demand != crate::playback_control::PlaybackDemand::End {
                for child in &session.children {
                    if Arc::ptr_eq(&child.rendition, &control_rendition) {
                        if !readers.contains_key(&child.reader_id) {
                            return Some(Err(
                                crate::playback_control::ControlStateError::Unavailable,
                            ));
                        }
                        root_children.push(child.reader_id.clone());
                    } else if let Some((_, ids)) = child_groups
                        .iter_mut()
                        .find(|(rendition, _)| Arc::ptr_eq(rendition, &child.rendition))
                    {
                        ids.push(child.reader_id.clone());
                    } else {
                        child_groups
                            .push((Arc::clone(&child.rendition), vec![child.reader_id.clone()]));
                    }
                }
            }
            child_groups.sort_by(|(left, _), (right, _)| left.key.cmp(&right.key));
            let mut child_readers = Vec::with_capacity(child_groups.len());
            for (rendition, ids) in &child_groups {
                let guard = rendition.readers.lock().await;
                if ids.iter().any(|id| !guard.contains_key(id)) {
                    return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                }
                child_readers.push(guard);
            }
            if let Some(authority) = source.as_deref() {
                authority.before_acceptance().await;
                if authority.validate().is_err()
                    || session.supersession_user
                        != authority.assignment().binding().principal().owner_key()
                    || !control_rendition
                        .source_owners
                        .contains_live_assignment(authority.assignment())
                {
                    return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                }
            }
            // Acceptance and M6's selection gate are one lock scope. They are
            // two reads of the same fence, and taking the lock twice would let
            // another exchange land between them and be measured against a
            // selection this one had already replaced.
            // Build finite control copies before publishing the sequence. The
            // detached fence step does not touch the live observation tokens;
            // a refused budget therefore preserves the previous sequence.
            let mut preflight_session_snapshot = None;
            let mut preflight_marker_payload = None;
            let (accepted, selection, preparation_directive) = {
                let mut fence = session.control.lock().expect("control lock");
                #[cfg(target_os = "linux")]
                let finite_outcome = if let Some(lease) = &control_rendition.finite_client_lease {
                    // Reserve the peak for all input and detached-state clones
                    // before ControlAcceptance::observed or the step can allocate.
                    // It stays live alongside the eventual retained-state slot
                    // until adoption and the post-reserve copies have ended.
                    let transient_required = fence.finite_prepare_transient_bytes(
                        control.generation,
                        &control.snapshot,
                        &control.prepared_successor,
                    );
                    let Some(transient_required) = transient_required else {
                        return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                    };
                    let _transient_slot = match lease.reserve(transient_required) {
                        Ok(slot) => slot,
                        Err(_) => {
                            return Some(Err(
                                crate::playback_control::ControlStateError::Unavailable,
                            ));
                        }
                    };
                    let prepared = fence.prepare_finite_control(
                        crate::playback_control::ControlRequestView {
                            generation: control.generation,
                            owner_epoch: control.owner_epoch,
                            client_instance_id: control.client_instance_id,
                            sequence: control.sequence,
                            acceptance: crate::playback_control::ControlAcceptance::observed(
                                control.snapshot.platform(),
                                &control.prepared_successor,
                                control.snapshot.acknowledgement.as_ref(),
                                control.snapshot.request_fingerprint.as_deref(),
                                &control.snapshot.selection,
                                control.snapshot.capabilities.as_ref(),
                            ),
                        },
                        &control.snapshot,
                    );
                    let prepared_accepted = prepared.accepted();
                    let marker_payload_needed = prepared_accepted
                        && source.is_none()
                        && control.snapshot.demand != crate::playback_control::PlaybackDemand::End;
                    let required = (|| -> Option<usize> {
                        let mut bytes = prepared.requested_heap_bytes()?;
                        // One fixed-capacity JSON body remains live with this
                        // exchange until the final HTTP Bytes owner drops.
                        if prepared.returns_result() {
                            bytes =
                                bytes.checked_add(crate::playback_control::MAX_RESPONSE_BYTES)?;
                        }
                        if prepared_accepted {
                            bytes = bytes.checked_add(control.snapshot.requested_heap_bytes()?)?;
                        }
                        if marker_payload_needed {
                            bytes = bytes.checked_add(control.snapshot.requested_heap_bytes()?)?;
                            bytes = bytes.checked_add(
                                session
                                    .marker_destinations
                                    .capacity()
                                    .checked_mul(std::mem::size_of::<MarkerDestination>())?,
                            )?;
                        }
                        if prepared_accepted
                            && control.snapshot.demand
                                == crate::playback_control::PlaybackDemand::End
                        {
                            let committer = terminal_committer.as_ref()?;
                            bytes = bytes.checked_add(committer.requested_heap_bytes()?)?;
                            let status = ending_status.as_ref()?;
                            let status_copy =
                                std::mem::size_of::<crate::vodserve::VodSessionInfo>()
                                    .checked_add(status.requested_heap_bytes()?)?;
                            bytes = bytes.checked_add(status_copy.checked_mul(2)?)?;
                            bytes = bytes.checked_add(prepared.external_payload_bytes()?)?;
                            // The original cleanup task owns this exact id
                            // after Session detaches and may outlive it.
                            bytes = bytes.checked_add(control.session_id.len())?;
                            bytes = bytes.checked_add(
                                std::mem::size_of::<TerminalCleanup>()
                                    .checked_add(2 * std::mem::size_of::<usize>())?,
                            )?;
                            bytes = bytes.checked_add(
                                std::mem::size_of::<crate::playback_control::LocalControlResult>()
                                    .checked_mul(2)?,
                            )?;
                        }
                        Some(bytes)
                    })();
                    let Some(required) = required else {
                        return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                    };
                    let slot = match lease.reserve(required) {
                        Ok(slot) => crate::vodserve::client_state::ClientSlot(slot),
                        Err(_) => {
                            return Some(Err(
                                crate::playback_control::ControlStateError::Unavailable,
                            ));
                        }
                    };
                    if slot.validate_actual(required).is_err() {
                        return Some(Err(crate::playback_control::ControlStateError::Unavailable));
                    }
                    let outcome = prepared.adopt(&mut fence);
                    if prepared_accepted {
                        preflight_session_snapshot = Some(control.snapshot.clone());
                        if marker_payload_needed {
                            preflight_marker_payload = Some((
                                control.snapshot.clone(),
                                session.marker_destinations.clone(),
                            ));
                        }
                    }
                    session.finite_control_slot = Some(slot);
                    Some(outcome)
                } else {
                    None
                };
                #[cfg(not(target_os = "linux"))]
                let finite_outcome: Option<_> = None;
                if let Some(outcome) = finite_outcome {
                    outcome
                } else {
                    let accepted = fence.accept(
                        control.generation,
                        control.owner_epoch,
                        control.client_instance_id,
                        control.sequence,
                        crate::playback_control::ControlAcceptance::observed(
                            control.snapshot.platform(),
                            &control.prepared_successor,
                            control.snapshot.acknowledgement.as_ref(),
                            control.snapshot.request_fingerprint.as_deref(),
                            &control.snapshot.selection,
                            control.snapshot.capabilities.as_ref(),
                        ),
                    );
                    let observation = if matches!(
                        &accepted,
                        Ok((crate::playback_control::ControlDisposition::Accepted, ..))
                    ) {
                        fence.observe(
                            &control.snapshot.selection,
                            control.snapshot.capabilities.as_ref(),
                        )
                    } else {
                        crate::playback_control::SelectionObservation::default()
                    };
                    let preparation_directive = fence.preparation_directive();
                    (accepted, observation, preparation_directive)
                }
            };
            let (disposition, accepted_sequence, action, platform, action_suppressed) =
                match accepted {
                    Ok(outcome) => outcome,
                    Err(error) => return Some(Err(error)),
                };
            let mut pending_companion = None;
            if disposition == crate::playback_control::ControlDisposition::Accepted
                && control.snapshot.demand != crate::playback_control::PlaybackDemand::End
            {
                // The parent's accepted selection can direct its immutable
                // companion before that child's init exists in the family.
                // A height-only selection is usable only if exactly one of
                // this parent's candidate-bound video children names it.
                let selected_companion = session
                    .children
                    .iter()
                    .find(|child| {
                        if !child.controlled || child.candidate_id.is_none() {
                            return false;
                        }
                        let height = child
                            .rendition
                            .recipe
                            .encoding
                            .as_ref()
                            .map(|encoding| encoding.options.target_height);
                        match control.snapshot.selection.quality {
                            crate::playback_control::QualitySelection::Auto {
                                candidate_id: Some(id),
                                height: named,
                            } => {
                                child.candidate_id == Some(id)
                                    && named.is_none_or(|named| Some(named) == height)
                            }
                            crate::playback_control::QualitySelection::Manual { height: named }
                            | crate::playback_control::QualitySelection::Auto {
                                height: Some(named),
                                candidate_id: None,
                            } => {
                                height == Some(named)
                                    && session
                                        .children
                                        .iter()
                                        .filter(|peer| {
                                            peer.controlled
                                                && peer.candidate_id.is_some()
                                                && peer
                                                    .rendition
                                                    .recipe
                                                    .encoding
                                                    .as_ref()
                                                    .is_some_and(|encoding| {
                                                        encoding.options.target_height == named
                                                    })
                                        })
                                        .count()
                                        == 1
                            }
                            _ => false,
                        }
                    })
                    .map(|child| {
                        (
                            child.reader_id.as_str(),
                            &child.rendition,
                            child._reservation.is_none(),
                        )
                    });
                if let Some(reader) = readers.get_mut(control.session_id) {
                    reader.accept_control(
                        control.sequence,
                        entry_containing(
                            &control_rendition.plan,
                            control.snapshot.buffer_anchor_ms() as f64 / 1_000.0,
                        ),
                    );
                    // Recompute optional speculation only after the accepted
                    // intent is published. Cancellation before that async
                    // recompute leaves speculation disabled, never enabled
                    // for the previous seek, pause, or playback position.
                    let mut prewarm = reader.marker_prewarm.lock().expect("marker ledger lock");
                    prewarm.enabled = false;
                    prewarm.deactivate();
                }
                let anchor_ms = control.snapshot.buffer_anchor_ms();
                for id in root_children {
                    readers
                        .get_mut(&id)
                        .expect("locked private root reader")
                        .accept_control(
                            control.sequence,
                            media_entry_containing_ms(&control_rendition.plan, anchor_ms),
                        );
                }
                for ((rendition, ids), child_readers) in
                    child_groups.iter().zip(child_readers.iter_mut())
                {
                    let index = media_entry_containing_ms(&rendition.plan, anchor_ms);
                    for id in ids {
                        let reader = child_readers.get_mut(id).expect("locked private reader");
                        reader.accept_control(control.sequence, index);
                        if reader.companion_bootstrap {
                            reader.control_required = selected_companion.is_some_and(
                                |(selected_id, selected_rendition, _)| {
                                    selected_id == id.as_str()
                                        && Arc::ptr_eq(selected_rendition, rendition)
                                },
                            );
                            if reader.control_required
                                && selected_companion
                                    .is_some_and(|(_, _, missing)| missing || reader.authority_only)
                            {
                                pending_companion = Some((rendition.key.clone(), control.sequence));
                            }
                            reader.admission_priority =
                                if reader.control_required || reader.quality_required {
                                    crate::admission::Priority::Live
                                } else {
                                    crate::admission::Priority::Speculative
                                };
                        }
                    }
                    rendition.kick();
                }
                control_rendition.kick();
            }
            drop(child_readers);
            drop(readers);

            if disposition == crate::playback_control::ControlDisposition::Accepted
                && control.snapshot.demand == crate::playback_control::PlaybackDemand::End
            {
                let status = ending_status?;
                let rendition = session.rendition.as_ref().map(Arc::clone)?;
                let lease_expires_at_unix_ms = crate::media_sessions::unix_ms();
                if let (Some(admission), Some(directive)) = (
                    preparation_admission.as_ref(),
                    preparation_directive.clone(),
                ) {
                    // End tombstones the VOD session and synchronously clears
                    // its local slot. Transfer any rollover/acknowledgement
                    // cleanup to the durable owner first, while the retained
                    // gate can still settle that exact slot even if the HTTP
                    // waiter disappears after this critical section.
                    admission.accepted(crate::playback_control::PreparationControlOutcome {
                        _allocation: None,
                        disposition,
                        accepted_sequence,
                        action: action.clone(),
                        action_suppressed,
                        preparation_directive: directive,
                        platform,
                        lease_expires_at_unix_ms,
                        lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                        lease_state: "ended",
                        selection: selection.clone(),
                    });
                }
                let mut result = crate::playback_control::LocalControlResult {
                    disposition,
                    accepted_sequence,
                    action,
                    action_suppressed,
                    preparation_directive,
                    lease_expires_at_unix_ms,
                    lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                    lease_state: "ended",
                    status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
                    platform,
                    terminal_handoff: None,
                    terminal_commit: None,
                    selection: selection.clone(),
                    #[cfg(target_os = "linux")]
                    finite_client_slot: session
                        .finite_control_slot
                        .as_ref()
                        .map(|slot| Arc::clone(&slot.0)),
                };
                let terminal_commit = terminal_committer
                    .as_ref()
                    .map(|committer| deferred_terminal_commit(Arc::clone(committer), &mut result));
                let cleanup = Arc::new(TerminalCleanup::new());
                session.terminal_cleanup = Some(Arc::clone(&cleanup));
                session.tombstone = Some(Terminal::Deleted);
                session.abort_staged_preparation();
                session.control_end = Some(result.clone());
                session.control_end_snapshot = Some(
                    preflight_session_snapshot
                        .take()
                        .unwrap_or_else(|| control.snapshot.clone()),
                );
                Ok::<_, crate::playback_control::ControlStateError>(AppliedControl::End {
                    terminal_commit: terminal_commit.map(Box::new),
                    result,
                    cleanup,
                    rendition,
                    file_id: session.file.id,
                    height: session.target_height,
                    kind: session.kind,
                })
            } else {
                let marker_prewarm = (source.is_none()
                    && disposition == crate::playback_control::ControlDisposition::Accepted)
                    .then(|| {
                        let (snapshot, destinations) =
                            preflight_marker_payload.take().unwrap_or_else(|| {
                                (
                                    control.snapshot.clone(),
                                    session.marker_destinations.clone(),
                                )
                            });
                        MarkerPrewarmControl {
                            rendition: session
                                .live_rendition()
                                .map(Arc::clone)
                                .expect("a live VOD control has a rendition"),
                            snapshot,
                            destinations,
                            sequence: control.sequence,
                            file_id: session.file.id,
                            kind: session.kind,
                            #[cfg(target_os = "linux")]
                            _finite_control_slot: session.finite_control_slot.clone(),
                        }
                    });
                if disposition == crate::playback_control::ControlDisposition::Accepted {
                    #[cfg(target_os = "linux")]
                    if control.snapshot.seek_target_ms.is_some() {
                        if let Some(runtime) = session
                            .live_rendition()
                            .and_then(|rendition| rendition.recipe.encoding.as_ref())
                            .and_then(|encoding| encoding.dv_runtime.as_ref())
                        {
                            runtime.invalidate_report_owner(&session.incarnation);
                        }
                    }
                    session.last_control_snapshot = Some(
                        preflight_session_snapshot
                            .take()
                            .unwrap_or_else(|| control.snapshot.clone()),
                    );
                    session.control_observed_at = Some(Instant::now());
                }
                let mut last_touch = session.last_touch.lock().expect("touch lock");
                if disposition == crate::playback_control::ControlDisposition::Accepted {
                    *last_touch = Instant::now();
                }
                let remaining = SESSION_IDLE_TTL.saturating_sub(last_touch.elapsed());
                let remaining_ms = i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX);
                let lease_expires_at_unix_ms =
                    crate::media_sessions::unix_ms().saturating_add(remaining_ms);
                if let (Some(admission), Some(directive)) = (
                    preparation_admission.as_ref(),
                    preparation_directive.clone(),
                ) {
                    // Transfer durable ownership while the accepted sequence
                    // and its preparation directive are still under the VOD
                    // lifecycle/control fence. The HTTP future may disappear
                    // immediately after this scope without stranding the slot.
                    admission.accepted(crate::playback_control::PreparationControlOutcome {
                        _allocation: None,
                        disposition,
                        accepted_sequence,
                        action: action.clone(),
                        action_suppressed,
                        preparation_directive: directive,
                        platform,
                        lease_expires_at_unix_ms,
                        lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                        lease_state: "active",
                        selection: selection.clone(),
                    });
                }
                Ok(AppliedControl::Live {
                    disposition,
                    accepted_sequence,
                    action,
                    action_suppressed,
                    preparation_directive: preparation_directive.map(Box::new),
                    platform,
                    selection: selection.clone(),
                    lease_expires_at_unix_ms,
                    marker_prewarm: marker_prewarm.map(Box::new),
                    #[cfg(target_os = "linux")]
                    finite_client_slot: session
                        .finite_control_slot
                        .as_ref()
                        .map(|slot| Arc::clone(&slot.0)),
                    pending_companion: pending_companion.map(Box::new),
                })
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        self.shared.hooks.get().after_control_applied().await;
        let (
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive,
            platform,
            selection,
            lease_expires_at_unix_ms,
            marker_prewarm,
            _finite_client_slot,
            pending_companion,
        ) = match outcome {
            AppliedControl::End {
                result,
                terminal_commit,
                cleanup,
                rendition,
                file_id,
                height,
                kind,
            } => {
                self.spawn_terminal_cleanup(
                    control.session_id.to_owned(),
                    Arc::clone(&cleanup),
                    rendition,
                    file_id,
                    height,
                    kind,
                    Terminal::Deleted,
                    terminal_commit,
                    #[cfg(target_os = "linux")]
                    result.finite_client_slot.as_ref().map(Arc::clone),
                );
                cleanup.wait().await;
                return Some(Ok(result));
            }
            AppliedControl::Live {
                disposition,
                accepted_sequence,
                action,
                action_suppressed,
                preparation_directive,
                platform,
                selection,
                lease_expires_at_unix_ms,
                marker_prewarm,
                #[cfg(target_os = "linux")]
                finite_client_slot,
                pending_companion,
            } => (
                disposition,
                accepted_sequence,
                action,
                action_suppressed,
                preparation_directive.map(|directive| *directive),
                platform,
                selection,
                lease_expires_at_unix_ms,
                marker_prewarm.map(|prewarm| *prewarm),
                {
                    #[cfg(target_os = "linux")]
                    {
                        finite_client_slot
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        None::<Arc<plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientSlot>>
                    }
                },
                pending_companion.map(|pending| *pending),
            ),
        };
        if let Some(marker_prewarm) = marker_prewarm {
            if let Some(outcome) = apply_marker_prewarm_control(
                &marker_prewarm.rendition,
                control.session_id,
                marker_prewarm.sequence,
                &marker_prewarm.snapshot,
                &marker_prewarm.destinations,
            )
            .await
            {
                self.emit_marker_prewarm(
                    control.session_id,
                    marker_prewarm.file_id,
                    marker_prewarm.kind,
                    outcome,
                );
            }
        }
        drop(lifecycle_guard);
        if let Some((rendition_id, sequence)) = pending_companion {
            let remaining = deadline_unix_ms
                .saturating_sub(crate::media_sessions::unix_ms())
                .max(0);
            if let Some(deadline) =
                Instant::now().checked_add(Duration::from_millis(remaining.min(12_000) as u64))
            {
                if let Err(error) = self
                    .admit_controlled_video_for_control_before(
                        control.session_id,
                        &rendition_id,
                        control.snapshot.buffer_anchor_ms(),
                        deadline,
                        Some(sequence),
                    )
                    .await
                {
                    tracing::debug!(target: "plurxd::vodserve", session_id = control.session_id, rendition = %rendition_id,
                        %error, "accepted pending companion could not acquire its original source credit");
                }
            }
        }
        let status = self.status(control.session_id).await?;
        Some(Ok(crate::playback_control::LocalControlResult {
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive,
            lease_expires_at_unix_ms,
            lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            lease_state: "active",
            status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
            platform,
            terminal_handoff: None,
            terminal_commit: None,
            selection,
            #[cfg(target_os = "linux")]
            finite_client_slot: _finite_client_slot,
        }))
    }
}
