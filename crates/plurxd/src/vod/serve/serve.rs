use super::*;

impl VodServe {
    /// The facts a stall-reopen's normalization checks against its
    /// predecessor. `None` for unknown/tombstoned.
    pub async fn reopen_facts(&self, session_id: &str) -> Option<ReopenFacts> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        Some(ReopenFacts {
            supersession_user: session.supersession_user.clone(),
            playback_id: session.playback_id.clone(),
            file_id: session.file.id,
        })
    }

    /// The file a live VOD session is serving, for callers that need source
    /// facts at response time (the Apple init-record rewrite).
    pub async fn session_file_id(&self, session_id: &str) -> Option<i64> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        session.live_rendition()?;
        Some(session.file.id)
    }

    /// Exact copy-recipe facts for native HLS wrappers. Lookup alone does not
    /// touch the sliding TTL; the HTTP response commit does that after every
    /// playlist and init-object check succeeds.
    pub async fn hls_facts(&self, session_id: &str) -> Option<VodHlsFacts> {
        let publication = self.session_rendition(session_id).await?;
        let rendition = match publication.result {
            Ok((rendition, _, _)) => rendition,
            _ => return None,
        };
        Some(VodHlsFacts {
            file: rendition.recipe.file.clone(),
            audio_index: rendition.recipe.audio_index,
            aac: rendition.recipe.aac,
            preserve_dolby_vision: rendition.recipe.video.preserves_dolby_vision(),
            convert_dolby_vision: rendition.recipe.video.converts_dolby_vision(),
            encoding: rendition.recipe.encoding.clone(),
            response_owner: publication.owner,
        })
    }

    /// One immutable plan entry's film-time window for WebVTT children.
    pub async fn segment_window(&self, session_id: &str, segment_index: i64) -> Option<(f64, f64)> {
        let index = u32::try_from(segment_index).ok()?;
        let publication = self.session_rendition(session_id).await?;
        let (rendition, _, _) = match publication.result {
            Ok(value) => value,
            _ => return None,
        };
        let entry = rendition.plan.entry(index)?;
        Some((
            entry.start_ticks as f64 / f64::from(rendition.timescale),
            entry.end_ticks() as f64 / f64::from(rendition.timescale),
        ))
    }

    /// Rebuild the create answer for an idempotent replay (`request_id`
    /// recovery). `None` for a session that is not ours or has ended — the
    /// caller then answers the way it always has.
    pub async fn recovered_start(&self, session_id: &str) -> Option<RecoveredVod> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some() {
            return None;
        }
        Some(RecoveredVod {
            start: VodStart {
                session_id: session_id.to_owned(),
                duration_ms: plan_duration_ms(&session.live_rendition()?.plan),
            },
            target_height: session.target_height,
            kind: session.kind,
        })
    }

    /// Periodic maintenance, called from a spawned interval task the manager
    /// owns: dormant-session reap (sliding TTL), dormant-rendition purge
    /// after TTL (un-admitted only), driver kicks.
    pub async fn maintain(&self) {
        // Startup seeds the first bounded batch; every live maintenance tick
        // discovers the next one. Plan deletion precedes directory deletion,
        // so a crash cannot erase the only durable key needed to collect the
        // node-local database row.
        self.shared.reconcile_obsolete_encoded_generations().await;

        let terminal_cleanups = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .values()
                .filter(|session| session.tombstone.is_some())
                .filter_map(|session| session.terminal_cleanup.as_ref().map(Arc::clone))
                .filter(|cleanup| !cleanup.is_finished())
                .collect::<Vec<_>>()
        };
        // Bounded, because every later step in this function is behind it and
        // this task is the node's only maintenance loop. A cleanup that never
        // completes — a task the runtime dropped mid-flight, a future ordering
        // change that installs one with no owner — would otherwise stop the
        // idle reap, the dormant purge, the tombstone eviction and every
        // driver kick on the node, permanently and silently.
        //
        // Giving up on the wait gives up nothing else: each step below re-reads
        // what it needs, and tombstone eviction independently requires
        // `is_finished()`, so a cleanup still legitimately running is simply
        // waited for on the next tick. The bound is generous against real
        // settlement — a terminal commit is the slow part — so reaching it is
        // a defect worth a loud line rather than a busy node.
        if tokio::time::timeout(
            TERMINAL_CLEANUP_MAINTENANCE_WAIT,
            futures_util::future::join_all(
                terminal_cleanups
                    .into_iter()
                    .map(|cleanup| async move { cleanup.wait().await }),
            ),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                target: "plurxd::vodserve",
                "a VOD terminal cleanup outlived {:?} of maintenance waiting; \
                 continuing this pass without it",
                TERMINAL_CLEANUP_MAINTENANCE_WAIT
            );
        }

        // End tombstones outlive the response-recovery operation so late or
        // mismatched control cannot resurrect a detached reader. Once the
        // exact durable acknowledgement window closes, retain only that small
        // ownership fence and release the Store/response/retry graph.
        {
            let mut sessions = self.shared.sessions.lock().await;
            for session in sessions
                .values_mut()
                .filter(|session| session.tombstone.is_some())
            {
                let expired = session
                    .control_end
                    .as_ref()
                    .and_then(|result| result.terminal_commit.as_ref())
                    .is_some_and(|commit| commit.is_expired());
                if expired {
                    session.control_end = None;
                }
            }
        }

        // A terminal session retains only its compact exact response owner for
        // the bounded late-410 replay window. After that window, release it
        // only when a fresh durable read proves this capability cannot be active.
        // Store I/O stays outside both the per-id lifecycle gate and the
        // node-wide session registry lock.
        let mut terminal_eviction_candidates = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter_map(|(session_id, session)| {
                    let cleanup = session.terminal_cleanup.as_ref()?;
                    (session.tombstone.is_some()
                        && cleanup.is_finished()
                        && cleanup.retention_expired()
                        && session.terminal_replay_expired())
                    .then(|| TerminalEvictionCandidate {
                        session_id: session_id.clone(),
                        lifecycle: Arc::clone(&session.lifecycle),
                        incarnation: Arc::clone(&session.incarnation),
                        cleanup: Arc::clone(cleanup),
                    })
                })
                .collect::<Vec<_>>()
        };
        // Confirm one bounded, fair batch per tick. A Store timeout therefore
        // costs at most one timeout interval instead of one interval per
        // fanout wave, while rotating the start prevents a consistently bad
        // route from pinning every candidate behind it.
        if !terminal_eviction_candidates.is_empty() {
            terminal_eviction_candidates
                .sort_by(|left, right| left.session_id.cmp(&right.session_id));
            let start = self
                .shared
                .terminal_eviction_cursor
                .fetch_add(TERMINAL_ROUTE_CONFIRM_BATCH as u64, Relaxed)
                as usize
                % terminal_eviction_candidates.len();
            terminal_eviction_candidates.rotate_left(start);
            terminal_eviction_candidates.truncate(TERMINAL_ROUTE_CONFIRM_BATCH);
        }
        let shared = Arc::clone(&self.shared);
        let terminal_eviction_candidates =
            futures_util::stream::iter(terminal_eviction_candidates.into_iter().map(|candidate| {
                let shared = Arc::clone(&shared);
                async move {
                    let durable_non_live = shared
                        .terminal_route_durably_non_live(&candidate.session_id)
                        .await;
                    durable_non_live.then_some(candidate)
                }
            }))
            .buffer_unordered(TERMINAL_ROUTE_CONFIRM_FANOUT)
            .filter_map(std::future::ready)
            .collect::<Vec<_>>()
            .await;
        for candidate in terminal_eviction_candidates {
            let _lifecycle = candidate.lifecycle.lock().await;
            let removed = {
                let mut sessions = self.shared.sessions.lock().await;
                let exact_terminal = sessions.get(&candidate.session_id).is_some_and(|session| {
                    Arc::ptr_eq(&session.lifecycle, &candidate.lifecycle)
                        && Arc::ptr_eq(&session.incarnation, &candidate.incarnation)
                        && session.tombstone.is_some()
                        && session.terminal_replay_expired()
                        && session.terminal_cleanup.as_ref().is_some_and(|cleanup| {
                            Arc::ptr_eq(cleanup, &candidate.cleanup)
                                && cleanup.is_finished()
                                && cleanup.retention_expired()
                        })
                });
                if exact_terminal {
                    sessions.remove(&candidate.session_id)
                } else {
                    None
                }
            };
            if removed.is_some() {
                tracing::debug!(
                    target: "plurxd::vodserve",
                    session = %session_log_id(&candidate.session_id),
                    "released an expired VOD terminal response-owner tombstone"
                );
            }
        }

        let now = Instant::now();
        // Idle live sessions vanish — tombstone-free, because an idle reap is
        // the one ending a session may come back from (via the durable route
        // machinery outside this module).
        let expired: Vec<(String, Arc<Mutex<()>>)> = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.tombstone.is_none()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                })
                .map(|(id, session)| (id.clone(), Arc::clone(&session.lifecycle)))
                .collect()
        };
        for (id, lifecycle) in expired {
            let _lifecycle = lifecycle.lock().await;
            let rendition = {
                let mut sessions = self.shared.sessions.lock().await;
                let still_expired = sessions.get(&id).is_some_and(|session| {
                    Arc::ptr_eq(&session.lifecycle, &lifecycle)
                        && session.tombstone.is_none()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                });
                if still_expired {
                    sessions.remove(&id).and_then(|session| session.rendition)
                } else {
                    None
                }
            };
            if let Some(rendition) = rendition {
                // Keep the per-id gate through detach. A resurrection for the
                // same durable id must attach only after this old reader is
                // gone, never between registry removal and detach.
                rendition.detach_reader(&self.shared.pool, &id).await;
                tracing::info!(
                    target: "plurxd::vodserve",
                    session = %session_log_id(&id),
                    rendition = %rendition.key,
                    "vod session idle-reaped (sliding TTL)"
                );
            }
        }

        // Purge un-admitted renditions dormant past their TTL. The collection
        // is only a cheap pre-filter; `purge_if_dormant` takes the exact-key
        // build gate and re-checks everything before its short map commit.
        let dormant: Vec<String> = {
            let renditions = self.shared.renditions.lock().await;
            renditions
                .values()
                .filter(|rendition| {
                    rendition
                        .dormant_since
                        .lock()
                        .expect("dormant lock")
                        .is_some_and(|since| now.duration_since(since) > DORMANT_RENDITION_TTL)
                })
                .map(|rendition| rendition.key.clone())
                .collect()
        };
        for key in dormant {
            self.shared
                .purge_if_dormant(&key, DORMANT_RENDITION_TTL)
                .await;
        }

        // Kick every driver so holds and idle reclaims are re-examined.
        let renditions: Vec<Arc<Rendition>> = {
            self.shared
                .renditions
                .lock()
                .await
                .values()
                .map(Arc::clone)
                .collect()
        };
        for rendition in renditions {
            rendition.kick();
        }
        self.shared.prune_session_lifecycles();
        self.shared.prune_rendition_builds();
    }

    // ---- serving internals -------------------------------------------------

    /// The session's rendition and block budget, or the typed reason there
    /// isn't one. This is lookup only; response commit owns liveness.
    pub(super) async fn session_rendition(
        &self,
        session_id: &str,
    ) -> Option<VodPublication<(Arc<Rendition>, Duration, Arc<crate::meter::Meter>)>> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let owner = session.response_owner();
        Some(VodPublication {
            result: match session.tombstone {
                Some(cause) => Err(VodError::Gone(cause)),
                None => Ok((
                    session.live_rendition().map(Arc::clone)?,
                    session.block_budget,
                    Arc::clone(&session.delivery),
                )),
            },
            owner,
        })
    }

    pub(super) async fn serve_init(
        &self,
        rendition: &Arc<Rendition>,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(
                &self.shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(VodError::ProducerFailed(cause));
        }
        let demand = self
            .shared
            .arm_materialize_watchdog(rendition, INIT_DEMAND_INDEX);
        rendition.kick();
        let deadline = Instant::now() + budget;
        loop {
            // Arm the notification — created AND enabled — before checking
            // the disk. `Notified` registers with its `Notify` only on first
            // poll or `enable()`; without the explicit enable, a
            // `notify_waiters` firing between the disk check and the await
            // wakes nobody, and the first init fetch stalls a full budget for
            // bytes that are already there.
            let notified = rendition.init_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if rendition.dir.has_init().await {
                rendition.clear_demand(INIT_DEMAND_INDEX);
                let path = rendition.dir.path().join(INIT_NAME);
                let ready = open_ready(&path, &format!("{}-init", rendition.key), &delivery)
                    .await
                    .map_err(VodError::Io)?;
                if self.source_changed(rendition) {
                    let cause =
                        "source changed while the rendition init was being opened".to_owned();
                    record_failure(
                        &self.shared,
                        rendition,
                        crate::playback_control::ProducerDecisionReason::SourceChanged,
                        cause.clone(),
                    );
                    return Err(VodError::ProducerFailed(cause));
                }
                return Ok(ready);
            }
            if let Some(cause) = rendition.failure_cause() {
                return Err(VodError::ProducerFailed(cause));
            }
            if demand.expired() {
                return Err(VodError::ProducerFailed(
                    "materializing init.mp4 exceeded the producer deadline".to_owned(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || tokio::time::timeout(remaining, &mut notified)
                    .await
                    .is_err()
            {
                demand.retry_pending();
                return Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                });
            }
        }
    }

    pub(super) async fn serve_segment(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        // Materialized → serve immediately: the overwhelmingly common case,
        // and until now the invisible one. It never reaches the wait pool, so
        // nothing counted it: a node serving a hundred concurrent cache hits
        // and one serving none reported the same thing, and "is this node
        // busy" was a question only `ps` could answer.
        //
        // The guard, not a counter pair, because the request future is dropped
        // when a client goes away and a decrement on the success path leaks on
        // exactly the disconnects worth seeing.
        let _serving = self.shared.pool.metrics_handle().serving();
        if let Some(ready) = self.open_materialized(rendition, index, &delivery).await? {
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure_cause() {
            return Err(VodError::ProducerFailed(cause));
        }
        self.blocked_wait(rendition, session_id, index, budget, delivery)
            .await
    }

    /// The blocking half of a segment GET: register on the wait pool, close
    /// the lost-wakeup window, and sleep until one of the four named ends.
    pub(super) async fn blocked_wait(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        let deadline = tokio::time::Instant::now() + budget;
        let key = WaitKey {
            rendition: rendition.key.clone(),
            index,
        };
        let _wake = DemandWake(Arc::clone(rendition));
        // Reserve synchronously before any await: a seek storm must not park
        // unbounded unadmitted futures behind the manifest or a disk open.
        let (mut wait, demand) = {
            // The watchdog holds this same lock through fail_entry: an
            // admitted receiver is bound to its deadline before it is visible
            // to expiry, including on a multithread runtime.
            let mut clocks = rendition.demand_since.lock().expect("demand lock");
            let wait = self
                .shared
                .pool
                .register(key, session_id)
                // The class travels with the refusal. `blocked_wait` is the only
                // production caller of `register`, so this line is the whole
                // seam between the pool knowing which cap fired and a client
                // or an operator ever finding out.
                .map_err(VodError::Busy)?;
            let demand = self
                .shared
                .arm_materialize_watchdog_locked(rendition, index, &mut clocks);
            (wait, demand)
        };
        // Admission comes before any persistent demand or deadline state. A
        // refused GET is not work owed by this rendition.
        rendition.kick();
        // Register before rechecking bytes/failure to close the lost-wakeup
        // window. The registration stays alive through open_materialized,
        // protecting a just-published target from concurrent eviction.
        if let Some(ready) = self.open_materialized(rendition, index, &delivery).await? {
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure_cause() {
            return Err(VodError::ProducerFailed(cause));
        }
        if demand.expired() {
            return Err(VodError::ProducerFailed(format!(
                "materializing {} exceeded the producer deadline",
                segment_name(u64::from(index))
            )));
        }
        #[cfg(test)]
        let waiting_pause = self
            .shared
            .segment_ready_pause
            .lock()
            .expect("segment pause lock")
            .clone();
        #[cfg(test)]
        if let Some(pause) = waiting_pause {
            // Phase one proves the post-registration recheck has finished;
            // the Ready arm supplies phases two and three around file open.
            pause.wait().await;
        }
        let outcome = wait
            .wait(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
        match outcome {
            WaitOutcome::Ready => {
                #[cfg(test)]
                let pause = self
                    .shared
                    .segment_ready_pause
                    .lock()
                    .expect("segment pause lock")
                    .clone();
                #[cfg(test)]
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
                match self.open_materialized(rendition, index, &delivery).await? {
                    Some(ready) => Ok(ready),
                    // An eviction already committed before registration, or an
                    // external unlink, can still remove the file. Publication
                    // during this admitted wait is protected by its narrow pin.
                    None => Err(VodError::Pending {
                        retry_after: PENDING_RETRY_AFTER,
                    }),
                }
            }
            WaitOutcome::Deadline => {
                if demand.expired() {
                    return Err(VodError::ProducerFailed(format!(
                        "materializing {} exceeded the producer deadline",
                        segment_name(u64::from(index))
                    )));
                }
                demand.retry_pending();
                Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                })
            }
            WaitOutcome::ProducerFailed(cause) => Err(VodError::ProducerFailed(cause)),
            // The rendition went away under the wait; the session's own
            // tombstone (if any) is the more precise cause on the next GET.
            WaitOutcome::Gone => Err(VodError::Gone(Terminal::Deleted)),
        }
    }

    /// Open one materialized segment under the manifest lock, so eviction
    /// cannot unlink it between the check and the open.
    pub(super) async fn open_materialized(
        &self,
        rendition: &Arc<Rendition>,
        index: u32,
        delivery: &Arc<crate::meter::Meter>,
    ) -> Result<Option<SegmentReady>, VodError> {
        if self.source_changed(rendition) {
            let cause = "source changed after the fragment index was selected".to_owned();
            record_failure(
                &self.shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(VodError::ProducerFailed(cause));
        }
        let manifest = rendition.manifest.lock().await;
        let Some(SegState::Materialized { at_ms, .. }) = manifest.state(index) else {
            return Ok(None);
        };
        let path = rendition.dir.path().join(segment_name(u64::from(index)));
        // The materialization instant is part of the etag: key-index-length
        // alone collides across an evict-and-regenerate whose bytes differ
        // while its length happens to match.
        match open_ready(
            &path,
            &format!("{}-{index}-{at_ms}", rendition.key),
            delivery,
        )
        .await
        {
            Ok(ready) => Ok(Some(ready)),
            // The manifest lied — treat as planned; reconcile repairs it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VodError::Io(error)),
        }
    }

    fn source_changed(&self, rendition: &Rendition) -> bool {
        rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
    }
}
