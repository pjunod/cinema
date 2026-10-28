use super::*;

#[cfg(target_os = "linux")]
pub(super) struct FiniteClosingOwner {
    /// Kept until the real producer driver has stopped reporting output.
    rich: Mutex<Option<Arc<Rendition>>>,
    output: Mutex<Option<plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput>>,
    producer: Mutex<Option<Arc<super::finite_software::FiniteSoftwareProducer>>>,
    ticket: Mutex<Option<plurx_core::adaptive_resources::runtime::finite_vod::FiniteCloseTicket>>,
    cache_grant: std::sync::Mutex<Option<crate::playback_cache::FiniteCacheReservation>>,
    claimed_bytes: AtomicU64,
    claimed_admitted: AtomicBool,
    known_closed: AtomicBool,
    running: AtomicBool,
    // False while a build/publication arm still owns the dormant output.
    cleanup_ready: AtomicBool,
}

/// The map owns the original producer/output before the first post-R await.
/// The directory owns this non-clone arm until exact Shared publication. The
/// weak Shared link avoids a cycle through a published rendition; the map's
/// producer/output have no PendingWrites, arm or strong Shared backreference.
#[cfg(target_os = "linux")]
pub(crate) struct FiniteBuildCleanup {
    shared: std::sync::Weak<Shared>,
    key: String,
    closing: Arc<FiniteClosingOwner>,
    armed: bool,
}

#[cfg(target_os = "linux")]
impl FiniteBuildCleanup {
    pub(crate) fn selector(&self) -> PreparedAttemptCleanup {
        PreparedAttemptCleanup::from_closing(Arc::clone(&self.closing))
    }

    fn validate_publication(
        &self,
        shared: &Arc<Shared>,
        key: &str,
        closings: &std::collections::BTreeMap<String, Arc<FiniteClosingOwner>>,
    ) -> Result<(), String> {
        if self.key != key
            || !std::sync::Weak::ptr_eq(&self.shared, &Arc::downgrade(shared))
            || self.closing.cleanup_ready.load(Acquire)
            || !closings
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.closing))
        {
            return Err("original finite build cleanup no longer matches publication".into());
        }
        Ok(())
    }

    /// Only after the exact rendition was inserted, under both map locks.
    /// No await separates that insertion, disarming and exact slot removal.
    fn published(
        mut self,
        closings: &mut std::collections::BTreeMap<String, Arc<FiniteClosingOwner>>,
    ) {
        self.armed = false;
        closings.remove(&self.key);
        if closings.is_empty() {
            closings.clear();
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for FiniteBuildCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.closing.cleanup_ready.store(true, Release);
        if let (Ok(_runtime), Some(shared)) =
            (tokio::runtime::Handle::try_current(), self.shared.upgrade())
        {
            let _original_cleanup = shared
                .spawn_finite_closing(std::mem::take(&mut self.key), Arc::clone(&self.closing));
        }
        // The original selectors were already installed in the map. Executor
        // loss or a failed cleanup keeps that same map owner and full charge.
    }
}

#[cfg(target_os = "linux")]
pub(super) struct FiniteBuildSlot<'a> {
    shared: &'a Arc<Shared>,
    key: String,
    closings:
        tokio::sync::MutexGuard<'a, std::collections::BTreeMap<String, Arc<FiniteClosingOwner>>>,
}

#[cfg(target_os = "linux")]
impl FiniteBuildSlot<'_> {
    pub(super) fn install(
        mut self,
        owner: plurx_core::adaptive_resources::runtime::finite_vod::SoftwareVodOwner,
        cache_grant: crate::playback_cache::FiniteCacheReservation,
    ) -> RenditionDir {
        let mut dir = RenditionDir::from_finite_producer(owner, cache_grant);
        let output = dir.finite_output().expect("fresh finite output");
        let producer = dir
            .finite_producer()
            .expect("fresh original finite producer");
        let closing = Arc::new(FiniteClosingOwner {
            rich: Mutex::new(None),
            output: Mutex::new(Some(output)),
            producer: Mutex::new(Some(producer)),
            ticket: Mutex::new(None),
            cache_grant: std::sync::Mutex::new(None),
            claimed_bytes: AtomicU64::new(0),
            claimed_admitted: AtomicBool::new(false),
            known_closed: AtomicBool::new(false),
            running: AtomicBool::new(false),
            cleanup_ready: AtomicBool::new(false),
        });
        assert!(
            self.closings
                .insert(self.key.clone(), Arc::clone(&closing))
                .is_none(),
            "finite build slot is exclusive before accepted R"
        );
        dir.install_finite_build_cleanup(FiniteBuildCleanup {
            shared: Arc::downgrade(self.shared),
            key: self.key,
            closing,
            armed: true,
        });
        dir
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct PreparedAttemptRendition {
    shared: Arc<Shared>,
    key: String,
    rendition: Option<Arc<Rendition>>,
    build_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    // Last: the original core charge stays live until every rich field above
    // and the detached abandoned-owner transfer have been destroyed.
    application_lease:
        Option<plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease>,
    installed: bool,
}

/// The exact existing finite-closing owner, retained independently of the
/// rich attempt entry. Store may record a losing attempt as closed only after
/// this original owner reports a known native and journal close.
#[cfg(target_os = "linux")]
pub(crate) struct PreparedAttemptCleanup {
    closing: Arc<FiniteClosingOwner>,
}

#[cfg(target_os = "linux")]
pub(crate) struct AdaptiveBuildFailure {
    pub(crate) message: String,
    /// Present only when the original finite R was admitted and its exact
    /// closing owner was registered before this failure was returned.
    pub(crate) cleanup: Option<PreparedAttemptCleanup>,
}

#[cfg(target_os = "linux")]
impl AdaptiveBuildFailure {
    pub(crate) fn before_r(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cleanup: None,
        }
    }
}

/// Owns the original cleanup registration task across a cancelled waiter.
/// The M4 registry keeps this handle until it has joined the same task and
/// recovered the exact finite-closing selector.
#[cfg(target_os = "linux")]
pub(crate) struct PreparedAttemptCleanupTask {
    task: Option<tokio::task::JoinHandle<Arc<FiniteClosingOwner>>>,
}

#[cfg(target_os = "linux")]
async fn run_prepared_attempt_cleanup(
    shared: Arc<Shared>,
    key: String,
    gate: Option<tokio::sync::OwnedMutexGuard<()>>,
    rendition: Arc<Rendition>,
    output: plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput,
    producer: Arc<super::finite_software::FiniteSoftwareProducer>,
    lease: Option<plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease>,
) -> Arc<FiniteClosingOwner> {
    let closing = shared
        .retain_unattached_finite_with_handle(&key, Some(Arc::clone(&rendition)), output, producer)
        .await;
    drop(gate);
    drop(rendition);
    drop(lease);
    closing
}

/// Exact application-owned original registration future layout, obtained
/// from its function item without constructing a dummy native owner.
#[cfg(target_os = "linux")]
pub(crate) fn prepared_attempt_cleanup_frame_bytes() -> usize {
    fn frame_size<A, B, C, D, E, F, G, Fut: std::future::Future>(
        _: impl Fn(A, B, C, D, E, F, G) -> Fut,
    ) -> usize {
        std::mem::size_of::<Fut>()
    }
    frame_size(run_prepared_attempt_cleanup)
}

#[cfg(target_os = "linux")]
impl PreparedAttemptCleanupTask {
    pub(crate) fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    pub(crate) async fn join(&mut self) -> Result<PreparedAttemptCleanup, String> {
        let task = self
            .task
            .as_mut()
            .ok_or("adaptive cleanup task was already joined")?;
        let closing = task
            .await
            .map_err(|error| format!("registering adaptive finite cleanup: {error}"))?;
        self.task.take();
        Ok(PreparedAttemptCleanup { closing })
    }
}

#[cfg(target_os = "linux")]
impl PreparedAttemptCleanup {
    pub(super) fn from_closing(closing: Arc<FiniteClosingOwner>) -> Self {
        Self { closing }
    }

    pub(crate) fn known_closed(&self) -> bool {
        self.closing.known_closed.load(Acquire)
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) async fn original_output_for_test(
        &self,
    ) -> plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput {
        self.closing
            .output
            .lock()
            .await
            .as_ref()
            .expect("dormant original cleanup still retains its output")
            .clone()
    }
}

#[cfg(target_os = "linux")]
impl PreparedAttemptRendition {
    /// A bounded preview from the SAME dormant plan that Adopt later attaches.
    /// It creates no session, route, File or process and carries no physical
    /// proof. Its exact encoded bytes become Store activation's negative check.
    pub(crate) fn planned_preview(
        &self,
        session_id: &str,
        serving_generation: u64,
    ) -> Result<crate::media_sessions::RemoteStartResponse, String> {
        let rendition = self
            .rendition
            .as_ref()
            .ok_or("adaptive dormant rendition was already transferred")?;
        let encoding = rendition
            .recipe
            .encoding
            .as_ref()
            .ok_or("adaptive finite rendition has no frozen encoding")?;
        let audio_bytes = rendition
            .recipe
            .audio_delivery
            .as_ref()
            .map_or(Some(0), |audio| audio.requested_heap_bytes())
            .ok_or("adaptive audio preview capacity overflow")?;
        if audio_bytes > crate::media_sessions::ADAPTIVE_PREVIEW_BYTES / 2 {
            return Err("adaptive audio preview exceeds its precharged partition".into());
        }
        let height = encoding.options.target_height;
        let preview = crate::media_sessions::RemoteStartResponse {
            processed_dv_profile: None,
            processed_fel: None,
            retained_output: None,
            audio_delivery: rendition.recipe.audio_delivery.clone(),
            session_id: session_id.to_owned(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(self.planned_duration_ms()),
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            target_height: height,
            kind: crate::transcode::SessionKind::Transcode { height },
            encoder: encoding.delivered_encoder_label().to_owned(),
            grade: encoding.options.pipeline.output_grade(),
            vod: true,
            control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            activation_generation: Some(serving_generation),
        };
        if !preview.is_valid()
            || preview
                .adaptive_preview_requested_heap_bytes()
                .is_none_or(|bytes| bytes > crate::media_sessions::ADAPTIVE_PREVIEW_BYTES)
        {
            return Err("adaptive dormant preview exceeds its measured partition".into());
        }
        Ok(preview)
    }

    /// Attach only this original prepared rendition after the Store's exact
    /// Adopt is known. The request contributes viewer intent; its executable
    /// recipe and source file come from the retained charged Rendition.
    #[expect(
        clippy::too_many_arguments,
        reason = "The original adopted claim, exact route envelope and serving bindings remain borrowed in one precharged handoff frame without an extra request allocation."
    )]
    pub(crate) async fn attach_after_store_adopt(
        &mut self,
        vod: &VodServe,
        envelope: &crate::media_sessions::RemoteStartRequest,
        settings: &VodSettings,
        user_name: &str,
        session_id: String,
        authority: crate::serving_fence::ServingAuthority,
        serving_generation: u64,
        deadline: Instant,
    ) -> Result<VodStart, String> {
        if !envelope.is_valid()
            || envelope.request.continuous_media.is_some()
            || envelope.retained_output.is_some()
        {
            return Err("adaptive selected route is outside the finite AVC profile".into());
        }
        self.install_after_adoption().await?;
        let rendition = self.rendition();
        let file = &rendition.recipe.file;
        if envelope.request.file_id != file.id {
            return Err("adaptive adopted file differs from the original rendition".into());
        }
        let encoding = rendition
            .recipe
            .encoding
            .as_ref()
            .ok_or("adaptive rendition lost its planned encoding")?;
        let request = VodRecipeRequest {
            root_admission: None,
            measured_candidate: None,
            retained_capture: RetainedOutputCapture::New,
            request: &envelope.request,
            encoding: Some(Arc::clone(encoding)),
            soundtrack: None,
            companion: None,
        };
        let item_title = format!("#{}", file.item_id);
        vod.try_create_adopted_attempt(
            request,
            file,
            settings,
            VodAttribution {
                user_name,
                item_title: &item_title,
                supersession_user: user_name,
            },
            session_id,
            VodServingAdmission::new(authority, serving_generation, deadline),
            crate::state::PlaybackViewerDemand {
                principal: envelope.principal.clone(),
                playback_id: envelope.request.playback_id.clone(),
            },
            self,
        )
        .await
    }

    pub(crate) fn planned_duration_ms(&self) -> i64 {
        plan_duration_ms(&self.rendition().plan)
    }

    pub(super) fn rendition(&self) -> &Arc<Rendition> {
        self.rendition
            .as_ref()
            .expect("prepared attempt owns its rendition")
    }

    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        self.key.capacity().checked_add(std::mem::size_of::<Self>())
    }

    /// The registered M4 task takes this one-shot fence before it keeps the
    /// pending owner across peer replies. Its final Drop must follow the
    /// actual charged registry entry and coordinator future, whether the
    /// attempt is adopted or moved to the compact closing selector.
    pub(crate) fn take_application_lease(
        &mut self,
    ) -> Result<plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease, String>
    {
        self.application_lease
            .take()
            .ok_or_else(|| "adaptive application lease was already transferred".into())
    }

    pub(crate) async fn prepare_worker_binding(
        &self,
        deadline: Instant,
    ) -> Result<(String, String), String> {
        let producer = self
            .rendition()
            .dir
            .finite_producer()
            .expect("adaptive attempt has its original producer");
        let prepared = producer.prepare_adaptive_inputs(deadline).await?;
        let output = self
            .rendition()
            .dir
            .finite_output()
            .expect("adaptive attempt has its output selector");
        let output = output
            .output_digest()
            .map_err(|error| format!("binding finite attempt output: {error}"))?;
        Ok((hex::encode(prepared), hex::encode(output)))
    }

    /// The committed Store Adopt is the only caller that may publish this
    /// already charged rendition and start its original driver.
    pub(crate) async fn install_after_adoption(&mut self) -> Result<(), String> {
        if self.installed {
            return Ok(());
        }
        let rendition = Arc::clone(self.rendition());
        {
            let mut closings = self.shared.finite_closing.lock().await;
            let mut renditions = self.shared.renditions.lock().await;
            if renditions.contains_key(&self.key) {
                return Err("adaptive rendition key already has an owner".into());
            }
            let cleanup = self
                .shared
                .take_finite_build_for_publication(&self.key, &rendition, &closings)?
                .ok_or("adaptive finite publication lost its original build arm")?;
            renditions.insert(self.key.clone(), Arc::clone(&rendition));
            cleanup.published(&mut closings);
        }
        self.installed = true;
        let _driver = spawn_driver(Arc::clone(&self.shared), rendition);
        self.build_guard.take();
        Ok(())
    }

    fn launch_abandoned_cleanup(
        &mut self,
    ) -> Option<tokio::task::JoinHandle<Arc<FiniteClosingOwner>>> {
        if self.installed {
            return None;
        }
        let rendition = self.rendition.take()?;
        let output = rendition
            .dir
            .finite_output()
            .expect("adaptive attempt built one finite output");
        let producer = rendition
            .dir
            .finite_producer()
            .expect("adaptive attempt built a fresh finite producer");
        let shared = Arc::clone(&self.shared);
        let key = std::mem::take(&mut self.key);
        let gate = self.build_guard.take();
        let lease = self.application_lease.take();
        // The original charged owner is moved into a detached existing
        // finite-closing registration. Dropping the waiter cannot discard it.
        Some(tokio::spawn(run_prepared_attempt_cleanup(
            shared, key, gate, rendition, output, producer, lease,
        )))
    }

    pub(crate) fn begin_abandon(mut self) -> Result<PreparedAttemptCleanupTask, String> {
        let task = self
            .launch_abandoned_cleanup()
            .ok_or("adaptive attempt no longer owns a dormant rendition")?;
        Ok(PreparedAttemptCleanupTask { task: Some(task) })
    }
}

#[cfg(target_os = "linux")]
impl Drop for PreparedAttemptRendition {
    fn drop(&mut self) {
        if self.installed || self.rendition.is_none() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            let _ = self.launch_abandoned_cleanup();
        }
        // Without an executor, the core runtime retains the full unresolved
        // obligation. Dropping Rust selectors grants no native-close proof.
    }
}

#[cfg(target_os = "linux")]
struct FiniteClosingRun(Arc<FiniteClosingOwner>);

#[cfg(target_os = "linux")]
impl Drop for FiniteClosingRun {
    fn drop(&mut self) {
        self.0.running.store(false, Release);
    }
}

/// How many refused titles the local index pass is asked to put first.
/// More than one pass's worth on purpose, and small, because a viewer whose
/// start was refused asks again on the next start.
const LOCAL_INDEX_DEMAND_CAP: usize = 64;

impl Shared {
    /// Why a start has no local fragment index for `identity`, as the code
    /// and words of its VOD refusal.
    ///
    /// The recorded outcome decides, so the refusal says what the indexer
    /// found rather than "pending" for a title it will never index:
    /// terminal is `vod_index_unavailable` with the stored reason; a wait
    /// says when the next attempt is due; due, or never tried, asks the
    /// local pass to examine the file next (`demand_local_index`).
    pub(super) async fn local_index_refusal(
        &self,
        file_id: i64,
        identity: &plurx_core::segplan::SourceIdentity,
        untried: String,
    ) -> (&'static str, String) {
        use plurx_core::segplan::IndexOutcomeState;
        let now = crate::state::clock_ms();
        let outcome = match self.store.fragment_index_outcome(file_id, identity).await {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::debug!(file_id, %error, "reading the local index outcome");
                None
            }
        };
        let Some(outcome) = outcome else {
            self.demand_local_index(file_id);
            return ("vod_index_pending", untried);
        };
        let found = crate::http::index_refusal_summary(std::slice::from_ref(&outcome), now)
            .map_or_else(|| outcome.reason.clone(), |(_, detail)| detail);
        // The indexer's own words (`found`) can carry source paths and
        // ffmpeg's stderr. A pending refusal's message reaches the client, so
        // it names the failure code; the terminal one is replaced with a
        // generic message at the HTTP edge and logged there in full.
        let failure = outcome.typed_code.map_or_else(
            || outcome.refusal.code().to_owned(),
            |code| code.as_str().to_owned(),
        );
        match outcome.state(now) {
            IndexOutcomeState::Terminal => (
                "vod_index_unavailable",
                format!("this node cannot index this title: {found}"),
            ),
            IndexOutcomeState::WaitUntil(at) => {
                self.demand_local_index(file_id);
                (
                    "vod_index_pending",
                    format!(
                        "the last index attempt failed ({failure}); the next is due in {} s",
                        (at.saturating_sub(now).max(0) + 999) / 1_000
                    ),
                )
            }
            IndexOutcomeState::Due => {
                self.demand_local_index(file_id);
                (
                    "vod_index_pending",
                    format!(
                        "{untried}; the last index attempt failed ({failure}) and is due again"
                    ),
                )
            }
        }
    }

    /// A start for `file_id` was refused for want of a local fragment index:
    /// ask the node-local pass to examine it before its backlog.
    pub(super) fn demand_local_index(&self, file_id: i64) {
        let mut demand = self
            .local_index_demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        demand.retain(|demanded| *demanded != file_id);
        demand.push_front(file_id);
        demand.truncate(LOCAL_INDEX_DEMAND_CAP);
    }

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
                Ok(discovered) => {
                    // A recovered/live finite owner can protect a child before
                    // the per-key maintenance pass reaches it. Do not enqueue
                    // those paths for legacy recursive deletion.
                    pending.extend(discovered.into_iter().filter(|candidate| {
                        !self
                            .resource_runtime
                            .as_ref()
                            .is_some_and(|runtime| runtime.protects_output_path(&candidate.path))
                    }));
                }
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
            let Ok(_dependency_guard) = self.rendition_build_gate(&candidate.key).try_lock_owned()
            else {
                if let Some(candidate) = pending.pop_front() {
                    pending.push_back(candidate);
                }
                continue;
            };
            // Protection is a negative guard, not authority to clean or adopt
            // the rendition. Recheck after taking the exact-key gate because
            // a protected owner may have appeared since discovery.
            if self
                .resource_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.protects_output_path(&candidate.path))
            {
                if let Some(candidate) = pending.pop_front() {
                    pending.push_back(candidate);
                }
                continue;
            }
            match super::preparation_storage::has_private_marker(&candidate.path).await {
                Ok(false) => {}
                Ok(true) => {
                    pending.pop_front();
                    continue;
                }
                Err(error) => {
                    tracing::warn!(target: "plurxd::vodserve",  %error, "encoded cache ownership unreadable; cleanup deferred");
                    break;
                }
            }
            match self.store.quality_reserved_intervals(&candidate.key).await {
                Ok(intervals) if intervals.is_empty() => {}
                Ok(_) => {
                    if let Some(candidate) = pending.pop_front() {
                        pending.push_back(candidate);
                    }
                    continue;
                }
                Err(error) => {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %candidate.key,
                        %error, "retaining obsolete media with unknown continuous dependencies");
                    break;
                }
            }
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
            let resource_runtime = self.resource_runtime.clone();
            let removal = tokio::task::spawn_blocking(move || {
                // This final check runs in the same blocking closure as the
                // recursive unlink, closing the gap after the async checks.
                if resource_runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.protects_output_path(&path))
                {
                    return Ok(false);
                }
                std::fs::remove_dir_all(path).map(|()| true)
            })
            .await;
            let gone = match removal {
                Ok(Ok(gone)) => gone,
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
        if let Some(durably_non_live) = self.hooks.get().terminal_route_outcome(session_id) {
            return durably_non_live;
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
    ) -> std::io::Result<MaterializeDemand> {
        let mut clocks = rendition.demand_since.lock().expect("demand lock");
        self.arm_materialize_watchdog_locked(rendition, index, &mut clocks)
    }

    pub(super) fn arm_materialize_watchdog_locked(
        self: &Arc<Self>,
        rendition: &Arc<Rendition>,
        index: u32,
        demands: &mut DemandTable,
    ) -> std::io::Result<MaterializeDemand> {
        let started = Instant::now();
        if let Some(clock) = demands.get_mut(&index) {
            clock.owners += 1;
            clock.retry_pending = false;
            return Ok(MaterializeDemand {
                rendition: Arc::clone(rendition),
                index,
                started: clock.started,
            });
        }
        if !demands.has_room_for(&index) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "finite demand table has no remaining slot",
            ));
        }
        let watchdog_future = |task_slot: Option<super::client_state::ClientSlot>| {
            let shared = Arc::clone(self);
            let rendition = Arc::clone(rendition);
            async move {
                // The task and its clock share this same original budget slot.
                let _task_slot = task_slot;
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
                // Name the phase the deadline caught rather than calling every
                // expiry a producer failure: a rendition parked on a refused
                // encoder permit never had a producer to fail.
                #[cfg(target_os = "linux")]
                let verdict = if rendition.finite_client_lease.is_some() {
                    match rendition.finite_materialize_verdict_bounded(
                        index,
                        rendition.producer_belief().await,
                    ) {
                        Ok(verdict) => verdict,
                        Err(error) => {
                            tracing::warn!(target: "plurxd::vodserve", entry = index,
                            %error, "finite materialization verdict exceeds client partition");
                            // Allocation uncertainty is not evidence that a
                            // producer failed. Keep the original owner/charge;
                            // normal request deadlines can still answer pending.
                            return;
                        }
                    }
                } else {
                    rendition.materialize_verdict(index, rendition.producer_belief().await)
                };
                #[cfg(not(target_os = "linux"))]
                let verdict =
                    rendition.materialize_verdict(index, rendition.producer_belief().await);
                #[cfg(target_os = "linux")]
                let verdict = if let Some(slot) = _task_slot.as_ref() {
                    match verdict.into_finite(slot) {
                        Ok(verdict) => verdict,
                        Err(error) => {
                            tracing::warn!(target: "plurxd::vodserve", %error,
                            "finite materialization failure allocation refused");
                            return;
                        }
                    }
                } else {
                    verdict
                };
                let verdict = {
                    let mut clocks = rendition.demand_since.lock().expect("demand lock");
                    let Some(clock) = clocks
                        .get_mut(&index)
                        .filter(|clock| clock.started == started)
                    else {
                        return;
                    };
                    if let Some(recorded) = &clock.verdict {
                        recorded.clone()
                    } else {
                        clock.verdict = Some(verdict.clone());
                        verdict
                    }
                };
                tracing::warn!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key,
                    entry = index,
                    verdict = ?verdict,
                    "materialize deadline expired"
                );
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
                    shared
                        .pool
                        .expire_entry(&rendition.key, index, verdict.wait_outcome());
                    drop(clocks);
                    rendition.kick();
                } else {
                    // Init waiters inspect their own captured clock after this wake.
                    rendition.init_notify.notify_waiters();
                }
                // Preserve expiry long enough for a normal Retry-After cycle to
                // receive its typed failure, then forget an abandoned HTTP retry.
                // An old task never removes a newer episode for this same entry.
                tokio::time::sleep(MATERIALIZE_EXPIRY_RETAINED).await;
                let mut demands = rendition.demand_since.lock().expect("demand lock");
                if demands
                    .get(&index)
                    .is_some_and(|clock| clock.started == started && clock.owners == 0)
                {
                    demands.remove(&index);
                }
            }
        };
        let prototype = watchdog_future(None);
        let _future_bytes = std::mem::size_of_val(&prototype);
        drop(prototype);
        #[cfg(target_os = "linux")]
        let slot = if let Some(lease) = rendition.finite_client_lease.as_ref() {
            // One watchdog and one immediate-expiry request may construct a
            // payload concurrently. The chosen immutable diagnostic/body then
            // shares this same slot through every waiter and HTTP body.
            let payload = crate::waitpool::FiniteWaitFailure::required_payload_bytes()
                .and_then(|bytes| bytes.checked_mul(2))
                .and_then(|bytes| _future_bytes.checked_add(bytes))
                .ok_or_else(|| std::io::Error::other("finite watchdog size overflow"))?;
            Some(super::client_state::ClientSlot(lease.reserve(payload)?))
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let slot: Option<super::client_state::ClientSlot> = None;
        demands
            .try_insert(
                index,
                MaterializeClock {
                    started,
                    owners: 1,
                    retry_pending: false,
                    watchdog: None,
                    verdict: None,
                    _finite_slot: slot.clone(),
                },
            )
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "finite demand table filled after preflight",
                )
            })?;
        let owner = MaterializeDemand {
            rendition: Arc::clone(rendition),
            index,
            started,
        };
        let watchdog = tokio::spawn(watchdog_future(slot));
        if let Some(clock) = demands
            .get_mut(&index)
            .filter(|clock| clock.started == started)
        {
            clock.watchdog = Some(watchdog.abort_handle());
        } else {
            watchdog.abort();
        }
        Ok(owner)
    }

    /// Find or build the rendition for `key`, spawning its driver. `None`
    /// means the plan came out empty and the caller returns a typed refusal.
    #[cfg(test)]
    pub(super) async fn attach_rendition(
        self: &Arc<Shared>,
        key: &str,
        identity: &SourceIdentity,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        duration_ms: i64,
        settings: &VodSettings,
    ) -> Result<Option<RenditionAttachment>, String> {
        self.attach_rendition_with_storage(
            key,
            identity,
            index,
            recipe,
            duration_ms,
            settings,
            None,
            None,
        )
        .await
    }

    /// Build one selected finite software owner without publishing it to the
    /// serving map or starting its producer. The caller has already installed
    /// its bounded attempt slot and must retain this object until known Adopt
    /// or transfer it to exact finite cleanup.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_adaptive_attempt(
        self: &Arc<Shared>,
        key: String,
        identity: SourceIdentity,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        duration_ms: i64,
        settings: &VodSettings,
        application_owner_bytes: usize,
    ) -> Result<PreparedAttemptRendition, AdaptiveBuildFailure> {
        let build_guard = self.rendition_build_gate(&key).lock_owned().await;
        if self.finite_closing.lock().await.contains_key(&key)
            || self.renditions.lock().await.contains_key(&key)
        {
            return Err(AdaptiveBuildFailure::before_r(
                "adaptive rendition key already has an owner",
            ));
        }
        if !super::finite_software::selected(&key, &recipe, false, false) {
            return Err(AdaptiveBuildFailure::before_r(
                "adaptive attempt requires selected finite software AVC",
            ));
        }
        let plan = self
            .stored_plan(&key, &identity, &index, &recipe, duration_ms)
            .await
            .map_err(AdaptiveBuildFailure::before_r)?;
        if plan.is_empty() {
            return Err(AdaptiveBuildFailure::before_r(
                "adaptive attempt has no immutable segment plan",
            ));
        }
        let (rendition, application_lease) = super::finite_software::build_attempt(
            self,
            &key,
            index,
            recipe,
            plan,
            settings,
            application_owner_bytes,
        )
        .await?;
        Ok(PreparedAttemptRendition {
            shared: Arc::clone(self),
            key,
            rendition: Some(rendition),
            build_guard: Some(build_guard),
            application_lease: Some(application_lease),
            installed: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn attach_rendition_with_storage(
        self: &Arc<Shared>,
        key: &str,
        identity: &SourceIdentity,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        duration_ms: i64,
        settings: &VodSettings,
        private_storage: Option<Arc<super::copy_preparation::PreparationAllowance>>,
        canonical_processing_key: Option<(&str, Option<Arc<tokio::sync::OwnedSemaphorePermit>>)>,
    ) -> Result<Option<RenditionAttachment>, String> {
        if let Some(storage) = private_storage.as_ref() {
            storage.bind(key)?;
        }
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
        let canonical_processing_key =
            canonical_processing_key.map(|(key, allowance)| (key.to_owned(), allowance));
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
                    private_storage,
                    canonical_processing_key,
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
        private_storage: Option<Arc<super::copy_preparation::PreparationAllowance>>,
        canonical_processing_key: Option<(String, Option<Arc<tokio::sync::OwnedSemaphorePermit>>)>,
    ) -> Result<Option<RenditionAttachment>, String> {
        let key = key.as_str();
        #[cfg(target_os = "linux")]
        if self.finite_closing.lock().await.contains_key(key) {
            return Err("finite rendition cleanup remains in progress".into());
        }
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
                    #[cfg(unix)]
                    if existing.dir.is_finite() {
                        // An unresolved original finite producer/output is
                        // still the owner. Maintenance must finish its exact
                        // terminal transaction before another incarnation.
                        existing.closed.store(true, Release);
                        existing.kick();
                        return Err("finite rendition cleanup remains in progress".into());
                    }
                    if existing.ephemeral_processing {
                        return Err(
                            "processed incarnation requires physical storage cleanup".into()
                        );
                    }
                    if let Some(storage) = existing.private_storage.as_ref() {
                        storage.release();
                        return Err("private incarnation requires storage cleanup".into());
                    }
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
                    if stale.private_storage.is_some() {
                        return Err("private incarnation requires storage cleanup".into());
                    }
                    sub_saturating(&self.working_set, claimed);
                }
                tracing::info!(
                    target: "plurxd::vodserve",
                    rendition = %key, "replacing a failed rendition on create"
                );
            }
        }

        let phase_started = Instant::now();
        let plan = self
            .stored_plan(
                canonical_processing_key
                    .as_ref()
                    .map_or(key, |(canonical, _)| canonical.as_str()),
                &identity,
                &index,
                &recipe,
                duration_ms,
            )
            .await?;
        tracing::debug!(target: "plurxd::vodserve", rendition = %key,
            phase = "stored_plan", elapsed_ms = phase_started.elapsed().as_millis(),
            "rendition attachment phase completed");
        if plan.is_empty() {
            return Ok(None);
        }
        let phase_started = Instant::now();
        let rendition = self
            .build_rendition_with_storage(
                key,
                index,
                recipe,
                plan,
                &settings,
                private_storage.clone(),
                canonical_processing_key.is_some(),
                canonical_processing_key
                    .as_ref()
                    .and_then(|(_, allowance)| allowance.clone()),
            )
            .await?;
        tracing::debug!(target: "plurxd::vodserve", rendition = %key,
            phase = "build_rendition", elapsed_ms = phase_started.elapsed().as_millis(),
            "rendition attachment phase completed");
        let adopted_bytes = rendition.manifest.lock().await.materialized_bytes();
        let (rendition, installed) = {
            #[cfg(target_os = "linux")]
            let mut closings = self.finite_closing.lock().await;
            let mut renditions = self.renditions.lock().await;
            if let Some(winner) = renditions
                .get(key)
                .filter(|winner| winner.failure().is_none() && !winner.closed.load(Relaxed))
                .map(Arc::clone)
            {
                (winner, false)
            } else {
                #[cfg(target_os = "linux")]
                let cleanup = self.take_finite_build_for_publication(key, &rendition, &closings)?;
                renditions.insert(key.to_string(), Arc::clone(&rendition));
                #[cfg(target_os = "linux")]
                if let Some(cleanup) = cleanup {
                    cleanup.published(&mut closings);
                }
                (rendition, true)
            }
        };
        if installed {
            if rendition.ephemeral_processing {
                let shared = Arc::clone(self);
                let key = key.to_owned();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    shared.purge_if_dormant(&key, Duration::ZERO).await;
                });
            }
            if let Some(storage) = private_storage.as_ref() {
                storage.installed(&rendition);
            }
            if adopted_bytes > 0 && private_storage.is_none() {
                self.working_set.fetch_add(adopted_bytes, Relaxed);
            }
            let _driver = spawn_driver(Arc::clone(self), Arc::clone(&rendition));
            if adopted_bytes > 0 && private_storage.is_none() {
                self.preparation_storage.adopt(self, &rendition).await;
            }
            self.hooks.get().after_rendition_installed().await;
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
    pub(super) async fn stored_plan(
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
            encoding.media_plan(duration_ms)
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

    #[cfg(test)]
    pub(super) async fn build_rendition(
        self: &Arc<Shared>,
        key: &str,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        plan: SegmentPlan,
        settings: &VodSettings,
    ) -> Result<Arc<Rendition>, String> {
        self.build_rendition_with_storage(key, index, recipe, plan, settings, None, false, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_rendition_with_storage(
        self: &Arc<Shared>,
        key: &str,
        index: Option<FragmentIndex>,
        recipe: Recipe,
        plan: SegmentPlan,
        settings: &VodSettings,
        private_storage: Option<Arc<super::copy_preparation::PreparationAllowance>>,
        ephemeral_processing: bool,
        processed_instance_allowance: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    ) -> Result<Arc<Rendition>, String> {
        #[cfg(target_os = "linux")]
        if finite_software::selected(
            key,
            &recipe,
            private_storage.is_some(),
            ephemeral_processing,
        ) {
            return finite_software::build(self, key, index, recipe, plan, settings).await;
        }
        let output_path = self.base.join(key);
        if self
            .resource_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.protects_output_path(&output_path))
        {
            // Protection only prevents this legacy path from touching an
            // existing native-owned child. It does not reconcile or adopt it.
            return Err("rendition path is protected by physical resource custody".into());
        }
        let source = if key.starts_with("source-") {
            crate::fragment_index_cluster::open_source_playback_fence(
                &recipe.file,
                recipe.source_object_version.as_deref(),
            )
            .await?
        } else {
            crate::fragment_index_cluster::open_source_fence(
                &recipe.file,
                recipe.source_object_version.as_deref(),
            )
            .await?
        };
        let dir = RenditionDir::new(output_path);
        let mut existed = tokio::fs::metadata(dir.path()).await.is_ok();
        if let Some(storage) = private_storage.as_ref() {
            if existed {
                return Err("private namespace already exists; cleanup required".into());
            }
            storage.create_marker(&dir).await?;
        } else if super::preparation_storage::has_private_marker(dir.path())
            .await
            .map_err(|error| format!("checking private cache ownership: {error}"))?
        {
            return Err(
                "private preparation scratch cannot be adopted as ordinary playback".into(),
            );
        }
        let protected = !self
            .store
            .quality_reserved_intervals(key)
            .await
            .map_err(|error| format!("reading continuous media dependencies: {error}"))?
            .is_empty();
        if protected && !existed {
            return Err("a reserved rendition has lost its immutable directory; attachment repair is required".into());
        }

        let encoded_process = recipe
            .encoding
            .as_ref()
            .map(|encoding| encoding.engine.process_identity().to_owned());
        if existed {
            if let Some(process) = encoded_process.as_deref() {
                match tokio::fs::read_to_string(dir.path().join(ENCODED_PROCESS_NAME)).await {
                    Ok(owner) if owner.trim() == process => {}
                    Ok(_) => {
                        if protected {
                            return Err("a reserved rendition belongs to an earlier encoder process; attachment repair is required".into());
                        }
                        tokio::fs::remove_dir_all(dir.path())
                            .await
                            .map_err(|error| {
                                format!("removing an obsolete encoded rendition directory: {error}")
                            })?;
                        existed = false;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if protected {
                            return Err("a reserved rendition has no encoder process marker; attachment repair is required".into());
                        }
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
                        if key.starts_with("source-") {
                            return Err(crate::transcode::vod_refusal_error(
                                "vod_source_head_pending",
                                "Source cached-head recovery needs owned admitted activation",
                            ));
                        }
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
                                if protected {
                                    return Err(format!(
                                        "a reserved rendition's init cannot be verified: {why}"
                                    ));
                                }
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
                    if protected {
                        return Err("a reserved rendition has no immutable init identity; attachment repair is required".into());
                    }
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

        Ok(Self::construct_rendition(
            key,
            dir,
            recipe,
            Some(source),
            plan,
            index,
            settings,
            private_storage,
            ephemeral_processing,
            processed_instance_allowance,
            manifest,
            identity_state,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn construct_rendition(
        key: &str,
        dir: RenditionDir,
        recipe: Recipe,
        source: Option<crate::fragment_index_cluster::SourceFence>,
        plan: SegmentPlan,
        index: Option<FragmentIndex>,
        settings: &VodSettings,
        private_storage: Option<Arc<super::copy_preparation::PreparationAllowance>>,
        ephemeral_processing: bool,
        processed_instance_allowance: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
        manifest: Manifest,
        identity_state: IdentityState,
    ) -> Arc<Rendition> {
        let timescale = plan.timescale.max(1);
        let seconds_per_segment = if plan.is_empty() {
            1.0
        } else {
            plan.duration_ticks() as f64 / f64::from(timescale) / plan.len() as f64
        };
        let plan_len = plan.len();
        Arc::new(Rendition {
            ephemeral_processing,
            _processed_instance_allowance: processed_instance_allowance,
            key: key.to_string(),
            dir,
            recipe,
            source,
            playlist: plan.playlist().into_bytes(),
            plan,
            timescale,
            seconds_per_segment,
            index,
            policy: shipped_policy(timescale),
            working_set_budget: settings.working_set_bytes,
            // A private full-output obligation is admitted against its own
            // retained-output budget. Ordinary completed-cache admission stays
            // independently closed when its configured budget is zero.
            completed_cache_budget: if private_storage.is_some() {
                settings.output_budget_bytes
            } else {
                settings.completed_cache_bytes
            },
            materialize_budget: settings.materialize_budget,
            manifest: Mutex::new(manifest),
            output_measurement: StdMutex::new(PublishedOutputMeasurement::default()),
            private_storage,
            copy_preparation: StdMutex::new(None),
            preparation_epoch: AtomicU64::new(0),
            retained_offer: StdMutex::new(None),
            cancelled_preparation_epoch: AtomicU64::new(0),
            identity: Mutex::new(identity_state),
            slot: Arc::new(ProducerSlot::new()),
            source_owners: SourceRenditionOwners::default(),
            retained_admission: crate::vodencode::RetainedEncodeAdmission::default(),
            readers: Mutex::new(ReaderTable::legacy()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            unplaced: StdMutex::new(HashMap::new()),
            landing_origin: tokio::sync::OnceCell::new(),
            capacity_hold: StdMutex::new(None),
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            hooks: Box::new(NoopRenditionHooks),
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
            demand_since: StdMutex::new(DemandTable::legacy()),
            fragment_received: StdMutex::new(None),
            #[cfg(target_os = "linux")]
            finite_metadata_lease: None,
            #[cfg(target_os = "linux")]
            finite_client_lease: None,
        })
    }

    /// Completion → admission (plan §2.4): reserve, make durable, complete —
    /// in that order, because admission is the durability boundary and
    /// reversing it publishes a promise before the bytes behind it are real.
    pub(super) async fn try_admit(
        self: &Arc<Shared>,
        rendition: &Rendition,
        manifest: &mut Manifest,
    ) {
        if rendition.ephemeral_processing
            || rendition.private_storage.is_some()
            || rendition.closed.load(Acquire)
            || manifest.is_admitted()
        {
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
    /// Exact current-owner lookup, independent of namespace spelling. A
    /// recovered output without an original producer is protected; this does
    /// not add an obsolete recovered-cache disposer.
    #[cfg(target_os = "linux")]
    pub(super) async fn pressure_evict_finite_rendition(
        self: &Arc<Shared>,
        key: &str,
    ) -> Option<bool> {
        let closing = { self.finite_closing.lock().await.get(key).cloned() };
        if let Some(closing) = closing {
            self.retry_finite_closing(key.to_owned(), closing).await;
            return Some(false);
        }
        let original = self.renditions.lock().await.get(key).cloned()?;
        if !original.dir.is_finite() {
            return None;
        }
        let current = original.dir.finite_producer().is_some();
        drop(original);
        if current {
            self.purge_dormant_inner(key, Duration::ZERO, true).await;
        }
        Some(false)
    }

    pub(super) async fn purge_if_dormant(self: &Arc<Shared>, key: &str, ttl: Duration) {
        self.purge_dormant_inner(key, ttl, false).await;
    }

    async fn purge_dormant_inner(self: &Arc<Shared>, key: &str, ttl: Duration, pressure: bool) {
        let Ok(build_guard) = self.rendition_build_gate(key).try_lock_owned() else {
            return;
        };
        #[cfg(target_os = "linux")]
        let existing_closing = { self.finite_closing.lock().await.get(key).cloned() };
        #[cfg(target_os = "linux")]
        if let Some(closing) = existing_closing {
            drop(build_guard);
            self.retry_finite_closing(key.to_owned(), closing).await;
            return;
        }
        let rendition = {
            let renditions = self.renditions.lock().await;
            renditions.get(key).map(Arc::clone)
        };
        let Some(rendition) = rendition else {
            return;
        };
        // Original finite cancellation must remain reachable while its own
        // pending global quota grant pins the path. Legacy unlink still obeys
        // every cache pin.
        #[cfg(target_os = "linux")]
        let current_finite = rendition.dir.finite_producer().is_some();
        #[cfg(not(target_os = "linux"))]
        let current_finite = false;
        if !current_finite && crate::playback_cache::is_pinned(&self.base.join(key)) {
            return;
        }
        if let Some(storage) = rendition.private_storage.as_ref() {
            if rendition.preparation().is_none() {
                storage.release();
            }
            return;
        }
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
        if !rendition.ephemeral_processing {
            match self.store.quality_reserved_intervals(key).await {
                Ok(intervals) if intervals.is_empty() => {}
                Ok(_) => return,
                Err(error) => {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                    %error, "retaining dormant media with unknown continuous dependencies");
                    return;
                }
            }
        }
        // Keep the manifest fence through the short exact map removal. That
        // makes admission and purge mutually exclusive without ever holding
        // the node-wide registry while awaiting a rendition-local lock.
        let manifest = rendition.manifest.lock().await;
        if manifest.is_admitted() && !(pressure && current_finite) {
            return;
        }
        #[cfg(unix)]
        let counted_finite = rendition.dir.is_finite();
        #[cfg(not(unix))]
        let counted_finite = false;
        #[cfg(target_os = "linux")]
        if counted_finite {
            let Some(output) = rendition.dir.finite_output() else {
                return;
            };
            let closing = Arc::new(FiniteClosingOwner {
                rich: Mutex::new(Some(Arc::clone(&rendition))),
                output: Mutex::new(Some(output)),
                producer: Mutex::new(rendition.dir.finite_producer()),
                ticket: Mutex::new(None),
                cache_grant: std::sync::Mutex::new(None),
                claimed_bytes: AtomicU64::new(0),
                claimed_admitted: AtomicBool::new(false),
                known_closed: AtomicBool::new(false),
                running: AtomicBool::new(false),
                cleanup_ready: AtomicBool::new(true),
            });
            let removed = {
                let mut closings = self.finite_closing.lock().await;
                let mut renditions = self.renditions.lock().await;
                if closings.contains_key(key)
                    || !renditions
                        .get(key)
                        .is_some_and(|current| Arc::ptr_eq(current, &rendition))
                {
                    false
                } else {
                    // The tombstone and exact native owner exist before the
                    // rich registry reference goes away. This gate prevents
                    // any same-key build between the two short map writes.
                    closings.insert(key.to_owned(), Arc::clone(&closing));
                    renditions.remove(key);
                    rendition.closed.store(true, Release);
                    rendition.gen_epoch.fetch_add(1, Relaxed);
                    rendition.kick();
                    self.pool.close(key);
                    true
                }
            };
            drop(manifest);
            drop(rendition);
            drop(build_guard);
            if removed {
                self.retry_finite_closing(key.to_owned(), closing).await;
            }
            return;
        }
        if !counted_finite
            && self
                .resource_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.protects_output_path(rendition.dir.path()))
        {
            // Keep the rendition registered until its physical owner clears
            // the protection; legacy purge would bypass native cleanup.
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
                // A private processed instance remains discoverable and
                // charged until its directory has physically disappeared.
                if !rendition.ephemeral_processing && !counted_finite {
                    renditions.remove(key);
                }
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
            shared.hooks.get().after_dormant_purge_removed().await;
            let _ = perform_driver_step(
                &shared,
                &rendition,
                Step::Terminate {
                    why: Termination::Idle,
                },
            )
            .await;
            shared.pool.close(&rendition.key);
            if counted_finite {
                let mut manifest = rendition.manifest.lock().await;
                let freed = rendition.dir.purge(&mut manifest).await;
                if let Some(error) = freed.error {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %rendition.key,
                        %error, "retained finite cleanup remains charged for retry");
                    return;
                }
                sub_saturating(&shared.working_set, freed.bytes);
                drop(manifest);
                let mut registry = shared.renditions.lock().await;
                if registry
                    .get(&rendition.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &rendition))
                {
                    registry.remove(&rendition.key);
                }
                drop(registry);
                rendition.kick();
                shared.kick_all();
                return;
            }
            if shared
                .resource_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.protects_output_path(rendition.dir.path()))
            {
                return;
            }
            if rendition.ephemeral_processing {
                let deletion = if shared
                    .hooks
                    .get()
                    .private_cleanup_failure_for("processed_directory", &rendition.key)
                {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "processed cleanup seam",
                    ))
                } else {
                    tokio::fs::remove_dir_all(rendition.dir.path()).await
                };
                if let Err(error) = deletion {
                    if error.kind() != io::ErrorKind::NotFound {
                        tracing::warn!(target: "plurxd::vodserve", rendition = %rendition.key,
                            %error, "processed storage cleanup retained for retry");
                        return;
                    }
                }
                let claimed = rendition.manifest.lock().await.materialized_bytes();
                let mut registry = shared.renditions.lock().await;
                if registry
                    .get(&rendition.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &rendition))
                {
                    registry.remove(&rendition.key);
                    sub_saturating(&shared.working_set, claimed);
                }
                drop(registry);
                rendition.kick();
                shared.kick_all();
                return;
            }
            {
                let mut manifest = rendition.manifest.lock().await;
                if shared
                    .resource_runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.protects_output_path(rendition.dir.path()))
                {
                    return;
                }
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

    #[cfg(target_os = "linux")]
    pub(super) async fn reserve_finite_build_slot(
        self: &Arc<Self>,
        key: &str,
    ) -> Result<FiniteBuildSlot<'_>, String> {
        if key.is_empty() || key.len() > 255 {
            return Err("finite build key exceeds its original role bound".into());
        }
        let closings = self.finite_closing.lock().await;
        if closings.contains_key(key) {
            return Err("finite rendition cleanup remains in progress".into());
        }
        Ok(FiniteBuildSlot {
            shared: self,
            key: key.to_owned(),
            closings,
        })
    }

    #[cfg(target_os = "linux")]
    fn take_finite_build_for_publication(
        self: &Arc<Self>,
        key: &str,
        rendition: &Rendition,
        closings: &std::collections::BTreeMap<String, Arc<FiniteClosingOwner>>,
    ) -> Result<Option<FiniteBuildCleanup>, String> {
        let cleanup = rendition.dir.take_finite_build_cleanup();
        if let Some(cleanup) = cleanup.as_ref() {
            cleanup.validate_publication(self, key, closings)?;
        } else if closings.contains_key(key) {
            return Err("finite rendition cleanup remains in progress".into());
        }
        Ok(cleanup)
    }

    #[cfg(target_os = "linux")]
    fn spawn_finite_closing(
        self: &Arc<Shared>,
        key: String,
        closing: Arc<FiniteClosingOwner>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !closing.cleanup_ready.load(Acquire) || closing.running.swap(true, AcqRel) {
            return None;
        }
        let shared = Arc::clone(self);
        Some(tokio::spawn(async move {
            let _running = FiniteClosingRun(Arc::clone(&closing));
            shared.drive_finite_closing(&key, &closing).await;
        }))
    }

    #[cfg(target_os = "linux")]
    async fn retry_finite_closing(
        self: &Arc<Shared>,
        key: String,
        closing: Arc<FiniteClosingOwner>,
    ) {
        // The owner and exact operation remain in the map if this waiter is
        // cancelled, the native result is unknown, or the task fails.
        if let Some(task) = self.spawn_finite_closing(key, closing) {
            let _ = task.await;
        }
    }

    /// Fresh R was prepared but never reached the serving registry/driver.
    /// The production caller already holds this key's build gate. Keep the
    /// original owner discoverable before returning its construction error;
    /// the detached cleanup must not await that still-held gate here.
    #[cfg(target_os = "linux")]
    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) async fn retain_unattached_finite(
        self: &Arc<Shared>,
        key: &str,
        rich: Option<Arc<Rendition>>,
        output: plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput,
        producer: Arc<super::finite_software::FiniteSoftwareProducer>,
    ) {
        let _ = self
            .retain_unattached_finite_with_handle(key, rich, output, producer)
            .await;
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn retain_unattached_finite_with_handle(
        self: &Arc<Shared>,
        key: &str,
        rich: Option<Arc<Rendition>>,
        output: plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput,
        producer: Arc<super::finite_software::FiniteSoftwareProducer>,
    ) -> Arc<FiniteClosingOwner> {
        let build_cleanup = rich
            .as_ref()
            .and_then(|rendition| rendition.dir.take_finite_build_cleanup());
        if let Some(cleanup) = build_cleanup {
            let closing = Arc::clone(&cleanup.closing);
            // Remove the arm from PendingWrites before retaining rich, so
            // map -> rendition can never lead back to this closing owner.
            *closing.rich.lock().await = rich;
            drop(output);
            drop(producer);
            drop(cleanup); // Arms the existing original map slot synchronously.
            return closing;
        }
        let closing = Arc::new(FiniteClosingOwner {
            rich: Mutex::new(rich),
            output: Mutex::new(Some(output)),
            producer: Mutex::new(Some(producer)),
            ticket: Mutex::new(None),
            cache_grant: std::sync::Mutex::new(None),
            claimed_bytes: AtomicU64::new(0),
            claimed_admitted: AtomicBool::new(false),
            known_closed: AtomicBool::new(false),
            running: AtomicBool::new(false),
            cleanup_ready: AtomicBool::new(true),
        });
        {
            let mut closings = self.finite_closing.lock().await;
            use std::collections::btree_map::Entry;
            match closings.entry(key.to_owned()) {
                Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(&closing));
                }
                Entry::Occupied(_) => {
                    panic!("fresh finite preparation collided under its exact build gate");
                }
            }
        }
        let _task = self.spawn_finite_closing(key.to_owned(), Arc::clone(&closing));
        closing
    }

    #[cfg(target_os = "linux")]
    async fn drive_finite_closing(
        self: &Arc<Shared>,
        key: &str,
        closing: &Arc<FiniteClosingOwner>,
    ) {
        let rich = closing.rich.lock().await.as_ref().cloned();
        if let Some(rendition) = rich {
            let producer = closing.producer.lock().await.as_ref().cloned();
            if let Some(producer) = producer.as_ref() {
                if let Err(error) = producer.request_stop() {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                        %error, "finite producer stop remains pending");
                }
                rendition.kick();
                if !producer.driver_never_registered() {
                    match producer
                        .wait_driver_quiescent(Instant::now() + Duration::from_secs(30))
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                            "finite driver aborted; original native owner still must close"),
                        Err(error) => {
                            tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                                %error, "finite driver still owns rendition metadata");
                            return;
                        }
                    }
                }
            }
            // The actual driver no longer reports Materialized events. This
            // final manifest count is stable even if its early stop raced a
            // last queued event. The metadata lease remains in the rendition
            // until every other real Arc (request, session, generation) ends.
            let manifest = rendition.manifest.lock().await;
            closing
                .claimed_bytes
                .store(manifest.materialized_bytes(), Release);
            closing
                .claimed_admitted
                .store(manifest.is_admitted(), Release);
            drop(manifest);
            closing.rich.lock().await.take();
            drop(rendition);
            drop(producer);
        }

        if !closing.known_closed.load(Acquire) {
            if closing.ticket.lock().await.is_none() {
                let deadline = Instant::now() + Duration::from_secs(30);
                let output = closing.output.lock().await.as_ref().cloned();
                let Some(output) = output else {
                    return;
                };
                if let Err(error) = output.wait_allocations_release(deadline).await {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                        %error, "finite metadata or response remains live; cleanup retained");
                    return;
                }
                let producer = closing.producer.lock().await.as_ref().cloned();
                let prepared = if let Some(producer) = producer.as_ref() {
                    producer
                        .prepare_terminate(deadline)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    output
                        .prepare_eviction(deadline)
                        .await
                        .map_err(|error| error.to_string())
                };
                let ticket = match prepared {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                            %error, "original finite close remains charged for retry");
                        return;
                    }
                };
                // Install the exact original close selector before discarding
                // the last rich closing owner. Other selectors retain their
                // own final-drop guard and will delay final refund.
                if let Some(producer) = producer.as_ref() {
                    if let Some(grant) = producer.take_cache_grant() {
                        *closing
                            .cache_grant
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(grant);
                    }
                }
                *closing.ticket.lock().await = Some(ticket);
                drop(producer);
                drop(output);
                closing.producer.lock().await.take();
                closing.output.lock().await.take();
            }
            let deadline = Instant::now() + Duration::from_secs(30);
            {
                let mut held = closing.ticket.lock().await;
                let Some(ticket) = held.as_mut() else {
                    return;
                };
                if let Err(error) = ticket.wait_application_release(deadline).await {
                    tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                        %error, "finite application alias remains charged for retry");
                    return;
                }
            }
        }

        // The rich metadata and every application selector have drained. The
        // short key gate now spans the known journal commit and tombstone
        // removal, so no uncharged closing-map allocation can wait behind a
        // successor build after the role's final refund.
        let _build_guard = self.rendition_build_gate(key).lock_owned().await;
        if !closing.known_closed.load(Acquire) {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut held = closing.ticket.lock().await;
            let Some(ticket) = held.as_mut() else {
                return;
            };
            if let Err(error) = ticket.finish_retirement(deadline).await {
                tracing::warn!(target: "plurxd::vodserve", rendition = %key,
                    %error, "finite terminal close remains charged for retry");
                return;
            }
            if let Some(grant) = closing
                .cache_grant
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                grant.known_closed();
            }
            closing.known_closed.store(true, Release);
            held.take();
        }
        let removed = {
            let mut closings = self.finite_closing.lock().await;
            if closings
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, closing))
            {
                closings.remove(key);
                if closings.is_empty() {
                    // BTreeMap can retain an empty root after removal. Drop
                    // it so no historical registry capacity remains owned.
                    closings.clear();
                }
                true
            } else {
                false
            }
        };
        if removed {
            sub_saturating(
                if closing.claimed_admitted.load(Acquire) {
                    &self.completed_cache
                } else {
                    &self.working_set
                },
                closing.claimed_bytes.load(Acquire),
            );
            self.kick_all();
        }
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
