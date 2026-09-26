use super::*;

impl TranscodeManager {
    /// Delete working directories under the transcode root that no live session
    /// owns. Returns how many were removed.
    ///
    /// The reaper cleans up after sessions it knows about, which covers the
    /// normal case. It cannot cover the abnormal one: a SIGKILL, an OOM, or a
    /// host reboot leaves the directories on disk and the session map empty on
    /// the way back up, so nothing ever claims them. On a 4K library those
    /// leftovers are measured in gigabytes, and the only symptom is a disk
    /// filling for no visible reason.
    pub async fn sweep_orphan_dirs(&self) -> usize {
        let live: std::collections::HashSet<PathBuf> = self
            .sessions
            .lock()
            .await
            .values()
            .map(|s| s.dir.clone())
            .collect();
        // No root yet just means nothing has been transcoded.
        let Ok(mut entries) = tokio::fs::read_dir(&self.work_dir).await else {
            return 0;
        };
        let mut removed = 0usize;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if entry.file_name() == LIVE_TV_WORK_DIR_NAME
                || live.contains(&path)
                || !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
            {
                continue;
            }
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {
                    removed += 1;
                    tracing::info!(
                        target: "plurxd::transcode",
                        dir = %path.display(), "removed orphaned transcode directory"
                    );
                }
                Err(e) => tracing::warn!(
                    target: "plurxd::transcode",
                    dir = %path.display(), error = %e, "orphan sweep failed"
                ),
            }
        }
        removed
    }

    /// Hold a session that has run far enough ahead of the client, and let it
    /// go again once the client has caught up.
    ///
    /// This is what replaced realtime pacing as the bound on disk. `-re` held
    /// production to exactly the rate of consumption, which bounded the
    /// session directory *and* guaranteed the viewer never had a buffer. The
    /// pair that separates those two concerns is: pace generously (burst, then
    /// a small multiple of realtime) so a buffer actually builds, and stop the
    /// encoder outright once the buffer is deep enough. A stopped ffmpeg costs
    /// nothing and resumes in microseconds — it is the same trick every other
    /// just-in-time server uses, and unlike a rate limit it adapts to a viewer
    /// who pauses.
    ///
    /// Media time resumes 30 seconds below the ceiling (never below half), and
    /// byte limits resume at half. The gap prevents a fast producer from
    /// toggling once per segment near the boundary. This is not a structural
    /// escape for a client that stops fetching: that client may leave the
    /// producer held while already-published media remains available. The
    /// `want_suspend == suspended` guard is load-bearing too: repeated polls
    /// cannot re-signal the child or reset the actor's motion clock.
    /// SIGKILL still works on a stopped process, so idle/admin cleanup needs no
    /// special case.
    pub(super) async fn apply_ahead_window(
        &self,
        session: &Session,
        session_id: &str,
        limits: AheadLimits,
        global_live_bytes: i64,
        global_ahead_bytes: i64,
        scratch_grant_exhausted: Option<i64>,
    ) {
        // Control acceptance, expiry, retirement, child replacement, and
        // producer signals all cross this gate. The actor remains the state
        // authority; the gate only ensures that a flow decision made from one
        // actor snapshot cannot signal a child after a newer lifecycle
        // transition has already won.
        let transition = session.child_transition.lock().await;
        if session.control.is_retired() {
            return;
        }
        let Some(lease) = session.control.snapshot().await else {
            session.control.fence_unavailable();
            return;
        };
        if lease.retired {
            return;
        }
        let (ahead, produced_end_ms) = {
            let index = session.segments.lock().await;
            (
                ahead_of(&index, session.fetched_end_ms.load(Relaxed).max(0)),
                index.produced_playable_end_ms(),
            )
        };
        let suspended = session.suspended.load(Relaxed);
        let staged_publication_seconds = session
            .staged_publication_seconds(lease.delivery.producer_attempt)
            .await;
        let evaluation = evaluate_flow(FlowInputs {
            physical_ahead: ahead,
            produced_end_ms,
            staged_publication_seconds,
            startup_protected: lease.startup.protects_from_time_hold(),
            media_origin_ms: (session.media_origin_seconds * 1_000.0).round() as i64,
            lease_mode: lease.mode,
            demand: lease.demand.as_ref(),
            demand_observation_age: lease.demand_observation_age,
            global_live_bytes,
            global_ahead_bytes,
            limits,
            currently_suspended: suspended,
            scratch_grant_exhausted,
        });
        let hold = evaluation.hold;
        let want_suspend = hold.is_some();
        if want_suspend == suspended {
            if let Some(hold) = hold {
                let mut suspended_at = session.suspended_at.lock().await;
                if let Some(previous) = suspended_at.as_mut() {
                    if previous.hold != hold {
                        let previous_reason = previous.hold.reason;
                        previous.hold = hold;
                        drop(suspended_at);
                        drop(transition);
                        tracing::info!(
                            target: "plurxd::transcode",
                            session = %session_log_id(session_id),
                            previous_hold_reason = ?previous_reason,
                            hold_reason = ?hold.reason,
                            release_value = hold.release_value,
                            production_policy = evaluation.policy,
                            production_ahead_seconds = evaluation.production_ahead_seconds,
                            production_target_seconds = evaluation.production_target_seconds,
                            "transcode hold authority changed without resuming the producer"
                        );
                        self.emit_session_event(
                            session_id,
                            session,
                            "hold_change",
                            SessionEventFields {
                                reason: Some("policy_change"),
                                hold_reason: Some(hold.reason),
                                extra: Some(flow_event_extra(
                                    evaluation,
                                    &lease,
                                    ahead,
                                    global_live_bytes,
                                    global_ahead_bytes,
                                    Some(previous_reason),
                                )),
                                ..SessionEventFields::default()
                            },
                        )
                        .await;
                    }
                }
            }
            return;
        }
        // The desire goes on the actor sequence before any syscall, and the
        // reply is the only authorization to make one. Two flow evaluations
        // used to reach this point believing each owned the next signal,
        // because the only shared state was the `suspended` atomic read at the
        // top of this function; the actor now settles that ordering. A desire
        // that arrives while a signal is outstanding coalesces into the newest
        // revision and returns here without signalling, and the next
        // evaluation applies it once the acknowledgement lands.
        let producer_attempt = session.control.current_producer_attempt();
        let intention_deadline = Instant::now() + FLOW_INTENTION_BUDGET;
        match session
            .control
            .request_producer_flow_before(producer_attempt, want_suspend, intention_deadline)
            .await
        {
            crate::playback_control::ProducerFlowIntentionOutcome::Issue { .. } => {}
            crate::playback_control::ProducerFlowIntentionOutcome::Coalesced
            | crate::playback_control::ProducerFlowIntentionOutcome::Settled
            | crate::playback_control::ProducerFlowIntentionOutcome::Rejected => return,
        }
        let signal = if want_suspend {
            crate::process_control::ProcessSignal::Suspend
        } else {
            crate::process_control::ProcessSignal::Resume
        };
        let sent = {
            let child = session.child.lock().await;
            match child.as_ref() {
                // The sole process owner rechecks the actor's exact attempt
                // and deadline fence immediately before signaling. No caller
                // can race its cached pid against reaping or reuse.
                Some(child) => child.signal(signal).await.unwrap_or(false),
                None => false,
            }
        };
        if !sent {
            // A failed or refused syscall publishes no acknowledgement
            // barrier, so the actor's outstanding claim has to be released
            // here or every later desire coalesces behind a signal that no
            // longer exists.
            let _ = session
                .control
                .settle_producer_flow_signal_before(
                    producer_attempt,
                    Instant::now() + FLOW_INTENTION_BUDGET,
                )
                .await;
            return;
        }
        if !want_suspend {
            // Before the flag flips, so the actor can never observe
            // "running" beside a motion clock that still spans the suspension
            // — that read would fail a healthy session at the moment of its
            // resume, before ffmpeg has emitted a single post-SIGCONT block.
            session.progress.touch();
        }
        if want_suspend {
            let hold = hold.expect("a requested suspension has a hold reason");
            *session.suspended_at.lock().await = Some(SuspendedAt {
                since: Instant::now(),
                hold,
            });
            session.suspended.store(true, Relaxed);
            let suspend_count = session.suspend_count.fetch_add(1, Relaxed) + 1;
            crate::playback_control::record_producer_hold(hold.reason);
            drop(transition);
            tracing::info!(
                target: "plurxd::transcode",
                session = %session_log_id(session_id),
                suspend_count,
                hold_reason = ?hold.reason,
                release_value = hold.release_value,
                ahead_seconds = ahead.map(|ahead| ahead.seconds),
                ahead_bytes = ahead.map(|ahead| ahead.bytes),
                global_live_bytes, global_ahead_bytes,
                max_secs = limits.max_secs, max_bytes = limits.max_bytes,
                production_policy = evaluation.policy,
                production_ahead_seconds = evaluation.production_ahead_seconds,
                production_target_seconds = evaluation.production_target_seconds,
                "holding transcode producer"
            );
            self.emit_session_event(
                session_id,
                session,
                "suspend",
                SessionEventFields {
                    hold_reason: Some(hold.reason),
                    extra: Some(flow_event_extra(
                        evaluation,
                        &lease,
                        ahead,
                        global_live_bytes,
                        global_ahead_bytes,
                        None,
                    )),
                    ..SessionEventFields::default()
                },
            )
            .await;
        } else {
            session.suspended.store(false, Relaxed);
            let held = session.suspended_at.lock().await.take();
            let held_ms =
                held.map(|held| held.since.elapsed().as_millis().min(i64::MAX as u128) as i64);
            let hold_reason = held.map(|held| held.hold.reason);
            if let Some(hold_reason) = hold_reason {
                crate::playback_control::record_producer_resume(hold_reason);
            }
            drop(transition);
            tracing::info!(
                target: "plurxd::transcode",
                session = %session_log_id(session_id),
                suspend_count = session.suspend_count.load(Relaxed),
                ahead_seconds = ahead.map(|ahead| ahead.seconds),
                ahead_bytes = ahead.map(|ahead| ahead.bytes),
                production_policy = evaluation.policy,
                production_ahead_seconds = evaluation.production_ahead_seconds,
                production_target_seconds = evaluation.production_target_seconds,
                "resuming transcode producer"
            );
            self.emit_session_event(
                session_id,
                session,
                "resume",
                SessionEventFields {
                    hold_reason,
                    ms: held_ms,
                    extra: Some(flow_event_extra(
                        evaluation,
                        &lease,
                        ahead,
                        global_live_bytes,
                        global_ahead_bytes,
                        hold_reason,
                    )),
                    ..SessionEventFields::default()
                },
            )
            .await;
        }
    }

    /// The bounds every session is held to — from the snapshot while it is
    /// fresh, from the settings when it is not.
    pub(super) async fn ahead_limits(&self) -> AheadLimits {
        if let Some((at, limits)) = *self.cached_limits.read().expect("limits lock") {
            if at.elapsed() < AHEAD_LIMITS_TTL {
                return limits;
            }
        }
        let limits = AheadLimits {
            max_secs: self
                .num_setting(keys::HLS_AHEAD_MAX_SECS, HLS_AHEAD_MAX_SECS_DEFAULT)
                .await,
            max_bytes: self
                .num_setting(keys::HLS_AHEAD_MAX_BYTES, HLS_AHEAD_MAX_BYTES_DEFAULT)
                .await,
            global_max_bytes: self
                .num_setting(keys::HLS_SCRATCH_MAX_BYTES, HLS_SCRATCH_MAX_BYTES_DEFAULT)
                .await,
        };
        // Two refreshers racing both read the same rows; last write wins and
        // they agree to within the TTL anyway.
        *self.cached_limits.write().expect("limits lock") = Some((Instant::now(), limits));
        self.scratch_cap.store(limits.global_max_bytes, Relaxed);
        limits
    }

    /// Reserve the largest configured live working set before a producer can
    /// create bytes.  This is deliberately an admission decision rather than
    /// a later flow-control observation: the latter cannot make a disk ceiling
    /// hard when several starts race through an empty pre-playlist directory.
    pub(super) async fn reserve_rolling_scratch(
        &self,
        initial: RollingScratchSizing,
    ) -> Result<crate::scratch_ledger::ScratchPermit, String> {
        let limits = self.ahead_limits().await;
        // The ledger's own critical section is the linearization point, so
        // there is no separate admission gate to hold across this settings
        // read: two starts racing an empty directory still cannot both spend
        // the same remaining capacity.
        let grant = initial.grant_bytes(limits);
        self.scratch_ledger
            .reserve(grant, limits.global_max_bytes)
            .map_err(|refusal| {
                // Classified, so `session_start_error` answers 503 rather
                // than an anonymous 500. The figures are the operator's only
                // way to tell a full budget from a stuck cleanup.
                tracing::warn!(
                    target: "plurxd::transcode",
                    charged = refusal.charged,
                    requested = refusal.requested,
                    configured = refusal.configured,
                    holders = ?self.scratch_ledger.describe(6),
                    "refusing a rolling start: scratch budget is full"
                );
                capacity_error(refusal.to_string())
            })
    }

    /// The scratch charge every admission is compared against, and the
    /// categories that explain it.
    #[cfg(test)]
    pub(crate) fn scratch_snapshot(&self) -> crate::scratch_ledger::ScratchSnapshot {
        self.scratch_ledger.snapshot()
    }

    /// One accepted exact-incarnation release, from the capability-authenticated
    /// DELETE the client already sends.
    ///
    /// `session_id` is the credential and the identity at once: it names one
    /// process-local incarnation in one of the two registries, so a stale or
    /// unknown id finds nothing and another viewer's release cannot reach
    /// this entry. The class comes from the durable recipe the coordinator
    /// resolved, which is what makes a remote owner, a replay and an owner
    /// takeover all read the same answer as the create.
    ///
    /// Returns the shortened deadline only when this release actually moved
    /// it. Everything else — a conservative class, a duplicate DELETE, an
    /// unknown session — is a no-op, and says so by returning `None`.
    pub(crate) async fn accept_exact_session_release(
        &self,
        session_id: &str,
        class: ReleaseClass,
    ) -> Option<Instant> {
        class.allowance()?;
        let session = {
            let live = self.sessions.lock().await;
            match live.get(session_id) {
                Some(session) => Arc::clone(session),
                None => {
                    drop(live);
                    let retired = self.retired_presentations.lock().await;
                    Arc::clone(&retired.get(session_id)?.session)
                }
            }
        };
        if session.cached {
            return None;
        }
        let shortened = session.accept_exact_release(Instant::now(), class);
        if let Some(deadline) = shortened {
            // Everything that decides whether a read is admitted moves with
            // the promise, or cleanup would outrun the serve gate.
            session.shorten_object_grace(deadline).await;
            let mut retired = self.retired_presentations.lock().await;
            if let Some(entry) = retired.get_mut(session_id) {
                if Arc::ptr_eq(&entry.session, &session) && entry.serve_until > deadline {
                    entry.serve_until = deadline;
                }
            }
        }
        tracing::debug!(
            target: "plurxd::transcode",
            session = %session_log_id(session_id),
            class = class.label(),
            shortened = shortened.is_some(),
            "exact same-viewer release recorded against the retired promise"
        );
        shortened
    }

    /// Forget the snapshot, so the next evaluation reads the settings. For
    /// tests, which assert on the *policy* (cached-until-stale) and must not
    /// spend wall clock waiting a TTL out.
    #[cfg(test)]
    pub(super) fn forget_cached_limits(&self) {
        *self.cached_limits.write().expect("limits lock") = None;
    }

    /// Publish a session under `session_id` without starting a producer, so a
    /// test can drive the real `segment` path — and the HTTP handler above it
    /// — against files on disk instead of against ffmpeg.
    #[cfg(test)]
    pub(super) async fn register_session_for_test(&self, session_id: &str, session: Arc<Session>) {
        self.sessions
            .lock()
            .await
            .insert(session_id.to_owned(), session);
    }

    #[cfg(test)]
    pub(crate) fn set_subtitle_playlist_commit_pause(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .subtitle_playlist_commit_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    pub(crate) async fn pause_subtitle_playlist_commit_for_test(&self) {
        let pause = self
            .subtitle_playlist_commit_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(pause) = pause {
            pause.wait().await;
            pause.wait().await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn install_vod_http_test_session(
        &self,
        session_id: &str,
        file_id: i64,
        base: &std::path::Path,
    ) {
        let file = self
            .store
            .get_file(file_id)
            .await
            .expect("HTTP VOD fixture file lookup")
            .expect("HTTP VOD fixture file");
        self.vod
            .install_http_test_session(session_id, file, base)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn vod_last_touch_for_test(&self, session_id: &str) -> Option<Instant> {
        self.vod.last_touch_for_test(session_id).await
    }

    #[cfg(test)]
    pub(crate) fn set_vod_terminal_detach_pause_for_test(&self, pause: Arc<tokio::sync::Barrier>) {
        self.vod.set_terminal_detach_pause_for_test(pause);
    }

    #[cfg(test)]
    pub(crate) async fn vod_has_attached_reader_for_test(&self, session_id: &str) -> bool {
        self.vod.has_attached_reader_for_test(session_id).await
    }

    /// Both byte views across every live session, from their cached figures —
    /// summing these must not cost a directory walk per session, or the flow
    /// controller could not run on every segment fetch.
    ///
    /// TOTAL bytes enforce the documented disk ceiling
    /// ([`keys::HLS_SCRATCH_MAX_BYTES`]); drainable AHEAD bytes decide when a
    /// global hold may release. The distinction is load-bearing: each
    /// session's retained history is real scratch but cannot fall until its
    /// client frontier moves beyond [`RETENTION_SECS`].
    /// TOTAL bytes come from the ledger in one consistent read — one charge
    /// per incarnation, whichever registry currently holds it, including the
    /// starts that are in neither yet. The previous implementation folded the
    /// two registries under two separate locks and then inferred provisional
    /// usage by subtraction, so a session retiring between the folds was
    /// charged twice and its reservation subtracted twice; the zero clamp
    /// that hid the negative result also hid genuinely provisional starts.
    ///
    /// Drainable AHEAD bytes are still a live-registry question and are
    /// deliberately computed separately: retained history is real scratch
    /// that cannot fall until the client frontier moves past
    /// [`RETENTION_SECS`], so it belongs to the budget and not to pacing.
    pub(super) async fn global_flow_bytes(&self) -> (i64, i64) {
        let charged = self.scratch_ledger.snapshot().total;
        let ahead = self
            .sessions
            .lock()
            .await
            .values()
            .fold(0_i64, |ahead, session| {
                ahead.saturating_add(session.ahead_bytes.load(Relaxed))
            });
        (charged, ahead)
    }

    /// Ensure this rolling incarnation has exactly one detached consumer for
    /// actor-owned flow tickets. The task is intentionally rooted in the
    /// session, not in any HTTP request: accepted control remains effective
    /// after cancellation, and response-body EOF never waits on a child
    /// signal before it can publish END_STREAM.
    pub(super) fn ensure_flow_worker(self: &Arc<Self>, session_id: &str, session: Arc<Session>) {
        if session.flow_worker_started.swap(true, AcqRel) {
            return;
        }
        let manager = Arc::clone(self);
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            let mut handled = 0_u64;
            loop {
                let ticket = session.control.next_flow_request(handled).await;
                let is_current = manager
                    .sessions
                    .lock()
                    .await
                    .get(&session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &session));
                if session.control.is_retired() || !is_current {
                    // Release any response already waiting on this generation;
                    // it will observe retirement or replacement in status.
                    session.control.complete_flow(ticket);
                    break;
                }
                manager.flow_control(&session, &session_id).await;
                #[cfg(test)]
                let flow_pause = session
                    .flow_completion_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                #[cfg(test)]
                if let Some(pause) = flow_pause {
                    pause.wait().await;
                    pause.wait().await;
                }
                handled = ticket;
                session.control.complete_flow(ticket);
            }
            session.flow_worker_started.store(false, Release);
        });
    }

    /// Re-evaluate one session after a client request refreshes the published
    /// index or advances the download frontier.
    ///
    /// The reaper still sweeps every 15 seconds, but as a repair loop. A
    /// window that is only checked on a 15-second tick is not a flow
    /// controller — a fast encoder can put a great deal of 4K on disk between
    /// two ticks.
    pub(super) async fn flow_control(&self, session: &Session, session_id: &str) {
        session.refresh_segments().await;
        let limits = self.ahead_limits().await;
        // Growth, before the hold decision that depends on it. The ledger
        // entry is re-authorized to `measured + envelope`, so a session that
        // has pruned its retention gives capacity back and one that is
        // producing keeps a bounded allowance ahead of the disk.
        let scratch_grant_exhausted = self.regrant_rolling_scratch(session, limits);
        let (global_live, global_ahead) = self.global_flow_bytes().await;
        self.apply_ahead_window(
            session,
            session_id,
            limits,
            global_live,
            global_ahead,
            scratch_grant_exhausted,
        )
        .await;
    }

    /// Re-authorize one producing session, and say whether it must stay held.
    ///
    /// `Some(grant)` means the budget refused to raise this entry and the
    /// producer has already materialized everything it is authorized to, or
    /// that one of its writers is waiting on a refused grant. A denied grant
    /// is a hold, never permission to write into space nobody accounted for,
    /// and a starved writer means the producer is blocked on a write: held,
    /// that is a hold; unheld, its progress deadline reads it as a stall.
    fn regrant_rolling_scratch(&self, session: &Session, limits: AheadLimits) -> Option<i64> {
        let permit = session.scratch.as_ref()?;
        if session.scratch_envelope <= 0 {
            // Admitted with the whole per-session ceiling: there is nothing to
            // grow into and nothing to re-grant.
            return None;
        }
        let ledger = permit.ledger();
        let key = permit.key();
        let regranted = ledger.regrant(key, session.scratch_envelope, limits.global_max_bytes);
        if ledger.starved(key) || (!regranted && ledger.grant_exhausted(key)) {
            return Some(ledger.grant_of(key).unwrap_or(0));
        }
        None
    }

    /// Evaluate flow for every session whose writer just started or stopped
    /// waiting on a refused grant, so the hold lands while the writer waits
    /// and lifts as soon as it can write, not at the next repair pass.
    async fn evaluate_starved_sessions(self: &Arc<Self>) {
        let sessions = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, session)| (id.clone(), Arc::clone(session)))
            .collect::<Vec<_>>();
        for (id, session) in sessions {
            let Some(permit) = session.scratch.as_ref() else {
                continue;
            };
            if permit.ledger().starved(permit.key()) || session.suspended.load(Relaxed) {
                self.ensure_flow_worker(&id, Arc::clone(&session));
                let _ = session.control.request_flow();
            }
        }
    }

    /// Bind the upload endpoint a session's FFmpeg muxer writes through,
    /// spending grants from the allocation admission just made.
    pub(super) fn bind_scratch_upload(
        &self,
        dir: &std::path::Path,
        reservation: &crate::scratch_ledger::ScratchPermit,
        envelope: i64,
    ) -> Result<crate::scratch_put::PutSink, String> {
        crate::scratch_put::PutSink::bind(
            dir.to_path_buf(),
            Some(
                crate::copyseg::WriteGrants::new(
                    Arc::clone(reservation.ledger()),
                    reservation.key(),
                    Arc::clone(&self.scratch_cap),
                    envelope,
                )
                .with_starved_signal(Arc::clone(&self.scratch_starved)),
            ),
        )
        // Loopback bind fails on descriptor or port exhaustion: transient.
        .map_err(|error| capacity_error(format!("binding the scratch upload endpoint: {error}")))
    }

    /// Background loop: kill and remove sessions idle beyond the timeout,
    /// prune played-past segments, and hold sessions that have run ahead.
    pub async fn reap_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(FLOW_CONTROL_REPAIR_INTERVAL);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                () = self.scratch_starved.notified() => {
                    self.evaluate_starved_sessions().await;
                    continue;
                }
            }
            let now_unix_ms = crate::media_sessions::unix_ms();
            self.terminal_controls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|_, operation| !operation.expired(now_unix_ms));
            let limits = self.ahead_limits().await;
            let mut expired = Vec::new();
            let mut live = Vec::new();
            let sessions = self
                .sessions
                .lock()
                .await
                .iter()
                .map(|(id, session)| (id.clone(), Arc::clone(session)))
                .collect::<Vec<_>>();
            for (id, session) in sessions {
                match self.session_reap_verdict(id, session).await {
                    SessionReapVerdict::Live(id, session) => live.push((id, session)),
                    SessionReapVerdict::CleanupOwned => {}
                    SessionReapVerdict::Expired {
                        id,
                        session,
                        idle_seconds,
                        last_request,
                        cleanup_reason,
                    } => expired.push((id, session, idle_seconds, last_request, cleanup_reason)),
                }
            }
            for (id, session, idle_seconds, last_request, cleanup_reason) in expired {
                // Kills a suspended child too — SIGKILL is not blockable and
                // does not need the process scheduled to take effect.
                let end_reason = if session.failed.load(Relaxed) {
                    "failed"
                } else {
                    cleanup_reason
                };
                let Ok(outcome) = self
                    .retire_session_until_with_cause(&id, &session, None, end_reason)
                    .await
                else {
                    continue;
                };
                if !outcome.removed {
                    continue;
                }
                tracing::info!(
                    target: "plurxd::transcode",
                    session = %session_log_id(&id),
                    idle_seconds,
                    last_request,
                    requested_reason = end_reason,
                    winning_reason = %outcome.cause,
                    participation = ?outcome.participation,
                    "reaped transcode session after lease actor verdict"
                );
            }
            // What this box actually achieves, remembered per class of work.
            // Admission asks it the next time hardware is full, so the answer
            // to "can software cope with this" is a measurement from this
            // machine rather than an assumption about machines in general.
            // A suspended session is making no progress on purpose and would
            // poison the record with a speed it was never asked to reach.
            for (_id, session) in &live {
                let class = session.class.lock().expect("class mutex").clone();
                if class.is_empty() || session.suspended.load(Relaxed) {
                    continue;
                }
                if let Some(speed) = session.progress.recent_speed() {
                    self.admissions.record(&class, speed);
                }
            }
            // Repair pass. Client requests run flow control on playlist/index
            // refresh and frontier advance; this catches a producer that no
            // client is currently requesting from, and prunes retention.
            for (_id, session) in &live {
                session.refresh_segments().await;
                gc_expired_segments(session).await;
            }
            let (global_live, global_ahead) = self.global_flow_bytes().await;
            for (id, session) in &live {
                let scratch_grant_exhausted = self.regrant_rolling_scratch(session, limits);
                self.apply_ahead_window(
                    session,
                    id,
                    limits,
                    global_live,
                    global_ahead,
                    scratch_grant_exhausted,
                )
                .await;
            }
        }
    }

    pub(super) async fn session_reap_verdict(
        &self,
        id: String,
        session: Arc<Session>,
    ) -> SessionReapVerdict {
        if session.retirement_cleanup_started.load(Acquire)
            || session.prepublication_cleanup_active.load(Acquire)
            || session.cache_integrity_cleanup_started.load(Acquire)
        {
            return SessionReapVerdict::CleanupOwned;
        }
        // Serialize the actor's expiry fence with every producer signal and
        // child transition. A pending accepted-End handoff is live only for
        // cleanup purposes: the actor is terminal, but removing its Arc before
        // the replicated acknowledgement lands would discard the winner.
        let transition = session.child_transition.lock().await;
        if session.retirement_cleanup_started.load(Acquire)
            || session.prepublication_cleanup_active.load(Acquire)
            || session.cache_integrity_cleanup_started.load(Acquire)
        {
            drop(transition);
            return SessionReapVerdict::CleanupOwned;
        }
        if session.terminal_response_pending.load(Acquire) {
            drop(transition);
            return SessionReapVerdict::Live(id, session);
        }
        let claim = session.control.claim_expiry().await;
        drop(transition);
        match claim {
            Ok(crate::playback_control::RollingExpiryClaim::Claimed(lease)) => {
                SessionReapVerdict::Expired {
                    id,
                    session,
                    idle_seconds: lease.idle_for.as_secs(),
                    last_request: lease.last_renewal_kind,
                    cleanup_reason: "idle",
                }
            }
            Ok(crate::playback_control::RollingExpiryClaim::Retired(lease)) => {
                SessionReapVerdict::Expired {
                    id,
                    session,
                    idle_seconds: lease.idle_for.as_secs(),
                    last_request: lease.last_renewal_kind,
                    cleanup_reason: if lease.expiration_claimed {
                        "idle"
                    } else {
                        "retired_recovery"
                    },
                }
            }
            Ok(crate::playback_control::RollingExpiryClaim::Live) => {
                SessionReapVerdict::Live(id, session)
            }
            Err(_) => {
                session.control.fence_unavailable();
                SessionReapVerdict::Expired {
                    id,
                    session,
                    idle_seconds: 0,
                    last_request: "control-unavailable",
                    cleanup_reason: "control_unavailable",
                }
            }
        }
    }
}
