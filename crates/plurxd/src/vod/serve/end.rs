use super::*;

impl VodServe {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_terminal_cleanup(
        &self,
        session_id: String,
        cleanup: Arc<TerminalCleanup>,
        rendition: Arc<Rendition>,
        file_id: i64,
        height: i64,
        kind: SessionKind,
        cause: Terminal,
        terminal_commit: Option<Box<crate::playback_control::TerminalCommitReceipt>>,
    ) {
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let _completion = TerminalCleanupGuard(Arc::clone(&cleanup));
            #[cfg(test)]
            let terminal_detach_pause = shared
                .terminal_detach_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(pause) = terminal_detach_pause {
                pause.wait().await;
                pause.wait().await;
            }
            rendition.detach_reader(&shared.pool, &session_id).await;
            rendition.kick();
            let serve = VodServe { shared };
            serve.emit_lifecycle(
                &session_id,
                file_id,
                height,
                kind,
                "session_end",
                Some(terminal_reason(cause)),
            );
            tracing::info!(
                session = %session_log_id(&session_id),
                rendition = %rendition.key,
                "vod session ended for good: {cause:?}"
            );
            if let Some(terminal_commit) = terminal_commit {
                terminal_commit.retry();
                let _ = terminal_commit.wait().await;
            }
            // This task owns the exact cleanup identity. Compact before its
            // guard publishes completion so every waiter observing finished
            // cleanup also observes that the registry no longer retains the
            // rendition/media/process graph. A resurrected replacement has a
            // different cleanup pointer and is left untouched.
            let mut sessions = serve.shared.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                let exact_cleanup = session
                    .terminal_cleanup
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cleanup));
                let exact_rendition = session
                    .rendition
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &rendition));
                if session.tombstone.is_some() && exact_cleanup && exact_rendition {
                    session.rendition = None;
                }
            }
            drop(sessions);
            // `TerminalCleanupGuard` publishes completion on drop. Release
            // the task's own heavyweight captures first so a waiter that
            // observes completion cannot still race this future's teardown.
            drop(rendition);
            drop(serve);
        });
    }

    /// End one session for good with a cause; `true` if it was ours.
    /// Idempotent for authoritative causes. `Replaced` is also the fail-closed
    /// provisional cause used when lease reconciliation cannot yet read the
    /// durable winner; a later non-Replaced Store cause may refine only that
    /// placeholder, never another concrete first writer.
    pub(super) async fn begin_end(
        &self,
        session_id: &str,
        cause: Terminal,
    ) -> Option<Arc<TerminalCleanup>> {
        let (lifecycle, incarnation) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            (
                Arc::clone(&session.lifecycle),
                Arc::clone(&session.incarnation),
            )
        };
        let _lifecycle = lifecycle.lock().await;
        let (cleanup, work) = {
            let mut sessions = self.shared.sessions.lock().await;
            let session = sessions.get_mut(session_id)?;
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle)
                || !Arc::ptr_eq(&session.incarnation, &incarnation)
            {
                return None;
            }
            let terminal_cause = match session.tombstone {
                Some(Terminal::Replaced) if cause != Terminal::Replaced => {
                    tracing::info!(
                        session = %session_log_id(session_id),
                        durable_cause = cause.durable_reason(),
                        "refined provisional VOD terminal cause from durable route"
                    );
                    session.tombstone = Some(cause);
                    cause
                }
                Some(existing) => existing,
                None => {
                    session.tombstone = Some(cause);
                    cause
                }
            };
            // Idempotent, and correct on the refinement branch too: the slot
            // was already aborted when the provisional tombstone was written.
            session.abort_staged_preparation();
            if let Some(cleanup) = session.terminal_cleanup.as_ref() {
                (Arc::clone(cleanup), None)
            } else {
                let cleanup = Arc::new(TerminalCleanup::new());
                session.terminal_cleanup = Some(Arc::clone(&cleanup));
                match session.rendition.as_ref().map(Arc::clone) {
                    Some(rendition) => {
                        let work = (
                            session_id.to_owned(),
                            rendition,
                            session.file.id,
                            session.target_height,
                            session.kind,
                            terminal_cause,
                        );
                        (cleanup, Some(work))
                    }
                    // A session whose rendition is already gone has nothing to
                    // detach, so this cleanup owns no work and has to say so
                    // here. Leaving it standing unfinished is far worse than
                    // the ending it is refusing: `maintain` waits on every
                    // unfinished cleanup belonging to a tombstoned session
                    // with nothing bounding the wait, so one of these stops
                    // the idle reap, the dormant purge, the tombstone eviction
                    // and every driver kick — node-wide, for good.
                    //
                    // Today the compaction that clears `rendition` writes the
                    // tombstone first under the same pointer identity, so this
                    // arm is unreachable and the assertion states that rather
                    // than trusting it. The completion is what a later change
                    // to that ordering lands on instead of a wedged node.
                    None => {
                        debug_assert!(
                            session.tombstone.is_some(),
                            "a VOD session lost its rendition without a tombstone"
                        );
                        cleanup.complete();
                        (cleanup, None)
                    }
                }
            }
        };
        if let Some((session_id, rendition, file_id, height, kind, cause)) = work {
            self.spawn_terminal_cleanup(
                session_id,
                Arc::clone(&cleanup),
                rendition,
                file_id,
                height,
                kind,
                cause,
                None,
            );
        }
        Some(cleanup)
    }

    pub async fn end(&self, session_id: &str, cause: Terminal) -> bool {
        let Some(cleanup) = self.begin_end(session_id, cause).await else {
            return false;
        };
        cleanup.wait().await;
        true
    }

    /// Install the exact terminal tombstone and transfer reader/event/commit
    /// cleanup to its detached owner without waiting for physical settlement.
    /// Public release uses this before submitting the durable Store End, so a
    /// slow reader detach cannot keep the replicated route active.
    pub(crate) async fn begin_end_detached(&self, session_id: &str, cause: Terminal) -> bool {
        self.begin_end(session_id, cause).await.is_some()
    }

    /// Supersession sweep: end every session with this viewer's
    /// `playback_id` except `keep` (cause [`Terminal::Superseded`]). Called
    /// by the manager on create — for a VOD create AND a legacy one, because
    /// a viewer switching presentations is still one player replacing its own
    /// stream. Scoped by the same user string the legacy sweep uses, so a
    /// colliding `playback_id` from another account ends nothing.
    #[cfg(test)]
    pub(super) async fn supersede(
        &self,
        supersession_user: &str,
        playback_id: &str,
        keep: &str,
    ) -> usize {
        self.supersede_before(supersession_user, playback_id, keep, None)
            .await
            .expect("an unbounded VOD supersession cannot reach a deadline")
    }

    /// Transfer every matching predecessor to terminal cleanup ownership
    /// before returning. Every exact victim lifecycle is acquired within the
    /// deadline before the first tombstone, so timeout/cancellation is a whole
    /// no-op and the final registry mutation plus cleanup handoff is atomic.
    pub async fn supersede_before(
        &self,
        supersession_user: &str,
        playback_id: &str,
        keep: &str,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<usize, VodSupersedeError> {
        let collect = async {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(id, session)| {
                    session.playback_id == playback_id
                        && session.supersession_user == supersession_user
                        && id.as_str() != keep
                        && session.tombstone.is_none()
                })
                .map(|(id, session)| {
                    (
                        id.clone(),
                        Arc::clone(&session.lifecycle),
                        Arc::clone(&session.incarnation),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut victims = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, collect)
                .await
                .map_err(|_| VodSupersedeError::Deadline)?,
            None => collect.await,
        };
        victims.sort_by(|left, right| left.0.cmp(&right.0));
        // Own every exact victim lifecycle before the first mutation. A
        // deadline/cancellation can therefore only happen while the whole
        // operation is still a no-op; after all locks are held, tombstoning,
        // cleanup ownership and registry publication are synchronous.
        let mut lifecycle_guards = Vec::with_capacity(victims.len());
        for (_, lifecycle, _) in &victims {
            let lock = Arc::clone(lifecycle).lock_owned();
            let guard = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, lock)
                    .await
                    .map_err(|_| VodSupersedeError::Deadline)?,
                None => lock.await,
            };
            lifecycle_guards.push(guard);
        }
        let sessions_lock = self.shared.sessions.lock();
        let mut sessions = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, sessions_lock)
                .await
                .map_err(|_| VodSupersedeError::Deadline)?,
            None => sessions_lock.await,
        };
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(VodSupersedeError::Deadline);
        }
        let mut work = Vec::new();
        for (id, lifecycle, incarnation) in victims {
            let Some(session) = sessions.get_mut(&id) else {
                continue;
            };
            if !Arc::ptr_eq(&session.lifecycle, &lifecycle)
                || !Arc::ptr_eq(&session.incarnation, &incarnation)
                || session.tombstone.is_some()
            {
                continue;
            }
            session.tombstone = Some(Terminal::Superseded);
            session.abort_staged_preparation();
            let cleanup = Arc::new(TerminalCleanup::new());
            session.terminal_cleanup = Some(Arc::clone(&cleanup));
            let Some(rendition) = session.rendition.as_ref().map(Arc::clone) else {
                // Same hazard as `begin_end`'s: this victim now carries a
                // tombstone and a cleanup, and skipping to the next one would
                // leave that cleanup unfinished forever, which is the one
                // thing `maintain` waits on without a bound. Nothing to
                // detach means finished, not abandoned.
                cleanup.complete();
                continue;
            };
            work.push((
                id,
                cleanup,
                rendition,
                session.file.id,
                session.target_height,
                session.kind,
            ));
        }
        drop(sessions);
        let ended = work.len();
        for (id, cleanup, rendition, file_id, height, kind) in work {
            self.spawn_terminal_cleanup(
                id,
                cleanup,
                rendition,
                file_id,
                height,
                kind,
                Terminal::Superseded,
                None,
            );
        }
        drop(lifecycle_guards);
        Ok(ended)
    }

    /// Record that the durable row now names this ask, if this engine owns
    /// the session. `false` means it does not, and the caller should ask the
    /// other one.
    pub(crate) async fn record_desired_persisted(&self, session_id: &str, digest: &str) -> bool {
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session
            .control
            .lock()
            .expect("control lock")
            .record_desired_persisted(digest);
        true
    }

    /// Blocked-GET admission counters for the operator surfaces.
    pub(crate) fn blocked_get_metrics_handle(&self) -> Arc<crate::waitpool::BlockedGetMetrics> {
        self.shared.pool.metrics_handle()
    }

    /// Whether this session id is (or ever was) a VOD session here.
    pub async fn owns(&self, session_id: &str) -> bool {
        self.shared.sessions.lock().await.contains_key(session_id)
    }

    /// Every registered session id, tombstoned included — "still addressed
    /// here" is the fact the caller needs, not "still playing".
    pub async fn session_ids(&self) -> Vec<String> {
        self.shared.sessions.lock().await.keys().cloned().collect()
    }

    /// Session ids still live (tombstoned excluded) — what the durable lease
    /// loop may renew.
    pub async fn live_session_ids(&self) -> Vec<String> {
        self.shared
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.tombstone.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// What a lease renewal reports for one live session: the film-time end,
    /// in ms, of the last segment it was served (0 before the first). `None`
    /// for unknown/tombstoned.
    pub async fn frontier_ms(&self, session_id: &str) -> Option<i64> {
        let rendition = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            session.live_rendition().map(Arc::clone)?
        };
        let last_served = {
            let readers = rendition.readers.lock().await;
            readers
                .get(session_id)
                .and_then(|reader| reader.last_served)
        };
        Some(match last_served {
            None => 0,
            Some(index) => rendition
                .plan
                .entry(index)
                .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
                .unwrap_or(0),
        })
    }

    /// VOD-native health for the web diagnostics. Like the live session
    /// status read, this does not touch the session TTL: looking at a panel is
    /// not an authorized media GET and must not keep an abandoned handle alive.
    pub async fn status(&self, session_id: &str) -> Option<VodSessionInfo> {
        self.status_publication(session_id)
            .await
            .and_then(|publication| publication.result.ok())
    }

    /// Status plus the exact VOD incarnation that produced it. HTTP performs
    /// final serving/release admission against this owner, so a reap and
    /// resurrection using the same public id cannot publish stale telemetry.
    pub(crate) async fn status_publication(
        &self,
        session_id: &str,
    ) -> Option<VodPublication<VodSessionInfo>> {
        let (rendition, target_height, owner, delivery, control_snapshot) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            (
                session.live_rendition().map(Arc::clone)?,
                session.target_height,
                session.response_owner(),
                Arc::clone(&session.delivery),
                session.last_control_snapshot.clone(),
            )
        };
        let init_present = rendition.dir.has_init().await;
        let last_served = rendition
            .readers
            .lock()
            .await
            .get(session_id)
            .and_then(|reader| reader.last_served);
        let (
            materialized_segments,
            planned_segments,
            materialized_bytes,
            planned_bytes,
            admitted,
            published_end_ms,
            ready_ahead_end_ms,
            server_ready_state,
            server_ready_anchor_ms,
            server_ready_end_ms,
            server_ready_seconds,
            server_next_ready_start_ms,
            server_next_ready_end_ms,
        ) = {
            let manifest = rendition.manifest.lock().await;
            let end_of_contiguous_run = |start: u32| {
                (start..manifest.len() as u32)
                    .take_while(|index| {
                        manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                    })
                    .last()
                    .and_then(|index| rendition.plan.entry(index))
                    .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
            };
            // A far-seek may materialize one segment after a large hole. The
            // highest numbered file is not a publish frontier and must not be
            // reported as hours of runway; only contiguous bytes count.
            let published = end_of_contiguous_run(0);
            let ready_ahead = end_of_contiguous_run(last_served.unwrap_or(0));
            let ready_anchor = control_snapshot
                .as_ref()
                .map(crate::playback_control::PlaybackDemandSnapshot::buffer_anchor_ms);
            let anchor_index = ready_anchor.and_then(|anchor| {
                let index = entry_containing(&rendition.plan, anchor as f64 / 1_000.0);
                let entry = rendition.plan.entry(index)?;
                let start_ms = ticks_to_ms(entry.start_ticks, rendition.timescale);
                let end_ms = ticks_to_ms(entry.end_ticks(), rendition.timescale);
                (start_ms <= anchor && anchor < end_ms).then_some(index)
            });
            let anchored_end = anchor_index
                .filter(|index| {
                    init_present
                        && manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                })
                .and_then(end_of_contiguous_run);
            let ready_state = if ready_anchor.is_none() {
                "unavailable"
            } else if anchored_end.is_some() {
                "ready"
            } else {
                "missing"
            };
            let later_floor = anchored_end.or(ready_anchor);
            let next_index = init_present
                .then(|| {
                    (0..manifest.len() as u32).find(|index| {
                        let Some(entry) = rendition.plan.entry(*index) else {
                            return false;
                        };
                        let start_ms = ticks_to_ms(entry.start_ticks, rendition.timescale);
                        manifest
                            .state(*index)
                            .is_some_and(SegState::is_materialized)
                            && later_floor.is_some_and(|floor| start_ms > floor)
                    })
                })
                .flatten();
            let next_interval = next_index.and_then(|index| {
                let start = rendition.plan.entry(index)?;
                Some((
                    ticks_to_ms(start.start_ticks, rendition.timescale),
                    end_of_contiguous_run(index)?,
                ))
            });
            (
                manifest.materialized_count(),
                manifest.len(),
                manifest.materialized_bytes(),
                manifest.planned_bytes(),
                manifest.is_admitted(),
                published,
                ready_ahead,
                ready_state,
                ready_anchor,
                anchored_end
                    .or_else(|| (ready_state == "missing").then_some(ready_anchor).flatten()),
                anchored_end
                    .zip(ready_anchor)
                    .map(|(end, anchor)| end.saturating_sub(anchor) as f64 / 1_000.0)
                    .or_else(|| (ready_state == "missing").then_some(0.0)),
                next_interval.map(|(start, _)| start),
                next_interval.map(|(_, end)| end),
            )
        };
        let wait = self.shared.pool.session_snapshot(session_id);
        let failed = rendition.failure();
        let belief = rendition.slot.belief().await;
        let complete = planned_segments > 0 && materialized_segments == planned_segments;
        let (producer_state, producer_hold, suspended) = if failed.is_some() {
            ("failed", None, false)
        } else if complete {
            ("complete", None, false)
        } else {
            match belief {
                Producer::Running { .. } => ("running", None, false),
                Producer::Stopped { reason, .. } => (
                    "held",
                    Some(match reason {
                        crate::prodsched::Hold::Ahead { .. } => "ahead",
                        crate::prodsched::Hold::WorkingSetFull { .. } => "working_set",
                        crate::prodsched::Hold::NoRoom { .. } => "no_room",
                    }),
                    true,
                ),
                // A capacity hold outlives the producer it terminated, so a
                // rendition that wants to produce and cannot still says why.
                Producer::Absent { .. } => {
                    match *rendition.capacity_hold.lock().expect("capacity hold") {
                        Some(crate::prodsched::Hold::NoRoom { .. }) => {
                            ("waiting", Some("no_room"), false)
                        }
                        Some(crate::prodsched::Hold::WorkingSetFull { .. }) => {
                            ("waiting", Some("working_set"), false)
                        }
                        _ => ("waiting", None, false),
                    }
                }
            }
        };
        let fetched_end_ms = last_served
            .and_then(|index| rendition.plan.entry(index))
            .map(|entry| ticks_to_ms(entry.end_ticks(), rendition.timescale))
            .unwrap_or(0);
        let control_demand = control_snapshot
            .as_ref()
            .map(|snapshot| match snapshot.demand {
                crate::playback_control::PlaybackDemand::Active => "active",
                crate::playback_control::PlaybackDemand::Hold => "hold",
                crate::playback_control::PlaybackDemand::End => "end",
            });
        let render_state = control_snapshot
            .as_ref()
            .map(|snapshot| match snapshot.render_state {
                crate::playback_control::RenderState::Starting => "starting",
                crate::playback_control::RenderState::Rendering => "rendering",
                crate::playback_control::RenderState::Waiting => "waiting",
                crate::playback_control::RenderState::Stalled => "stalled",
                crate::playback_control::RenderState::Seeking => "seeking",
                crate::playback_control::RenderState::Ended => "ended",
                crate::playback_control::RenderState::Failed => "failed",
            });
        Some(VodPublication {
            result: Ok(VodSessionInfo {
                id: session_id.to_owned(),
                file_id: rendition.recipe.file.id,
                target_height,
                encoder: "vod",
                tone_map_peak_nits: rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| {
                        encoding.plan.options().tone_map == plurx_core::transcode::ToneMap::Zscale
                    })
                    .map(|encoding| encoding.plan.options().tone_map_peak_nits),
                tone_map_peak_source: rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| {
                        encoding.plan.options().tone_map == plurx_core::transcode::ToneMap::Zscale
                    })
                    .map(|encoding| encoding.plan.options().tone_map_peak_source.name()),
                playlist_shape: "vod",
                producer_state,
                producer_hold,
                producer_failed: failed.as_ref().map(|f| f.cause.clone()),
                producer_decision: failed.as_ref().map(|f| f.decision.status()),
                control_demand,
                reported_position_ms: control_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.position_ms),
                client_runway_ms: control_snapshot
                    .as_ref()
                    .map(crate::playback_control::PlaybackDemandSnapshot::runway_ms),
                render_state,
                server_ready_state,
                server_ready_anchor_ms,
                server_ready_end_ms,
                server_ready_seconds,
                server_next_ready_start_ms,
                server_next_ready_end_ms,
                published_end_ms,
                ready_ahead_end_ms,
                fetched_end_ms,
                fetched_segment: last_served.map(i64::from),
                ahead_seconds: ready_ahead_end_ms.map(|end| (end - fetched_end_ms).max(0) / 1000),
                materialized_segments,
                planned_segments,
                materialized_bytes,
                planned_bytes,
                working_set_bytes: self.shared.working_set.load(Relaxed),
                working_set_budget_bytes: rendition.working_set_budget,
                completed_cache_bytes: self.shared.completed_cache.load(Relaxed),
                admitted,
                // Bytes per second becomes bits per second here, the same
                // conversion and the same units the rolling view publishes.
                delivered_bytes: delivery.total_bytes(),
                delivered_bps: delivery.recent_bps().map(|bytes| bytes * 8),
                delivered_idle_ms: delivery.idle_for_ms(),
                http_wait_count: wait.count,
                http_wait_oldest_ms: wait.oldest_ms,
                http_wait_segment: wait.oldest_segment.map(i64::from),
                status_generated_unix_ms: crate::media_sessions::unix_ms(),
                suspended,
                final_: complete,
            }),
            owner,
        })
    }
}
