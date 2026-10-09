use super::*;

impl plurx_core::playback::continuous_quality::QualityReservationPublisher for VodServe {
    /// Publish reservations only while their verified bytes cannot be evicted
    /// or replaced. Old dependencies survive takeover even without a producer.
    async fn commit_quality_reservations(
        &self,
        owner_node_id: &str,
        expected: &plurx_core::store::QualityLedgerSnapshot,
        candidate: &plurx_core::playback::continuous_quality::QualityLedger,
        family: &plurx_core::transcode::VodPresentationFamily,
        now_ms: i64,
        deadline: Instant,
    ) -> Result<bool, String> {
        self.commit_quality_reservations_bound(
            QualityReservationCommit {
                owner_node_id,
                expected,
                candidate,
                family,
                now_ms,
                deadline,
            },
            None,
        )
        .await
    }
}

pub(super) struct QualityReservationCommit<'a> {
    pub owner_node_id: &'a str,
    pub expected: &'a plurx_core::store::QualityLedgerSnapshot,
    pub candidate: &'a plurx_core::playback::continuous_quality::QualityLedger,
    pub family: &'a plurx_core::transcode::VodPresentationFamily,
    pub now_ms: i64,
    pub deadline: Instant,
}

impl VodServe {
    pub(super) async fn commit_quality_reservations_bound(
        &self,
        commit: QualityReservationCommit<'_>,
        parent: Option<(&str, &ResponseOwner)>,
    ) -> Result<bool, String> {
        let QualityReservationCommit {
            owner_node_id,
            expected,
            candidate,
            family,
            now_ms,
            deadline,
        } = commit;
        let commit = async {
            if !candidate.valid()
                || !expected.ledger.valid()
                || expected.revision <= 0
                || candidate.generation != expected.ledger.generation
                || candidate.attachment != expected.ledger.attachment
                || candidate.attachment.family_id != family.id()
                || candidate.control_epoch < expected.ledger.control_epoch
                || candidate.shared_audio_rendition_id()
                    != expected.ledger.shared_audio_rendition_id()
                || expected
                    .ledger
                    .shared_audio_rendition_id()
                    .is_some_and(|id| {
                        family
                            .audio()
                            .is_none_or(|audio| audio.rendition_id() != id)
                    })
                || candidate
                    .shared_audio_reserved()
                    .iter()
                    .any(|interval| !expected.ledger.shared_audio_reserved().contains(interval))
                || self
                    .shared
                    .cluster_node_id
                    .as_deref()
                    .is_some_and(|node| node != owner_node_id)
            {
                return Err("continuous reservation owner or family changed".into());
            }
            let old: Vec<_> = expected
                .ledger
                .transactions
                .iter()
                .flat_map(|transaction| &transaction.reserved)
                .collect();
            let current: Vec<_> = candidate
                .transactions
                .iter()
                .flat_map(|transaction| &transaction.reserved)
                .collect();
            let mut keys: Vec<_> = old
                .iter()
                .chain(&current)
                .copied()
                .chain(expected.ledger.shared_audio_reserved().iter())
                .chain(candidate.shared_audio_reserved().iter())
                .map(|interval| interval.rendition_id.as_str())
                .collect();
            if let Some(audio) = family.audio() {
                keys.push(audio.rendition_id());
            }
            keys.sort_unstable();
            keys.dedup();
            let mut guards = Vec::with_capacity(keys.len());
            for key in keys {
                guards.push(self.shared.rendition_build_gate(key).lock_owned().await);
            }
            let mut derived_audio = Vec::new();
            for interval in current.iter().filter(|interval| !old.contains(interval)) {
                let rung = family
                    .video()
                    .rungs()
                    .iter()
                    .find(|rung| rung.rendition_id() == interval.rendition_id)
                    .ok_or("reservation target is outside its verified video family")?;
                let rendition = self
                    .shared
                    .renditions
                    .lock()
                    .await
                    .get(&interval.rendition_id)
                    .cloned()
                    .ok_or("reserved rendition is not locally attached")?;
                if rendition.closed.load(Relaxed)
                    || rendition.failure().is_some()
                    || rendition
                        .source
                        .as_ref()
                        .is_none_or(|source| !source.unchanged())
                    || rendition.recipe.encoding.as_ref().is_none_or(|encoding| {
                        encoding.shared_audio.is_some()
                            || encoding.source_object_version != rung.source_object_version()
                            || encoding.grid != rung.grid()
                            || encoding.plan.options().input_has_audio
                            || encoding.plan.options().video_sample_envelope
                                != plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
                            || rendition.recipe.source_object_version.as_deref()
                                != Some(encoding.source_object_version.as_str())
                    })
                {
                    return Err("reservation source or rendition is no longer verified".into());
                }
                let entry = rendition
                    .plan
                    .entries
                    .iter()
                    .find(|entry| {
                        entry.start_ticks == interval.from_tick
                            && entry.end_ticks() == interval.through_tick
                    })
                    .ok_or("reservation does not name one exact planned entry")?;
                if interval.timescale != rendition.timescale
                    || !rendition
                        .manifest
                        .lock()
                        .await
                        .state(entry.index)
                        .is_some_and(SegState::is_materialized)
                {
                    return Err("reserved media is not materialized on its exact clock".into());
                }
                let init = read_quality_artifact(&rendition.dir.path().join(INIT_NAME), 256 * 1024)
                    .await?;
                let media = read_quality_artifact(
                    &rendition
                        .dir
                        .path()
                        .join(segment_name(u64::from(entry.index))),
                    interval.byte_length,
                )
                .await?;
                verify_cached_quality_interval(&init, &media, rung, interval)?;
                if rendition
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
                {
                    return Err("reservation source changed during artifact verification".into());
                }
                if let Some(audio) = family.audio() {
                    let soundtrack = self
                        .shared
                        .renditions
                        .lock()
                        .await
                        .get(audio.rendition_id())
                        .cloned()
                        .ok_or("shared AAC rendition is not locally attached")?;
                    if soundtrack.closed.load(Relaxed)
                        || soundtrack.failure().is_some()
                        || soundtrack
                            .source
                            .as_ref()
                            .is_none_or(|source| !source.unchanged())
                        || soundtrack.recipe.encoding.as_ref().is_none_or(|encoding| {
                            encoding.source_object_version != audio.source_object_version()
                                || encoding
                                    .shared_audio
                                    .as_ref()
                                    .is_none_or(|recipe| recipe.digest() != audio.recipe_id())
                                || soundtrack.recipe.source_object_version.as_deref()
                                    != Some(encoding.source_object_version.as_str())
                        })
                    {
                        return Err("shared AAC source or recipe is no longer verified".into());
                    }
                    let dependencies = family
                        .shared_audio_dependencies(
                            &interval.rendition_id,
                            &rendition.plan,
                            entry.index,
                            Some(&soundtrack.plan),
                        )
                        .map_err(|error| error.to_string())?;
                    let init =
                        read_quality_artifact(&soundtrack.dir.path().join(INIT_NAME), 256 * 1024)
                            .await?;
                    for index in dependencies {
                        let entry = soundtrack
                            .plan
                            .entry(index)
                            .ok_or("shared AAC plan changed")?;
                        if !soundtrack
                            .manifest
                            .lock()
                            .await
                            .state(index)
                            .is_some_and(SegState::is_materialized)
                        {
                            return Err("shared AAC dependency is not materialized".into());
                        }
                        let media = read_quality_artifact(
                            &soundtrack.dir.path().join(segment_name(u64::from(index))),
                            16 * 1024 * 1024,
                        )
                        .await?;
                        let dependency =
                            plurx_core::playback::continuous_quality::QualityInterval {
                                artifact_id: hex::encode(Sha256::digest(&media)),
                                rendition_id: audio.rendition_id().into(),
                                timescale: soundtrack.timescale,
                                from_tick: entry.start_ticks,
                                through_tick: entry.end_ticks(),
                                byte_length: media.len() as u64,
                            };
                        verify_cached_shared_audio_interval(
                            &init,
                            &media,
                            audio,
                            &dependency,
                            index as usize + 1 == soundtrack.plan.len(),
                        )?;
                        if !derived_audio.contains(&dependency) {
                            derived_audio.push(dependency);
                        }
                    }
                    if soundtrack
                        .source
                        .as_ref()
                        .is_none_or(|source| !source.unchanged())
                    {
                        return Err("shared AAC source changed during artifact verification".into());
                    }
                }
            }
            let lifecycle = if let Some((session_id, response_owner)) = parent {
                let guard = Arc::clone(&response_owner.lifecycle).lock_owned().await;
                let sessions = self.shared.sessions.lock().await;
                if sessions.get(session_id).is_none_or(|session| {
                    session.tombstone.is_some()
                        || !Arc::ptr_eq(&session.incarnation, &response_owner.incarnation)
                        || !session.owns_response_media(response_owner)
                }) {
                    return Err("quality reservation parent attachment changed".into());
                }
                Some(guard)
            } else {
                None
            };
            let store = Arc::clone(&self.shared.store);
            let mut candidate = candidate.clone();
            candidate
                .reserve_shared_audio(&derived_audio)
                .map_err(|error| error.to_string())?;
            let owner = owner_node_id.to_owned();
            let revision = expected.revision;
            let submitted_at_ms = now_ms.max(super::now_ms());
            // A caller deadline may lose the acknowledgement, never release
            // these physical gates while a queued Store mutation can still land.
            let settlement = tokio::spawn(async move {
                let result = store
                    .write_quality_ledger(&candidate, &owner, revision, submitted_at_ms)
                    .await
                    .map_err(|error| format!("publishing continuous reservations: {error}"));
                drop(lifecycle);
                drop(guards);
                result
            });
            settlement
                .await
                .map_err(|error| format!("continuous reservation settlement failed: {error}"))?
        };
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), commit)
            .await
            .map_err(|_| "continuous reservation exceeded its inherited deadline".to_owned())?
    }
}

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
        // One registry read for the rendition, its owner and the session's
        // frozen master choice, so all three describe the same incarnation.
        // Same answers as `session_rendition`: a tombstoned or rendition-less
        // session has no facts.
        let (rendition, owner, sdr_master_codecs) = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            (
                session.live_rendition().map(Arc::clone)?,
                session.response_owner(),
                session.sdr_master_codecs,
            )
        };
        Some(VodHlsFacts {
            sdr_master_codecs,
            file: rendition.recipe.file.clone(),
            audio_index: rendition.recipe.audio_index,
            aac: rendition.recipe.aac,
            audio_delivery: rendition.recipe.audio_delivery.clone(),
            preserve_dolby_vision: rendition.recipe.video.preserves_dolby_vision(),
            convert_dolby_vision: rendition.recipe.video.converts_dolby_vision(),
            encoding: rendition.recipe.encoding.clone(),
            response_owner: owner,
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
        self.shared
            .preparation_storage
            .reconcile(&self.shared)
            .await;
        self.shared.preparation_storage.maintain(&self.shared).await;
        self.shared.reconcile_obsolete_encoded_generations().await;
        self.shared
            .retained_artifacts
            .collect(&self.shared.base)
            .await;

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
                    if let Some(session) = sessions.get(&candidate.session_id) {
                        session.invalidate_observational_attachment();
                    }
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

        let expired_grants = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.tombstone.is_none()
                        && session
                            .passive_grant
                            .as_ref()
                            .is_some_and(|grant| !grant.live())
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        for id in expired_grants {
            self.begin_end(&id, Terminal::PauseExpired).await;
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
                        && session.rendition.is_some()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                })
                .map(|(id, session)| (id.clone(), Arc::clone(&session.lifecycle)))
                .collect()
        };
        for (id, lifecycle) in expired {
            let lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
            let expired_session = {
                let mut sessions = self.shared.sessions.lock().await;
                let still_expired = sessions.get(&id).is_some_and(|session| {
                    Arc::ptr_eq(&session.lifecycle, &lifecycle)
                        && session.tombstone.is_none()
                        && now.duration_since(*session.last_touch.lock().expect("touch lock"))
                            > SESSION_IDLE_TTL
                });
                if still_expired {
                    let retained = sessions.get_mut(&id).filter(|session| {
                        session
                            .passive_grant
                            .as_ref()
                            .is_some_and(|grant| grant.live())
                    });
                    if let Some(session) = retained {
                        // Stale response and observation owners cannot publish
                        // after this detach; the retained artifact goes with
                        // the rendition, as terminal cleanup releases both.
                        session.invalidate_observational_attachment();
                        session.abort_staged_preparation();
                        session.incarnation = Arc::new(());
                        session.marker_destinations.clear();
                        session.retained_output = None;
                        // The private child readers share the parent's
                        // authority, so they detach with its rendition.
                        Some((
                            std::mem::take(&mut session.children),
                            session.rendition.take(),
                        ))
                    } else {
                        if let Some(session) = sessions.get(&id) {
                            session.invalidate_observational_attachment();
                        }
                        sessions.remove(&id).map(|mut session| {
                            (
                                std::mem::take(&mut session.children),
                                session.rendition.take(),
                            )
                        })
                    }
                } else {
                    None
                }
            };
            if let Some((children, rendition)) = expired_session {
                let shared = Arc::clone(&self.shared);
                let cleanup = spawn_cancellation_independent(async move {
                    // Keep the exact per-id gate through all child detaches.
                    // Cancelling maintain cannot let a same-id resurrection
                    // race the departed parent's still-attached reader graph.
                    let _lifecycle = lifecycle_guard;
                    for child in children {
                        child.detach(&shared.pool).await;
                    }
                    if let Some(rendition) = rendition {
                        rendition.detach_reader(&shared.pool, &id).await;
                        rendition.kick();
                        tracing::info!(
                            target: "plurxd::vodserve",
                            session = %session_log_id(&id),
                            rendition = %rendition.key,
                            "vod session idle-reaped (sliding TTL)"
                        );
                    }
                });
                let _ = cleanup.await;
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
        let mut offered = false;
        for rendition in renditions {
            rendition.kick();
            if !offered {
                offered = crate::vodserve::retained::RetainedArtifactRegistry::offer(
                    &self.shared,
                    &rendition,
                );
            }
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
        let publication = self.session_media_rendition(session_id, None).await?;
        let result = match publication.result {
            Ok(Some(reader)) => Ok((reader.rendition, reader.block_budget, reader.delivery)),
            Ok(None) => return None,
            Err(error) => Err(error),
        };
        Some(VodPublication {
            result,
            owner: publication.owner,
        })
    }

    /// Child lookup uses only the exact parent's owned graph. A cache key is
    /// never an authorization capability, including after a detach/rebind.
    pub(super) async fn session_media_rendition(
        &self,
        session_id: &str,
        child_rendition_id: Option<&str>,
    ) -> Option<VodPublication<Option<ResolvedMediaReader>>> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        let mut owner = session.response_owner();
        let result = if let Some(cause) = session.tombstone {
            Err(VodError::Gone(cause))
        } else {
            let selected = match child_rendition_id {
                None => session.live_rendition().map(Arc::clone),
                Some(id) => session
                    .children
                    .iter()
                    .find(|child| {
                        child.rendition.key == id
                            && child.rendition.recipe.file.id == session.file.id
                            && child
                                .rendition
                                .source
                                .as_ref()
                                .map(|source| source.object_version())
                                == session
                                    .live_rendition()
                                    .and_then(|root| root.source.as_ref())
                                    .map(|source| source.object_version())
                    })
                    .map(|child| {
                        owner.media_child = Some(MediaReaderResponseOwner {
                            reader_id: child.reader_id.clone(),
                            rendition: Arc::clone(&child.rendition),
                        });
                        Arc::clone(&child.rendition)
                    }),
            };
            Ok(selected.map(|rendition| ResolvedMediaReader {
                rendition,
                block_budget: session.block_budget,
                delivery: Arc::clone(&session.delivery),
            }))
        };
        Some(VodPublication { result, owner })
    }

    pub(super) async fn serve_init(
        &self,
        rendition: &Arc<Rendition>,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        self.serve_init_with_read_custody(rendition, budget, delivery, None)
            .await
    }

    pub(super) async fn serve_init_with_read_custody(
        &self,
        rendition: &Arc<Rendition>,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
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
        // A cached immutable init needs no producer. In particular, checking
        // an idle controlled rung must not wake it at the ordinary playhead
        // before its preparation frontier has been admitted.
        let mut demand = None;
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
                let ready = open_with_read_custody(
                    &path,
                    &format!("{}-init", rendition.key),
                    &delivery,
                    custody,
                )
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
            let demand = demand.get_or_insert_with(|| {
                let demand = self
                    .shared
                    .arm_materialize_watchdog(rendition, INIT_DEMAND_INDEX);
                rendition.kick();
                demand
            });
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
        self.serve_segment_for(rendition, (session_id, session_id), index, budget, delivery)
            .await
    }

    pub(super) async fn serve_segment_for(
        &self,
        rendition: &Arc<Rendition>,
        reader: (&str, &str),
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        self.serve_segment_for_with_read_custody(rendition, reader, index, budget, delivery, None)
            .await
    }

    pub(super) async fn serve_segment_with_read_custody(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Result<SegmentReady, VodError> {
        self.serve_segment_for_with_read_custody(
            rendition,
            (session_id, session_id),
            index,
            budget,
            delivery,
            custody,
        )
        .await
    }

    async fn serve_segment_for_with_read_custody(
        &self,
        rendition: &Arc<Rendition>,
        reader: (&str, &str),
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
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
        if let Some(ready) = self
            .open_materialized_select(rendition, index, &delivery, custody)
            .await?
        {
            return Ok(ready);
        }
        if let Some(cause) = rendition.failure_cause() {
            return Err(VodError::ProducerFailed(cause));
        }
        self.blocked_wait_for_with_read_custody(rendition, reader, index, budget, delivery, custody)
            .await
    }

    /// The blocking half of a segment GET: register on the wait pool, close
    /// the lost-wakeup window, and sleep until one of the four named ends.
    #[cfg(test)]
    pub(super) async fn blocked_wait(
        &self,
        rendition: &Arc<Rendition>,
        session_id: &str,
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        self.blocked_wait_for(rendition, (session_id, session_id), index, budget, delivery)
            .await
    }

    #[cfg(test)]
    async fn blocked_wait_for(
        &self,
        rendition: &Arc<Rendition>,
        reader: (&str, &str),
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<SegmentReady, VodError> {
        self.blocked_wait_for_with_read_custody(rendition, reader, index, budget, delivery, None)
            .await
    }

    async fn blocked_wait_for_with_read_custody(
        &self,
        rendition: &Arc<Rendition>,
        reader: (&str, &str),
        index: u32,
        budget: Duration,
        delivery: Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
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
                .register_reader(key, reader.0, reader.1)
                // Preserve the parent-cap refusal class for HTTP and operator
                // diagnostics; a refused child creates no persistent demand.
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
        if let Some(ready) = self
            .open_materialized_select(rendition, index, &delivery, custody)
            .await?
        {
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
        self.shared
            .hooks
            .get()
            .after_segment_wait_registered()
            .await;
        let outcome = wait
            .wait(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
        match outcome {
            WaitOutcome::Ready => {
                self.shared.hooks.get().before_segment_ready_open().await;
                match self
                    .open_materialized_select(rendition, index, &delivery, custody)
                    .await?
                {
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

    async fn open_materialized_select(
        &self,
        rendition: &Arc<Rendition>,
        index: u32,
        delivery: &Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Result<Option<SegmentReady>, VodError> {
        match custody {
            Some(custody) => {
                self.open_materialized_with_read_custody(rendition, index, delivery, Some(custody))
                    .await
            }
            None => self.open_materialized(rendition, index, delivery).await,
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
        self.open_materialized_with_read_custody(rendition, index, delivery, None)
            .await
    }

    pub(super) async fn open_materialized_with_read_custody(
        &self,
        rendition: &Arc<Rendition>,
        index: u32,
        delivery: &Arc<crate::meter::Meter>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
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
        match open_with_read_custody(
            &path,
            &format!("{}-{index}-{at_ms}", rendition.key),
            delivery,
            custody,
        )
        .await
        {
            Ok(mut ready) => {
                ready.observed_media_duration_ms = plan_media_duration_ms(rendition, index);
                Ok(Some(ready))
            }
            // The manifest lied — treat as planned; reconcile repairs it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VodError::Io(error)),
        }
    }

    /// Source-only response boundary: the current no-follow path must still
    /// identify the exact held producer input, even if scanning has not updated
    /// the stored file facts. This cannot queue or rebuild a fragment index.
    #[allow(dead_code)] // The private Source HTTP actor consumer is being integrated.
    pub(crate) async fn source_response_physical_fence(
        &self,
        owner: &ResponseOwner,
    ) -> Result<crate::fragment_index_cluster::SourceFence, String> {
        let rendition = owner
            .rendition
            .as_ref()
            .ok_or_else(|| "Source rendition is absent".to_owned())?;
        if !rendition.key.starts_with("source-") {
            return Err("Source response has no Source rendition".into());
        }
        let held = rendition
            .source
            .as_ref()
            .ok_or_else(|| "Source held input is absent".to_owned())?;
        if !held.unchanged() {
            return Err("Source held input changed".into());
        }
        let current = crate::fragment_index_cluster::open_source_playback_fence(
            &rendition.recipe.file,
            Some(held.object_version()),
        )
        .await?;
        if !held.unchanged() {
            return Err("Source held input changed during response admission".into());
        }
        Ok(current)
    }

    pub(super) fn source_changed(&self, rendition: &Rendition) -> bool {
        rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
    }
}

/// Local delivery keeps the existing Tokio opener. Only closed Source custody
/// selects a filesystem job retaining its actual response owner.
async fn open_with_read_custody(
    path: &std::path::Path,
    etag_stem: &str,
    delivery: &Arc<crate::meter::Meter>,
    custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
) -> io::Result<SegmentReady> {
    match custody {
        Some(custody) => custody.open_ready(path, etag_stem, delivery).await,
        None => open_ready(path, etag_stem, delivery).await,
    }
}

pub(super) async fn read_quality_artifact(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().await.map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err("continuous artifact is missing or exceeds its byte bound".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit || bytes.len() as u64 != metadata.len() {
        return Err("continuous artifact changed while being read".into());
    }
    Ok(bytes)
}

pub(super) fn verify_cached_quality_interval(
    init_bytes: &[u8],
    media: &[u8],
    rung: &plurx_core::transcode::VodVideoRung,
    interval: &plurx_core::playback::continuous_quality::QualityInterval,
) -> Result<(), String> {
    if interval.rendition_id != rung.rendition_id()
        || !interval.matches_bytes(media)
        || hex::encode(Sha256::digest(init_bytes)) != rung.init_id()
    {
        return Err("continuous artifact bytes do not match their immutable identities".into());
    }
    let mut reader = FragmentReader::new();
    reader.push(init_bytes);
    let Some(Unit::Init(init)) = reader.next_unit().map_err(|error| error.to_string())? else {
        return Err("continuous artifact has no complete init".into());
    };
    if reader.buffered() != 0
        || init.tracks.len() != 1
        || plurx_core::fmp4::avc_sample_entry_facts(&init)
            .map_err(|error| error.to_string())?
            .as_ref()
            != Some(rung.facts())
    {
        return Err("continuous artifact init shape changed".into());
    }
    let video = init
        .video()
        .ok_or("continuous artifact has no video track")?;
    if video.timescale != interval.timescale || video.timescale != rung.grid().numerator {
        return Err("continuous artifact clock changed".into());
    }
    reader.push(media);
    let mut next = interval.from_tick;
    let mut fragments = 0;
    while let Some(unit) = reader.next_unit().map_err(|error| error.to_string())? {
        let Unit::Fragment(fragment) = unit else {
            return Err("continuous media contains an unexpected init or trailer".into());
        };
        let track = fragment
            .track(video.id)
            .ok_or("continuous media lost its video track")?;
        if fragment.tracks.len() != 1
            || track.samples().next().is_none()
            || track.base_decode_time != next
            || (fragments == 0 && !plurx_core::fmp4::classify(&fragment, &init).is_clean())
            || track
                .samples()
                .any(|sample| sample.cto != 0 || sample.duration != rung.grid().denominator)
        {
            return Err("continuous media does not match its clean rational sample grid".into());
        }
        next = next
            .checked_add(track.duration())
            .ok_or("continuous media clock overflow")?;
        if next > interval.through_tick {
            return Err("continuous media exceeds its declared interval".into());
        }
        fragments += 1;
    }
    if fragments == 0 || reader.buffered() != 0 || next != interval.through_tick {
        return Err("continuous media does not complete its declared interval".into());
    }
    Ok(())
}

/// Verify actual published AAC bytes, including the final packet trim. No
/// fragment number or manifest-only claim can substitute for sample evidence.
pub(super) fn verify_cached_shared_audio_interval(
    init_bytes: &[u8],
    media: &[u8],
    audio: &plurx_core::transcode::VodSharedAudioRendition,
    interval: &plurx_core::playback::continuous_quality::QualityInterval,
    final_interval: bool,
) -> Result<(), String> {
    if interval.rendition_id != audio.rendition_id()
        || !interval.matches_bytes(media)
        || interval.timescale != plurx_core::transcode::VOD_AUDIO_RATE
        || hex::encode(Sha256::digest(init_bytes)) != audio.init_id()
    {
        return Err("shared AAC artifact bytes or identities changed".into());
    }
    let mut reader = FragmentReader::new();
    reader.push(init_bytes);
    let Some(Unit::Init(init)) = reader.next_unit().map_err(|error| error.to_string())? else {
        return Err("shared AAC artifact has no complete init".into());
    };
    if reader.buffered() != 0
        || init.tracks.len() != 1
        || init.tracks[0].kind != plurx_core::fmp4::TrackKind::Audio
        || init.tracks[0].timescale != interval.timescale
        || &plurx_core::fmp4::aac_lc_sample_entry_facts(&init).map_err(|error| error.to_string())?
            != audio.facts()
    {
        return Err("shared AAC init shape or clock changed".into());
    }
    reader.push(media);
    let mut next = interval.from_tick;
    let mut fragments = 0;
    while let Some(unit) = reader.next_unit().map_err(|error| error.to_string())? {
        let Unit::Fragment(fragment) = unit else {
            return Err("shared AAC media contains an unexpected init or trailer".into());
        };
        let track = fragment
            .track(init.tracks[0].id)
            .ok_or("shared AAC lost its track")?;
        if fragment.tracks.len() != 1 || track.sample_count() == 0 || track.base_decode_time != next
        {
            return Err("shared AAC sample clock is discontinuous".into());
        }
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                let through = offset
                    .checked_add(sample.size as usize)
                    .ok_or("shared AAC payload overflow")?;
                let end = next
                    .checked_add(u64::from(sample.duration))
                    .ok_or("shared AAC clock overflow")?;
                if sample.duration == 0
                    || sample.duration > 1_024
                    || sample.cto != 0
                    || sample.size == 0
                    || offset < fragment.mdat_payload.start
                    || through > fragment.mdat_payload.end
                    || through > fragment.bytes.len()
                    || end > interval.through_tick
                    || (sample.duration != 1_024
                        && (!final_interval || end != interval.through_tick))
                {
                    return Err(
                        "shared AAC sample payload or final trim differs from its plan".into(),
                    );
                }
                next = end;
                offset = through;
            }
        }
        fragments += 1;
    }
    if fragments == 0 || reader.buffered() != 0 || next != interval.through_tick {
        return Err("shared AAC media does not complete its exact interval".into());
    }
    Ok(())
}
