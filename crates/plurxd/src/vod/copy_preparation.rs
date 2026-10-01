//! Private background preparation accounting; never viewer demand or wire authority.
use super::*;

/// Private complete body held across the queue statement. Neither the job's
/// result reference nor this handle is a remotely reconstructible capability.
pub(crate) struct PreparedCopyOutput {
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    preparation: Arc<CopyPreparation>,
    artifact: Arc<super::retained::RetainedVodArtifact>,
    exposed: bool,
}

pub(super) struct PreparationRun {
    pub(super) rendition: Arc<Rendition>,
    pub(super) preparation: Arc<CopyPreparation>,
    pub(super) armed: bool,
}

impl Drop for PreparationRun {
    fn drop(&mut self) {
        if self.armed {
            let current = self.rendition.preparation();
            if current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.preparation))
            {
                self.rendition.revoke_preparation();
            } else {
                self.preparation.release();
            }
        }
    }
}

impl VodServe {
    pub(crate) fn preparation_attachment_observation(&self) -> u64 {
        *self
            .shared
            .preparation_attachment
            .lock()
            .expect("attachment observation")
    }
    pub(super) async fn wait_prepared_copy(
        &self,
        mut run: PreparationRun,
        still_idle: impl Fn() -> bool,
    ) -> Result<PreparedCopyOutput, String> {
        let lost = run.preparation.fence.loss_token();
        loop {
            // Register before inspection to avoid losing assembly completion.
            let progress = run.preparation.progress.notified();
            tokio::pin!(progress);
            progress.as_mut().enable();
            if !still_idle() || !run.preparation.live(&run.rendition).await {
                return Err("copy preparation unavailable or preempted".to_owned());
            }
            if let Some(artifact) = run.preparation.staged() {
                run.armed = false;
                return Ok(PreparedCopyOutput {
                    shared: Arc::clone(&self.shared),
                    rendition: Arc::clone(&run.rendition),
                    preparation: Arc::clone(&run.preparation),
                    artifact,
                    exposed: false,
                });
            }
            tokio::select! {
                _ = lost.cancelled() => return Err("copy preparation lease lost".to_owned()),
                _ = tokio::time::sleep_until(run.preparation.deadline.into()) =>
                    return Err("copy preparation deadline".to_owned()),
                _ = &mut progress => {},
            }
        }
    }
}

impl PreparedCopyOutput {
    #[cfg(test)]
    pub(super) fn private_facts(&self) -> crate::transcode::RetainedOutputFacts {
        self.artifact.facts()
    }
    pub(crate) async fn settle_and_expose(
        mut self,
        intent: &plurx_core::store::background_jobs::CopyOutputIntent,
    ) -> Result<bool, String> {
        if !self.preparation.live(&self.rendition).await
            || !recipe_engine_is_current(&self.rendition.recipe).await
        {
            return Ok(false);
        }
        let rates = &self.artifact.observation.rates;
        let facts = self.artifact.facts();
        let output = plurx_core::store::background_jobs::CopyOutputJobOutput {
            artifact_id: facts.artifact_id,
            output_identity: facts.output_identity,
            source_object_version: self.preparation.source_version.clone(),
            wire_bytes: i64::try_from(rates.wire_bytes).map_err(|_| "copy byte overflow")?,
            duration_micros: i64::try_from(rates.duration_micros)
                .map_err(|_| "copy duration overflow")?,
            average_bps: rates.average_bps,
            peak_bps: rates.rfc_peak_bps,
        };
        let published = self
            .preparation
            .fence
            .publish_copy_output(intent.clone(), output)
            .await
            .map_err(|error| error.to_string())?;
        if !published {
            return Ok(false);
        }
        // SQL completion is historical. A source change after the statement
        // can make exposure unavailable; never recreate acquire from its ack.
        self.exposed = self
            .shared
            .retained_artifacts
            .expose_prepared(
                &self.shared,
                &self.rendition,
                &self.preparation,
                &self.artifact,
            )
            .await;
        Ok(self.exposed)
    }
}

impl Drop for PreparedCopyOutput {
    fn drop(&mut self) {
        if self.exposed {
            let mut owner = self
                .rendition
                .copy_preparation
                .lock()
                .expect("copy preparation");
            if owner
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.preparation))
            {
                owner.take();
                self.rendition.preparation_epoch.store(0, Release);
            }
            drop(owner);
            self.preparation.release();
            self.rendition.kick();
        } else {
            self.rendition.revoke_preparation();
        }
    }
}

pub(super) struct CopyPreparation {
    pub(super) allowance: Arc<PreparationAllowance>,
    pub(super) fence: crate::background_jobs::JobFence,
    pub(super) token: plurx_core::store::background_jobs::JobToken,
    pub(super) deadline: Instant,
    pub(super) logical: super::retained_manifest::LogicalOutput,
    pub(super) source_version: String,
    admissions: crate::admission::Admissions,
    attachment_observation: u64,
    pub(super) executable: Arc<crate::ffmpeg::EncodedExecutable>,
    pub(super) engine: crate::ffmpeg::EncodedEngine,
    active: AtomicBool,
    failed: AtomicBool,
    pub(super) progress: Notify,
    staged: StdMutex<Option<Arc<super::retained::RetainedVodArtifact>>>,
}

impl CopyPreparation {
    // Keep the independent immutable source, engine and lease facts explicit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        allowance: Arc<PreparationAllowance>,
        fence: crate::background_jobs::JobFence,
        token: plurx_core::store::background_jobs::JobToken,
        deadline: Instant,
        logical: super::retained_manifest::LogicalOutput,
        source_version: String,
        admissions: crate::admission::Admissions,
        media_engine: (
            Arc<crate::ffmpeg::EncodedExecutable>,
            crate::ffmpeg::EncodedEngine,
        ),
        attachment_observation: u64,
    ) -> Self {
        Self {
            allowance,
            fence,
            token,
            deadline,
            logical,
            source_version,
            admissions,
            attachment_observation,
            executable: media_engine.0,
            engine: media_engine.1,
            active: AtomicBool::new(true),
            failed: AtomicBool::new(false),
            progress: Notify::new(),
            staged: StdMutex::new(None),
        }
    }

    pub(super) async fn live(&self, rendition: &Rendition) -> bool {
        let owned = self.fence.snapshot().await.is_some_and(|token| {
            token.job_id == self.token.job_id
                && token.node_id == self.token.node_id
                && token.boot_id == self.token.boot_id
                && token.claim_id == self.token.claim_id
                && token.fence == self.token.fence
                && token.revision >= self.token.revision
        });
        self.active.load(Acquire)
            && self.attachment_current()
            && !self.failed.load(Acquire)
            && self
                .engine
                .is_current_with_executable(&self.executable)
                .await
            && !self.admissions.background_must_yield()
            && Instant::now() < self.deadline
            && owned
            && !rendition.closed.load(Relaxed)
            && rendition.failure().is_none()
            && rendition.recipe.encoding.is_none()
            && rendition.recipe.retained_logical.as_ref() == Some(&self.logical)
            && rendition.source.as_ref().is_some_and(|source| {
                source.unchanged() && source.object_version() == self.source_version
            })
    }

    pub(super) fn release(&self) {
        let mut staged = self.staged.lock().expect("prepared artifact");
        if self.active.swap(false, AcqRel) {
            if let Some(artifact) = staged.take() {
                if let Some(shared) = self.allowance.shared.upgrade() {
                    shared.retained_artifacts.retire_prepared(artifact);
                }
            }
            self.allowance.release();
            self.progress.notify_waiters();
        }
    }

    pub(super) fn fail(&self) {
        self.failed.store(true, Release);
        self.progress.notify_waiters();
    }

    pub(super) fn stage(&self, artifact: Arc<super::retained::RetainedVodArtifact>) -> bool {
        let mut staged = self.staged.lock().expect("prepared artifact");
        if !self.active.load(Acquire) || Instant::now() >= self.deadline || staged.is_some() {
            return false;
        }
        *staged = Some(artifact);
        self.progress.notify_waiters();
        true
    }

    pub(super) fn staged(&self) -> Option<Arc<super::retained::RetainedVodArtifact>> {
        self.staged.lock().expect("prepared artifact").clone()
    }

    /// Settlement removes the job lease; this check is deliberately not
    /// `live()`. Publication still requires the original physical/logical
    /// source and the private staged body, independently of the SQL receipt.
    pub(super) fn publication_current(&self, rendition: &Rendition) -> bool {
        self.active.load(Acquire)
            && self.attachment_current()
            && Instant::now() < self.deadline
            && !rendition.closed.load(Acquire)
            && rendition.recipe.retained_logical.as_ref() == Some(&self.logical)
            && rendition.source.as_ref().is_some_and(|source| {
                source.unchanged() && source.object_version() == self.source_version
            })
    }

    pub(super) fn publish_staged(
        &self,
        rendition: &Rendition,
        expected: &Arc<super::retained::RetainedVodArtifact>,
        publish: impl FnOnce(Arc<super::retained::RetainedVodArtifact>),
    ) -> bool {
        let Some(shared) = self.allowance.shared.upgrade() else {
            return false;
        };
        let observation = shared
            .preparation_attachment
            .lock()
            .expect("attachment observation");
        let mut staged = self.staged.lock().expect("prepared artifact");
        if *observation != self.attachment_observation
            || *observation == u64::MAX
            || !self.active.load(Acquire)
            || Instant::now() >= self.deadline
            || rendition.closed.load(Acquire)
            || rendition.recipe.retained_logical.as_ref() != Some(&self.logical)
            || !rendition.source.as_ref().is_some_and(|source| {
                source.unchanged() && source.object_version() == self.source_version
            })
            || !staged
                .as_ref()
                .is_some_and(|artifact| Arc::ptr_eq(artifact, expected))
        {
            return false;
        }
        publish(staged.take().expect("checked private artifact"));
        true
    }

    fn attachment_current(&self) -> bool {
        self.allowance.shared.upgrade().is_some_and(|shared| {
            let observation = shared
                .preparation_attachment
                .lock()
                .expect("attachment observation");
            *observation == self.attachment_observation && *observation != u64::MAX
        })
    }
}

impl Drop for CopyPreparation {
    fn drop(&mut self) {
        self.release();
    }
}

impl Rendition {
    pub(super) fn preparation(&self) -> Option<Arc<CopyPreparation>> {
        self.copy_preparation
            .lock()
            .expect("copy preparation")
            .clone()
    }

    pub(super) fn revoke_preparation(&self) {
        let preparation = self.preparation();
        if let Some(preparation) = preparation {
            // Refuse measurement before removing the origin binding, so an
            // offer cannot misclassify a cancelled background body as normal.
            self.output_measurement
                .lock()
                .expect("output measurement lock")
                .refuse();
            self.copy_preparation
                .lock()
                .expect("copy preparation")
                .take();
            let owned = self.preparation_epoch.swap(0, AcqRel);
            if owned != 0 {
                self.cancelled_preparation_epoch.store(owned, Release);
            }
            preparation.release();
            self.kick();
        }
    }
}

/// Fences only the background epoch under the existing key build gate.
pub(super) async fn fence_cancelled_epoch(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    epoch: u64,
) -> bool {
    if rendition.cancelled_preparation_epoch.load(Acquire) != epoch.saturating_add(1) {
        return false;
    }
    let _build = shared
        .rendition_build_gate(&rendition.key)
        .lock_owned()
        .await;
    let readers = rendition.readers.lock().await;
    if !readers.is_empty() {
        rendition.cancelled_preparation_epoch.store(0, Release);
        return false;
    }
    if rendition.gen_epoch.load(Relaxed) != epoch {
        return true;
    }
    rendition.gen_epoch.fetch_add(1, Relaxed);
    drop(readers);
    let _ = perform_driver_step(
        shared,
        rendition,
        Step::Terminate {
            why: Termination::Idle,
        },
    )
    .await;
    true
}

#[derive(Default)]
pub(super) struct Footprint {
    committed: u64,
    inflight: u64,
    horizon_media: u64,
    released: bool,
}

/// The full footprint includes init/playlist/identity and in-flight bytes.
/// Only media already charged to Shared::working_set enters horizon_media.
pub(super) struct PreparationAllowance {
    shared: Weak<Shared>,
    pub(super) nonce: uuid::Uuid,
    pub(super) cap: u64,
    footprint: StdMutex<Footprint>,
}

impl PreparationAllowance {
    pub(super) fn new(shared: &Arc<Shared>, nonce: uuid::Uuid, cap: u64) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            nonce,
            cap,
            footprint: StdMutex::default(),
        }
    }

    pub(super) fn begin(self: &Arc<Self>, bytes: u64) -> Option<PendingFootprint> {
        let mut state = self.footprint.lock().expect("preparation footprint");
        let pending = state.inflight.checked_add(bytes)?;
        if state.released || state.committed.checked_add(pending)? > self.cap {
            return None;
        }
        state.inflight = pending;
        Some(PendingFootprint {
            allowance: Arc::clone(self),
            bytes,
            finished: false,
        })
    }

    pub(super) fn release(&self) {
        let media = {
            let mut state = self.footprint.lock().expect("preparation footprint");
            if state.released {
                return;
            }
            state.released = true;
            std::mem::take(&mut state.horizon_media)
        };
        if let Some(shared) = self.shared.upgrade() {
            // The physical media still exists: remove only the exclusion,
            // never its actual working-set charge. Cleanup owns that charge.
            sub_saturating(&shared.preparation_media, media);
            shared.retained_artifacts.release_preparation(self.nonce);
            shared.kick_all();
        }
    }

    pub(super) fn footprint(&self) -> Option<u64> {
        let state = self.footprint.lock().expect("preparation footprint");
        (!state.released && state.inflight == 0).then_some(state.committed)
    }
}

impl Drop for PreparationAllowance {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) struct PendingFootprint {
    allowance: Arc<PreparationAllowance>,
    bytes: u64,
    finished: bool,
}

impl PendingFootprint {
    /// Caller holds the manifest publication lock and has already charged the
    /// successful media publication to Shared::working_set, if media=true.
    pub(super) fn commit(mut self, media: bool) -> bool {
        let mut state = self
            .allowance
            .footprint
            .lock()
            .expect("preparation footprint");
        state.inflight = state
            .inflight
            .checked_sub(self.bytes)
            .expect("owned pending charge");
        self.finished = true;
        if state.released {
            return false;
        }
        state.committed = state
            .committed
            .checked_add(self.bytes)
            .expect("bounded footprint");
        if media {
            state.horizon_media = state
                .horizon_media
                .checked_add(self.bytes)
                .expect("bounded media");
            if let Some(shared) = self.allowance.shared.upgrade() {
                shared.preparation_media.fetch_add(self.bytes, Relaxed);
            }
        }
        true
    }
}

impl Drop for PendingFootprint {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self
                .allowance
                .footprint
                .lock()
                .expect("preparation footprint");
            state.inflight = state
                .inflight
                .checked_sub(self.bytes)
                .expect("owned pending charge");
        }
    }
}
