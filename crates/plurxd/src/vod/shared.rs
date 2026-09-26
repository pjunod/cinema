use super::*;

impl Shared {
    pub(super) async fn reconcile_obsolete_encoded_generations(&self) -> usize {
        // This lock belongs only to the cleanup loop. Holding it across the
        // exact Store delete and filesystem removal makes cancellation safe:
        // the front candidate remains queued unless both steps finish.
        let mut pending = self.obsolete_encoded_generations.lock().await;
        let available = ENCODED_RECONCILE_BATCH.saturating_sub(pending.len());
        if available > 0 {
            let excluded = pending
                .iter()
                .map(|candidate| candidate.key.clone())
                .collect::<HashSet<_>>();
            let scanner = Arc::clone(&self.encoded_generation_scanner);
            let process = crate::ffmpeg::encoded_process_identity().to_owned();
            match tokio::task::spawn_blocking(move || {
                scanner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .discover(&process, ENCODED_RECONCILE_BATCH, available, &excluded)
            })
            .await
            {
                Ok(discovered) => pending.extend(discovered),
                Err(error) => tracing::warn!(
                    target: "plurxd::vodserve",
                    %error, "encoded VOD generation discovery failed"
                ),
            }
        }

        let mut removed = 0;
        let attempts = pending.len().min(ENCODED_RECONCILE_BATCH);
        for _ in 0..attempts {
            let Some(candidate) = pending.front().cloned() else {
                break;
            };
            if let Err(error) = self.store.forget_rendition_plan(&candidate.key).await {
                tracing::warn!(
                    target: "plurxd::vodserve",
                    rendition = %candidate.key,
                    %error,
                    "cannot forget an obsolete encoded rendition plan"
                );
                // Store availability is shared by the batch. Keep every
                // directory, and retry from this exact candidate next tick.
                break;
            }
            let path = candidate.path.clone();
            let removal = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await;
            let gone = match removal {
                Ok(Ok(())) => true,
                Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => true,
                Ok(Err(error)) => {
                    tracing::warn!(
                        target: "plurxd::vodserve",
                        path = %candidate.path.display(),
                        %error,
                        "cannot remove an obsolete encoded VOD generation"
                    );
                    false
                }
                Err(error) => {
                    tracing::warn!(
                        target: "plurxd::vodserve",
                        %error, "encoded VOD generation removal failed"
                    );
                    false
                }
            };
            if gone {
                pending.pop_front();
                removed += 1;
            } else if let Some(candidate) = pending.pop_front() {
                // One unreadable directory cannot pin every later obsolete
                // generation. Its already-deleted plan makes a retry cheap.
                pending.push_back(candidate);
            }
        }
        if removed > 0 {
            tracing::info!(
                target: "plurxd::vodserve",
                removed, "reconciled obsolete encoded VOD generations"
            );
        }
        removed
    }

    pub(super) async fn terminal_route_durably_non_live(&self, session_id: &str) -> bool {
        #[cfg(test)]
        if let Some(outcome) = self
            .terminal_route_test_outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .copied()
        {
            // Timeout and Store error are both fail-closed retention outcomes;
            // the distinct variants exist so the regression inventory proves
            // both paths without depending on SQLite scheduler timing.
            return match outcome {
                TerminalRouteTestOutcome::Success => true,
                TerminalRouteTestOutcome::Timeout | TerminalRouteTestOutcome::Error => false,
            };
        }
        tokio::time::timeout(
            TERMINAL_ROUTE_CONFIRM_TIMEOUT,
            self.store.media_session_route(session_id),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|route| route.as_ref().is_none_or(|route| route.state != "active"))
    }

    pub(super) fn session_lifecycle(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut lifecycles = self
            .session_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lifecycle) = lifecycles.get(session_id).and_then(Weak::upgrade) {
            return lifecycle;
        }
        let lifecycle = Arc::new(Mutex::new(()));
        lifecycles.insert(session_id.to_owned(), Arc::downgrade(&lifecycle));
        lifecycle
    }

    pub(super) fn prune_session_lifecycles(&self) {
        self.session_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, lifecycle| lifecycle.strong_count() > 0);
    }

    pub(super) fn rendition_build_gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut builds = self
            .rendition_builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(build) = builds.get(key).and_then(Weak::upgrade) {
            return build;
        }
        let build = Arc::new(Mutex::new(()));
        builds.insert(key.to_owned(), Arc::downgrade(&build));
        build
    }

    pub(super) fn prune_rendition_builds(&self) {
        self.rendition_builds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, build| build.strong_count() > 0);
    }

    /// Arm ruling A3's producer deadline once per demanded plan entry. The
    /// timestamp survives shorter HTTP block deadlines and their 503 retries.
    pub(super) fn arm_materialize_watchdog(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        index: u32,
    ) -> MaterializeDemand {
        let mut clocks = rendition.demand_since.lock().expect("demand lock");
        self.arm_materialize_watchdog_locked(rendition, index, &mut clocks)
    }

    pub(super) fn arm_materialize_watchdog_locked(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        index: u32,
        demands: &mut HashMap<u32, MaterializeClock>,
    ) -> MaterializeDemand {
        let started = Instant::now();
        if let Some(clock) = demands.get_mut(&index) {
            clock.owners += 1;
            clock.retry_pending = false;
            return MaterializeDemand {
                rendition: Arc::clone(rendition),
                index,
                started: clock.started,
            };
        }
        demands.insert(
            index,
            MaterializeClock {
                started,
                owners: 1,
                retry_pending: false,
                watchdog: None,
            },
        );
        let owner = MaterializeDemand {
            rendition: Arc::clone(rendition),
            index,
            started,
        };
        let shared = Arc::clone(self);
        let rendition = Arc::clone(rendition);
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + rendition.materialize_budget,
            ))
            .await;
            if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
                rendition.clear_demand(index);
                return;
            }
            let expired = if index == INIT_DEMAND_INDEX {
                if rendition.dir.has_init().await {
                    rendition.clear_demand(index);
                    false
                } else {
                    rendition
                        .demand_since
                        .lock()
                        .expect("demand lock")
                        .get(&index)
                        .is_some_and(|clock| clock.started == started)
                }
            } else {
                // The sink takes these locks in the same order and clears the
                // demand before releasing the manifest, closing the
                // deadline/materialization race.
                let manifest = rendition.manifest.lock().await;
                if manifest.state(index).is_some_and(SegState::is_materialized) {
                    rendition.clear_demand(index);
                    false
                } else {
                    rendition
                        .demand_since
                        .lock()
                        .expect("demand lock")
                        .get(&index)
                        .is_some_and(|clock| clock.started == started)
                }
            };
            if !expired || rendition.failure().is_some() {
                return;
            }
            if index != INIT_DEMAND_INDEX {
                // A slow, cancelled, or superseded segment request cannot
                // prove that this shared producer is unhealthy. Settle only
                // currently admitted waiters for this entry; if all callers
                // disappeared, this is a no-op. Real child failures still
                // use record_failure to answer the whole rendition.
                // Keep the episode fence through the synchronous wake. A
                // cancelled old watchdog must not wake a newly registered
                // request for this same index after it acquired a new clock.
                let clocks = rendition.demand_since.lock().expect("demand lock");
                if !clocks
                    .get(&index)
                    .is_some_and(|clock| clock.started == started)
                {
                    return;
                }
                shared.pool.fail_entry(
                    &rendition.key,
                    index,
                    &format!(
                        "materializing {} exceeded the {:.1}s producer deadline",
                        segment_name(u64::from(index)),
                        rendition.materialize_budget.as_secs_f64()
                    ),
                );
                drop(clocks);
                rendition.kick();
            } else {
                // Init waiters inspect their own captured clock after this wake.
                rendition.init_notify.notify_waiters();
            }
            // Preserve expiry long enough for a normal Retry-After cycle to
            // receive its typed failure, then forget an abandoned HTTP retry.
            // An old task never removes a newer episode for this same entry.
            tokio::time::sleep(PENDING_RETRY_AFTER.saturating_mul(2)).await;
            let mut demands = rendition.demand_since.lock().expect("demand lock");
            if demands
                .get(&index)
                .is_some_and(|clock| clock.started == started && clock.owners == 0)
            {
                demands.remove(&index);
            }
        });
        if let Some(clock) = demands
            .get_mut(&index)
            .filter(|clock| clock.started == started)
        {
            clock.watchdog = Some(watchdog.abort_handle());
        } else {
            watchdog.abort();
        }
        owner
    }

    /// Find or build the rendition for `key`, spawning its driver. `None`
    /// means the plan came out empty and the caller returns a typed refusal.
    pub(super) async fn attach_rendition(
        self: &Arc<Shared>,
        key: &str,
        identity: &SourceIdentity,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        duration_ms: i64,
        settings: &VodSettings,
    ) -> Result<Option<RenditionAttachment>, String> {
        let build_guard = self.rendition_build_gate(key).lock_owned().await;
        // Once exact-key admission succeeds, transfer the entire slow
        // Store/filesystem/head/build transaction to a detached owner before
        // the caller reaches another cancellation point. The successful
        // result returns the same guard for the final reader/session attach;
        // a cancelled caller merely drops the receiver, so the owner still
        // confirms child reap and then drops the unused attachment guard.
        let shared = Arc::clone(self);
        let key = key.to_owned();
        let identity = identity.clone();
        let settings = settings.clone();
        let result_rx = spawn_cancellation_independent(async move {
            shared
                .attach_rendition_owned(
                    key,
                    identity,
                    index,
                    recipe,
                    duration_ms,
                    settings,
                    build_guard,
                )
                .await
        });
        result_rx
            .await
            .unwrap_or_else(|_| Err("the rendition build owner exited unexpectedly".to_owned()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_rendition_owned(
        self: &Arc<Shared>,
        key: String,
        identity: SourceIdentity,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        duration_ms: i64,
        settings: VodSettings,
        build_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Option<RenditionAttachment>, String> {
        let key = key.as_str();
        // Single-flight only this key. No Store, filesystem, process, manifest,
        // or reader await below owns the node-wide registry mutex.
        let existing = {
            let renditions = self.renditions.lock().await;
            match renditions.get(key).map(Arc::clone) {
                Some(existing)
                    if existing.failure().is_none() && !existing.closed.load(Relaxed) =>
                {
                    return Ok(Some(RenditionAttachment {
                        rendition: existing,
                        _build_guard: build_guard,
                    }));
                }
                Some(existing) => {
                    // A failed (or closed) handle cannot answer new readers.
                    // Publish closed first, but leave the exact handle in the
                    // map until its accounting facts have been collected. If
                    // this request is cancelled during that await, the next
                    // key owner can resume the same removal transaction.
                    existing.closed.store(true, Relaxed);
                    existing.gen_epoch.fetch_add(1, Relaxed);
                    existing.kick();
                    self.pool.close(key);
                    Some(existing)
                }
                None => None,
            }
        };
        if let Some(stale) = existing {
            let (claimed, admitted) = {
                let manifest = stale.manifest.lock().await;
                (manifest.materialized_bytes(), manifest.is_admitted())
            };
            let removed = {
                let mut renditions = self.renditions.lock().await;
                let exact_stale = renditions
                    .get(key)
                    .is_some_and(|current| Arc::ptr_eq(current, &stale));
                if exact_stale {
                    renditions.remove(key)
                } else {
                    None
                }
            };
            if removed.is_some() {
                if admitted {
                    sub_saturating(&self.completed_cache, claimed);
                } else {
                    sub_saturating(&self.working_set, claimed);
                }
                tracing::info!(
                    target: "plurxd::vodserve",
                    rendition = %key, "replacing a failed rendition on create"
                );
            }
        }

        let plan = self
            .stored_plan(key, &identity, &index, &recipe, duration_ms)
            .await?;
        if plan.is_empty() {
            return Ok(None);
        }
        let rendition = self
            .build_rendition(key, index, recipe, plan, &settings)
            .await?;
        let adopted_bytes = rendition.manifest.lock().await.materialized_bytes();
        let (rendition, installed) = {
            let mut renditions = self.renditions.lock().await;
            if let Some(winner) = renditions
                .get(key)
                .filter(|winner| winner.failure().is_none() && !winner.closed.load(Relaxed))
                .map(Arc::clone)
            {
                (winner, false)
            } else {
                renditions.insert(key.to_string(), Arc::clone(&rendition));
                (rendition, true)
            }
        };
        if installed {
            if adopted_bytes > 0 {
                self.working_set.fetch_add(adopted_bytes, Relaxed);
            }
            let _driver = spawn_driver(Arc::clone(self), Arc::clone(&rendition));
        }
        #[cfg(test)]
        if installed {
            let pause = self
                .rendition_install_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        // From this point cancellation leaves a registered, correctly
        // accounted dormant rendition. Admission may be retried by its driver
        // or maintenance; it can no longer leak counters or an untracked
        // directory if resurrection's request deadline wins.
        {
            let mut manifest = rendition.manifest.lock().await;
            if !manifest.is_empty() && manifest.next_gap(0).is_none() {
                self.try_admit(&rendition, &mut manifest).await;
            }
        }
        Ok(Some(RenditionAttachment {
            rendition,
            _build_guard: build_guard,
        }))
    }

    /// Plan acquisition (put-if-absent): the first plan written under a key
    /// is THE plan — on a lost race the stored one wins and is what every
    /// node serves.
    async fn stored_plan(
        &self,
        key: &str,
        identity: &SourceIdentity,
        index: &Option<FragmentIndex>,
        recipe: &Recipe,
        duration_ms: i64,
    ) -> Result<SegmentPlan, String> {
        if let Some(plan) = self
            .store
            .rendition_plan(key, identity)
            .await
            .map_err(|error| format!("reading the rendition plan: {error}"))?
        {
            return Ok(plan);
        }
        let plan = if let Some(encoding) = &recipe.encoding {
            encoding.grid.plan(
                duration_ms,
                (encoding.options.video_bitrate_kbps + encoding.options.audio_bitrate_kbps)
                    .saturating_mul(1000)
                    .into(),
            )
        } else {
            let index = index.as_ref().expect("copy recipe has a fragment index");
            let policy = shipped_policy(index.timescale);
            let tracks = track_durations(index, recipe, duration_ms);
            plurx_core::segplan::plan_copy(index, &policy, &tracks)
        };
        if plan.is_empty() {
            // Never stored: an empty plan under the key would poison it.
            return Ok(plan);
        }
        let stored = self
            .store
            .put_rendition_plan(key, recipe.file.id, &plan, identity)
            .await
            .map_err(|error| format!("storing the rendition plan: {error}"))?;
        if stored {
            return Ok(plan);
        }
        // Lost the put-if-absent race: re-get and serve the stored plan.
        match self
            .store
            .rendition_plan(key, identity)
            .await
            .map_err(|error| format!("re-reading the rendition plan: {error}"))?
        {
            Some(theirs) => Ok(theirs),
            None => {
                tracing::warn!(
                    target: "plurxd::vodserve",
                    rendition = %key,
                    "lost the plan race but the stored plan is gone; serving ours"
                );
                Ok(plan)
            }
        }
    }

    pub(super) async fn build_rendition(
        self: &Arc<Shared>,
        key: &str,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        plan: SegmentPlan,
        settings: &VodSettings,
    ) -> Result<Arc<Rendition>, String> {
        let source = crate::fragment_index_cluster::open_source_fence(
            &recipe.file,
            recipe.source_object_version.as_deref(),
        )
        .await?;
        let dir = RenditionDir::new(self.base.join(key));
        let mut existed = tokio::fs::metadata(dir.path()).await.is_ok();
        let encoded_process = recipe
            .encoding
            .as_ref()
            .map(|encoding| encoding.engine.process_identity().to_owned());
        if existed {
            if let Some(process) = encoded_process.as_deref() {
                match tokio::fs::read_to_string(dir.path().join(ENCODED_PROCESS_NAME)).await {
                    Ok(owner) if owner.trim() == process => {}
                    Ok(_) => {
                        tokio::fs::remove_dir_all(dir.path())
                            .await
                            .map_err(|error| {
                                format!("removing an obsolete encoded rendition directory: {error}")
                            })?;
                        existed = false;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // An encoded directory without its generation marker
                        // is unverifiable. Never adopt it under a new marker.
                        tokio::fs::remove_dir_all(dir.path())
                            .await
                            .map_err(|error| {
                                format!("removing an unmarked encoded rendition directory: {error}")
                            })?;
                        existed = false;
                    }
                    Err(error) => {
                        return Err(format!(
                            "reading the encoded rendition process marker: {error}"
                        ));
                    }
                }
            }
        }
        dir.create()
            .await
            .map_err(|error| format!("creating the rendition directory: {error}"))?;
        if !existed {
            if let Some(process) = encoded_process.as_deref() {
                if let Err(error) = publish_encoded_process_marker(dir.path(), process).await {
                    let _ = tokio::fs::remove_dir_all(dir.path()).await;
                    return Err(format!(
                        "publishing the encoded rendition process marker: {error}"
                    ));
                }
            }
        }

        let mut manifest = Manifest::new(plan.clone());
        let mut identity_state = IdentityState::default();
        if existed {
            // Resurrection/adoption: make the manifest agree with the disk.
            let report = dir
                .reconcile(&mut manifest, now_ms())
                .await
                .map_err(|error| format!("reconciling the rendition directory: {error}"))?;
            if !report.adopted.is_empty() || !report.forgotten.is_empty() {
                tracing::info!(
                    target: "plurxd::vodserve",
                    rendition = %key,
                    adopted = report.adopted.len(),
                    forgotten = report.forgotten.len(),
                    "reconciled an adopted rendition directory"
                );
            }
            match load_identity(&dir.path().join(IDENTITY_NAME)).await {
                Some(identity) => {
                    if !report.init_present && manifest.materialized_count() > 0 {
                        // Handoff §5's missing-init arm, and it cannot wait
                        // for the next ordinary generation: a fully adopted
                        // manifest has no gap, so the scheduler answers Idle
                        // forever, no generation ever spawns to re-write the
                        // init, and every init GET pends to deadline for the
                        // session's life. Run a HEAD regeneration now — spawn
                        // the generation child, read only to its muxer init,
                        // verify against the stored digests.
                        match regenerate_init_head(
                            &recipe,
                            &source,
                            &identity,
                            &self.head_regeneration_slots,
                            &self.runtime_cache,
                        )
                        .await
                        {
                            Ok(served) => {
                                // Match keeps every surviving segment.
                                dir.write_init(&served.bytes).await.map_err(|error| {
                                    format!("re-writing the regenerated init: {error}")
                                })?;
                                tracing::info!(
                                    target: "plurxd::vodserve",
                                    rendition = %key,
                                    "head regeneration re-derived a missing init.mp4; \
                                     every adopted segment kept"
                                );
                                identity_state = IdentityState {
                                    identity: Some(identity),
                                    from_disk: true,
                                };
                            }
                            Err(HeadRegenerationError::Busy) => {
                                return Err(crate::transcode::vod_refusal_error(
                                    "vod_head_regeneration_busy",
                                    "missing-init recovery is at its node-wide process limit",
                                ));
                            }
                            Err(HeadRegenerationError::Oversize) => {
                                return Err(crate::transcode::vod_refusal_error(
                                    "vod_head_regeneration_oversize",
                                    "the regenerated muxer head exceeded its strict byte limit",
                                ));
                            }
                            Err(HeadRegenerationError::Failed(why)) => {
                                // Mismatch (or an unverifiable head): purge to
                                // planned-only and establish fresh — before
                                // the counters below, so nothing is adopted.
                                let freed = dir.purge(&mut manifest).await;
                                if let Some(error) = freed.error {
                                    tracing::warn!(
                                        target: "plurxd::vodserve",
                                        rendition = %key,
                                        "purging after a failed head regeneration: {error}"
                                    );
                                }
                                let _ =
                                    tokio::fs::remove_file(dir.path().join(IDENTITY_NAME)).await;
                                tracing::info!(
                                    target: "plurxd::vodserve",
                                    rendition = %key,
                                    "head regeneration could not verify the adopted \
                                     rendition; purged to planned-only: {why}"
                                );
                            }
                        }
                    } else {
                        // Init present (or nothing adopted): the stored
                        // digests let the FIRST ordinary generation verify
                        // (vodgen refuses InitDrift before any write) and
                        // re-write the init if it is the missing piece.
                        identity_state = IdentityState {
                            identity: Some(identity),
                            from_disk: true,
                        };
                    }
                }
                None => {
                    // No identity means nothing on disk is verifiable: purge
                    // to planned-only and establish fresh.
                    if manifest.materialized_count() > 0 || dir.has_init().await {
                        let freed = dir.purge(&mut manifest).await;
                        if let Some(error) = freed.error {
                            tracing::warn!(
                                target: "plurxd::vodserve",
                                rendition = %key,
                                "purging an unverifiable rendition: {error}"
                            );
                        }
                        tracing::info!(
                            target: "plurxd::vodserve",
                            rendition = %key,
                            "purged an adopted rendition with no stored identity"
                        );
                    }
                }
            }
        }

        let timescale = plan.timescale.max(1);
        let seconds_per_segment = if plan.is_empty() {
            1.0
        } else {
            plan.duration_ticks() as f64 / f64::from(timescale) / plan.len() as f64
        };
        let plan_len = plan.len();
        let rendition = Arc::new(Rendition {
            key: key.to_string(),
            dir,
            recipe,
            source: Some(source),
            playlist: plan.playlist().into_bytes(),
            plan,
            timescale,
            seconds_per_segment,
            index,
            policy: shipped_policy(timescale),
            working_set_budget: settings.working_set_bytes,
            completed_cache_budget: settings.completed_cache_bytes,
            materialize_budget: settings.materialize_budget,
            manifest: Mutex::new(manifest),
            identity: Mutex::new(identity_state),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            #[cfg(test)]
            stopped_poll_armed: Notify::new(),
            #[cfg(test)]
            stopped_poll_fired: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            // Creation can be cancelled after the rendition is installed but
            // before a session reader attaches. Start it as dormant so that
            // abandoned resurrection preparation remains reclaimable; the
            // atomic reader/session commit clears this on success.
            dormant_since: StdMutex::new(Some(Instant::now())),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            permit_wait_logged: AtomicBool::new(false),
            handoff: StdMutex::new(None),
            handoff_expiry_armed: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        });
        Ok(rendition)
    }

    /// Completion → admission (plan §2.4): reserve, make durable, complete —
    /// in that order, because admission is the durability boundary and
    /// reversing it publishes a promise before the bytes behind it are real.
    pub(super) async fn try_admit(
        self: &Arc<Shared>,
        rendition: &Rendition,
        manifest: &mut Manifest,
    ) {
        if manifest.is_admitted() {
            return;
        }
        let budgets = Budgets {
            working_set_bytes: rendition.working_set_budget,
            admission_sizing_bytes: rendition.completed_cache_budget,
            admission_share: 0.5,
        };
        if let Err(refused) = manifest.reserve(&budgets) {
            if !rendition.warned_admission.swap(true, Relaxed) {
                tracing::info!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key,
                    "rendition stays working-set-only: {refused:?}"
                );
            }
            return;
        }
        // The identity file is part of the promise; sync it with the rest.
        if let Err(error) = sync_file(&rendition.identity_path()).await {
            tracing::warn!(
                target: "plurxd::vodserve",
                rendition = %rendition.key,
                "could not sync identity.json before admission: {error}"
            );
            return;
        }
        if let Err(error) = rendition.dir.make_durable(manifest).await {
            tracing::warn!(
                target: "plurxd::vodserve",
                rendition = %rendition.key,
                "make_durable refused, staying un-admitted: {error}"
            );
            return;
        }
        match manifest.complete(&budgets) {
            Ok(bytes) => {
                sub_saturating(&self.working_set, bytes);
                self.completed_cache.fetch_add(bytes, Relaxed);
                // The working set just shrank node-wide: producers on OTHER
                // renditions terminated for NoRoom are waiting on exactly
                // this, and the next maintain tick is a viewer's deadline
                // away.
                self.kick_all();
                tracing::info!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key,
                    bytes,
                    "rendition completed and admitted to the cache"
                );
            }
            Err(refused) => {
                if !rendition.warned_admission.swap(true, Relaxed) {
                    tracing::info!(
                        target: "plurxd::vodserve",
                        rendition = %rendition.key,
                        "completion refused, staying un-admitted: {refused:?}"
                    );
                }
            }
        }
    }

    /// Give a dormant, un-admitted rendition up whole — but only after
    /// serializing against creation for this exact key and re-verifying that
    /// it is still dormant.
    ///
    /// The re-check is the point: maintain's collect-then-purge scan races a
    /// create, and a session attached between the scan and the commit would
    /// be pinned to a closed rendition — its driver exited, its wait keys
    /// answering nothing, every GET pending forever. `attach_rendition` keeps
    /// the same per-key gate through session attach, while maintenance skips
    /// a key whose build/attach is active; different keys never wait for it.
    pub(super) async fn purge_if_dormant(self: &Arc<Shared>, key: &str, ttl: Duration) {
        let Ok(build_guard) = self.rendition_build_gate(key).try_lock_owned() else {
            return;
        };
        let rendition = {
            let renditions = self.renditions.lock().await;
            renditions.get(key).map(Arc::clone)
        };
        let Some(rendition) = rendition else {
            return;
        };
        if !rendition.readers.lock().await.is_empty() {
            return;
        }
        let dormant = rendition
            .dormant_since
            .lock()
            .expect("dormant lock")
            .is_some_and(|since| since.elapsed() > ttl);
        if !dormant {
            return;
        }
        // Keep the manifest fence through the short exact map removal. That
        // makes admission and purge mutually exclusive without ever holding
        // the node-wide registry while awaiting a rendition-local lock.
        let manifest = rendition.manifest.lock().await;
        if manifest.is_admitted() {
            return;
        }
        let removed = {
            let mut renditions = self.renditions.lock().await;
            let exact = renditions
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &rendition));
            if exact {
                rendition.closed.store(true, Relaxed);
                rendition.gen_epoch.fetch_add(1, Relaxed);
                renditions.remove(key);
            }
            exact
        };
        drop(manifest);
        if !removed {
            return;
        }
        // The removal commit has no following request-owned await. Transfer
        // exact key authority, child termination, accounting, directory and
        // identity cleanup to one detached settlement owner first. Awaiting
        // its handle is only a convenience for maintenance/tests; cancellation
        // drops the handle, not the transaction or its build gate.
        let shared = Arc::clone(self);
        let settlement = tokio::spawn(async move {
            let _build_guard = build_guard;
            #[cfg(test)]
            let pause = shared
                .dormant_purge_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
            let _ = perform_driver_step(
                &shared,
                &rendition,
                Step::Terminate {
                    why: Termination::Idle,
                },
            )
            .await;
            shared.pool.close(&rendition.key);
            {
                let mut manifest = rendition.manifest.lock().await;
                let freed = rendition.dir.purge(&mut manifest).await;
                sub_saturating(&shared.working_set, freed.bytes);
                // Whatever a failing unlink left both on disk and claimed is
                // no longer managed; subtract its final claim exactly once
                // while same-key rebuild is still excluded by the gate.
                sub_saturating(&shared.working_set, manifest.materialized_bytes());
                if let Some(error) = freed.error {
                    tracing::warn!(
                        target: "plurxd::vodserve",
                        rendition = %rendition.key,
                        "purging a dormant rendition: {error}"
                    );
                }
            }
            let _ = tokio::fs::remove_file(rendition.identity_path()).await;
            rendition.kick();
            // Freed bytes are node-wide news (see `try_admit`).
            shared.kick_all();
            tracing::info!(
                target: "plurxd::vodserve",
                rendition = %rendition.key,
                "purged a dormant un-admitted rendition"
            );
        });
        let _ = settlement.await;
    }

    /// Kick EVERY rendition's driver — for events that change the node-wide
    /// working set. A producer terminated for `NoRoom` on another rendition
    /// is waiting on exactly this; without it, the hold stands until the next
    /// maintain tick while a blocked viewer's deadline burns.
    ///
    /// Spawned rather than inline so callers already holding the renditions
    /// lock (or a manifest lock ordered after it) cannot deadlock.
    pub(super) fn kick_all(self: &Arc<Shared>) {
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            let renditions: Vec<Arc<Rendition>> = shared
                .renditions
                .lock()
                .await
                .values()
                .map(Arc::clone)
                .collect();
            for rendition in renditions {
                rendition.kick();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// the producer driver
// ---------------------------------------------------------------------------
