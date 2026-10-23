//! Ordinary software VOD owns one concrete finite producer through retirement.
//! Native resources remain inside core; events follow durable publication and
//! the daemon manifest is only a serving index, never settlement authority.
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteFile, FiniteWriterOutcome, SoftwareVodControl, SoftwareVodEvent, SoftwareVodOwner,
};
use plurx_core::transcode::{DecodeBackend, Encoder};
use std::sync::atomic::AtomicU8;

const PREPARED: u8 = 0;
const RUNNING: u8 = 1;
const RETIRING: u8 = 2;
const RETAINED: u8 = 3;
const FAILED: u8 = 4;
const SETTLED: u8 = 5;
const PREPARING: u8 = 6;
const IDENTITY_METADATA_BYTES: u64 = 64 * 1024;
const ENCODED_MARKER_BYTES: u64 = 4 * 1024;

/// Persistent application owner. Stage is diagnostic/control serialization;
/// only core's original receipts and known journal commits release resources.
pub(crate) struct FiniteSoftwareProducer {
    owner: Mutex<SoftwareVodOwner>,
    control: SoftwareVodControl,
    stage: AtomicU8,
    produced_through: AtomicU64,
    suspended: AtomicBool,
}
impl std::fmt::Debug for FiniteSoftwareProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FiniteSoftwareProducer")
            .field("stage", &self.stage.load(Acquire))
            .finish_non_exhaustive()
    }
}
impl FiniteSoftwareProducer {
    pub(crate) fn new(owner: SoftwareVodOwner) -> Self {
        Self {
            control: owner.control(),
            owner: Mutex::new(owner),
            stage: AtomicU8::new(PREPARED),
            produced_through: AtomicU64::new(0),
            suspended: AtomicBool::new(false),
        }
    }
    pub(crate) fn request_stop(&self) -> io::Result<()> {
        self.control.request_stop()
    }
    pub(crate) async fn perform(&self, step: Step) -> io::Result<Performed> {
        match step {
            Step::Stop => {
                self.control.suspend()?;
                self.suspended.store(true, Release);
            }
            Step::Resume => {
                self.control.resume()?;
                self.suspended.store(false, Release);
            }
            Step::Terminate { .. } => {
                self.control.request_stop()?;
                let owner = self.owner.lock().await;
                let deadline = Instant::now() + Duration::from_secs(30);
                match self.stage.load(Acquire) {
                    RETAINED | SETTLED => {}
                    RETIRING => {
                        owner
                            .retire_output(deadline)
                            .await
                            .map_err(io::Error::other)?;
                        self.stage.store(RETAINED, Release);
                    }
                    _ => self.cancel_owned(&owner, deadline).await?,
                }
            }
            _ => {}
        }
        Ok(Performed::Done)
    }
    /// Executor/diagnostic belief only; settlement uses core native receipts.
    pub(crate) fn belief(&self) -> Producer {
        let produced_through = self
            .produced_through
            .load(Acquire)
            .checked_sub(1)
            .and_then(|value| u32::try_from(value).ok());
        if self.stage.load(Acquire) == RUNNING {
            if self.suspended.load(Acquire) {
                Producer::Stopped {
                    produced_through,
                    positioned_at: 0,
                    reason: crate::prodsched::Hold::Ahead {
                        horizon: produced_through.unwrap_or(0),
                    },
                }
            } else {
                Producer::Running {
                    produced_through,
                    positioned_at: 0,
                }
            }
        } else {
            Producer::Absent { produced_through }
        }
    }
    pub(crate) fn settled(&self) -> bool {
        self.stage.load(Acquire) == SETTLED
    }
    async fn cancel_owned(&self, owner: &SoftwareVodOwner, deadline: Instant) -> io::Result<()> {
        if owner.cancel(deadline).await.is_err() {
            owner
                .cancel_failed(deadline)
                .await
                .map_err(io::Error::other)?;
        }
        self.stage.store(SETTLED, Release);
        Ok(())
    }
}

pub(super) fn selected(key: &str, recipe: &Recipe, private: bool, ephemeral: bool) -> bool {
    !key.starts_with("source-") && selected_recipe(recipe, private, ephemeral)
}

/// Also used before canonical key hashing, so the finite identity and the
/// eventual finite builder select exactly the same ordinary recipe.
pub(super) fn selected_recipe(recipe: &Recipe, private: bool, ephemeral: bool) -> bool {
    !private
        && !ephemeral
        && recipe.encoding.as_ref().is_some_and(|encoding| {
            encoding.plan.encoder() == Encoder::Software
                && encoding.plan.decode().backend() == DecodeBackend::Software
                && encoding.plan.output_contract().output_codec() == "h264"
                && encoding.plan.options().strict_dolby.is_none()
                && encoding.plan.macos_processing_identity().is_none()
                && encoding.plan.options().subtitle_burn.is_none()
                && encoding.subtitle.is_none()
                && encoding.dv_runtime.is_none()
                && encoding.shared_audio.is_none()
                && recipe.file.dolby_vision.is_empty()
        })
}

pub(super) async fn build(
    shared: &Arc<Shared>,
    key: &str,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    plan: SegmentPlan,
    settings: &VodSettings,
) -> Result<Arc<Rendition>, String> {
    let runtime = shared
        .resource_runtime
        .as_ref()
        .ok_or("finite resource runtime unavailable")?;
    if plan.timescale == 0 || plan.entries.is_empty() || key.is_empty() {
        return Err("finite software profile requires a nonempty plan and rendition key".into());
    }
    let object_version = finite_profile::source_version(&recipe)?;
    let segment_count = u32::try_from(plan.len())
        .map_err(|_| "finite software segment count overflows".to_owned())?;
    let media_file_id = recipe.file.id.to_string();
    let recovered = runtime
        .recovered_software_output(key, &media_file_id, &object_version, segment_count)
        .await
        .map_err(|error| format!("recovered finite software output is unresolved: {error}"))?;
    if let Some(output) = recovered {
        let deadline = Instant::now() + settings.materialize_budget;
        let identity_len = output
            .file_len(FiniteFile::IdentityMetadata)
            .await
            .map_err(|error| format!("reading recovered finite identity length: {error}"))?
            .ok_or("recovered finite identity is missing")?;
        let mut identity_file = ReadyFile::Finite {
            output: output.clone(),
            file: FiniteFile::IdentityMetadata,
            len: identity_len,
        };
        let identity_bytes = identity_file
            .read_all_bounded(IDENTITY_METADATA_BYTES, deadline)
            .await
            .map_err(|error| format!("reading recovered finite identity: {error}"))?;
        let identity = init::decode_identity(&identity_bytes)
            .ok_or("recovered finite identity is malformed")?;
        drop(identity_bytes);
        if !output.has_encoded_process_metadata() {
            return Err("recovered finite process marker is absent".into());
        }
        let marker_len = output
            .file_len(FiniteFile::EncodedProcessMetadata)
            .await
            .map_err(|error| format!("reading recovered finite marker length: {error}"))?
            .ok_or("recovered finite process marker is missing")?;
        let mut marker_file = ReadyFile::Finite {
            output: output.clone(),
            file: FiniteFile::EncodedProcessMetadata,
            len: marker_len,
        };
        let marker = marker_file
            .read_all_bounded(ENCODED_MARKER_BYTES, deadline)
            .await
            .map_err(|error| format!("reading recovered finite process marker: {error}"))?;
        let current = recipe
            .encoding
            .as_ref()
            .ok_or("recovered finite encoding is missing")?
            .engine
            .stable_software_digest()
            .ok_or("same-boot finite software engine identity unavailable")?;
        if std::str::from_utf8(&marker).ok().map(str::trim) != Some(current) {
            return Err("recovered finite output belongs to another boot or engine".into());
        }
        let mut manifest = Manifest::new(plan.clone());
        for entry in 0..segment_count {
            let len = output
                .file_len(FiniteFile::Segment(entry))
                .await
                .map_err(|error| format!("reading recovered finite segment {entry}: {error}"))?
                .ok_or_else(|| format!("recovered finite segment {entry} is missing"))?;
            if !manifest.materialize(entry, len, now_ms()) {
                return Err("recovered finite segment is outside the immutable plan".into());
            }
        }
        let dir = RenditionDir::from_finite(output);
        return Ok(Shared::construct_rendition(
            key,
            dir,
            recipe,
            None,
            plan,
            index,
            settings,
            None,
            false,
            None,
            manifest,
            IdentityState {
                identity: Some(identity),
                from_disk: true,
            },
        ));
    }
    // Read coherent saved intent before acquisition. Automatic/absent retain
    // the conservative local default until actual learning authorizes a plan;
    // this does not rewrite numeric rollback values or provenance.
    let (_, software) = plurx_core::resource_settings::admission_pair(shared.store.as_ref())
        .await
        .map_err(|error| format!("reading finite software pool policy: {error}"))?;
    let software_pool_threads = software
        .as_deref()
        .map(str::trim)
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| "finite software pool policy is invalid".to_owned())?
        .unwrap_or_else(|| crate::admission::software_budget() as u64);
    let observed_runtime = Arc::clone(runtime);
    let epoch = tokio::task::spawn_blocking(move || observed_runtime.observe_admission_capacity())
        .await
        .map_err(|error| format!("finite admission observer failed: {error}"))?
        .map_err(|error| format!("observing finite admission: {error:?}"))?;
    let capacity = runtime
        .accounting()
        .map_err(|error| format!("reading finite admission: {error:?}"))?
        .capacity;
    let request = finite_profile::compile(
        shared,
        key,
        &recipe,
        &plan,
        epoch,
        capacity,
        software_pool_threads,
    )?;
    // No selected source/root/marker has been opened. Core synchronously
    // reserves the complete role and retains its prepare operation before return.
    let owner = runtime
        .prepare_software_vod(request, Instant::now() + settings.materialize_budget)
        .map_err(|error| {
            if let Some(refusal) = error.admission_refusal() {
                crate::transcode::vod_refusal_error(
                    "vod_family_capacity",
                    &format!("finite software admission refused: {refusal:?}"),
                )
            } else {
                format!("preparing finite software VOD: {error}")
            }
        })?;
    let dir = RenditionDir::from_finite_producer(owner);
    let manifest = Manifest::new(plan.clone());
    Ok(Shared::construct_rendition(
        key,
        dir,
        recipe,
        None,
        plan,
        index,
        settings,
        None,
        false,
        None,
        manifest,
        IdentityState::default(),
    ))
}

pub(super) async fn run(
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    producer: Arc<FiniteSoftwareProducer>,
) {
    // The same registered per-rendition driver waits for demand; it does not
    // create a detached retry loop or one native producer per response.
    if !rendition.closed.load(Acquire) {
        rendition.wake.notified().await;
    }
    let mut owner = producer.owner.lock().await;
    if rendition.closed.load(Acquire) {
        if let Err(error) = producer
            .cancel_owned(&owner, Instant::now() + rendition.materialize_budget)
            .await
        {
            tracing::warn!(target: "plurxd::vodserve", rendition=%rendition.key, %error, "finite preparation remains held during cancellation");
        }
        return;
    }
    producer.stage.store(PREPARING, Release);
    let Some(mut events) = owner.take_events() else {
        return;
    };
    // Start publishes only the start decision, never completion. Its concrete
    // writer owns a bounded event channel, drained before joining that writer.
    let start_result = owner
        .start(Instant::now() + rendition.materialize_budget)
        .await;
    if let Err(error) = start_result {
        fail(
            &shared,
            &rendition,
            &producer,
            format!("starting finite software VOD: {error}"),
        );
        let _ = owner.stop_producer().await;
        if let Err(error) = producer
            .cancel_owned(&owner, Instant::now() + rendition.materialize_budget)
            .await
        {
            tracing::warn!(target: "plurxd::vodserve", rendition=%rendition.key, %error, "finite failed start remains charged");
        }
        return;
    }
    producer.stage.store(RUNNING, Release);
    let completion = {
        let joined = owner.join_producer();
        tokio::pin!(joined);
        let mut channel_open = true;
        loop {
            tokio::select! {
                completion = &mut joined => break completion,
                event = events.recv(), if channel_open => match event {
                    Some(event) => if let Err(error) = observe(&shared, &rendition, event).await {
                        fail(&shared, &rendition, &producer, format!("indexing finite output: {error}"));
                    },
                    None => channel_open = false,
                },
                _ = rendition.wake.notified() => {
                    if rendition.closed.load(Acquire) { let _ = producer.request_stop(); }
                },
            }
        }
    };
    // The writer can finish before the final metadata event wins select.
    while let Ok(event) = events.try_recv() {
        if let Err(error) = observe(&shared, &rendition, event).await {
            fail(
                &shared,
                &rendition,
                &producer,
                format!("indexing finite output: {error}"),
            );
        }
    }
    let success = completion.as_ref().is_ok_and(|completion| {
        completion
            .root_status
            .is_some_and(|status| status.success())
            && completion.writer.is_some_and(|writer| {
                writer.outcome == FiniteWriterOutcome::Ran && writer.completed_output
            })
    });
    if !success || rendition.failure().is_some() {
        fail(
            &shared,
            &rendition,
            &producer,
            format!("finite producer did not complete: {completion:?}"),
        );
        if let Err(error) = producer
            .cancel_owned(&owner, Instant::now() + rendition.materialize_budget)
            .await
        {
            tracing::warn!(target: "plurxd::vodserve", rendition=%rendition.key, %error, "finite failed producer remains charged");
        }
        return;
    }
    producer.stage.store(RETIRING, Release);
    let deadline = Instant::now() + rendition.materialize_budget;
    let retired = async {
        owner.freeze_output(deadline).await?;
        owner.retire_output(deadline).await
    }
    .await;
    match retired {
        Ok(_) => {
            producer.stage.store(RETAINED, Release);
            let mut manifest = rendition.manifest.lock().await;
            shared.try_admit(&rendition, &mut manifest).await;
            rendition.kick();
        }
        Err(error) => {
            // Exact runtime operation and full R survive a lost/unknown result.
            record_failure(
                &shared,
                &rendition,
                crate::playback_control::ProducerDecisionReason::ProducerWriteFailed,
                format!("finite retirement remains held: {error}"),
            );
        }
    }
}

fn fail(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    producer: &FiniteSoftwareProducer,
    cause: String,
) {
    producer.stage.store(FAILED, Release);
    let _ = producer.request_stop();
    record_failure(
        shared,
        rendition,
        crate::playback_control::ProducerDecisionReason::ProducerWriteFailed,
        cause,
    );
}

async fn observe(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    event: SoftwareVodEvent,
) -> io::Result<()> {
    match event {
        SoftwareVodEvent::Initialized(identity) => {
            rendition.identity.lock().await.identity = Some(identity);
            rendition.init_notify.notify_waiters();
        }
        SoftwareVodEvent::Materialized {
            entry,
            bytes,
            digest,
        } => {
            let init = rendition.identity.lock().await.identity.clone();
            let mut manifest = rendition.manifest.lock().await;
            if manifest
                .state(entry)
                .is_none_or(|state| state.is_materialized())
            {
                return Err(io::Error::other(
                    "finite event is outside the immutable plan or duplicated",
                ));
            }
            if !manifest.materialize(entry, bytes, now_ms()) {
                return Err(io::Error::other("finite plan entry absent"));
            }
            let epoch = rendition.gen_epoch.load(Acquire);
            let publication = credit_marker_prewarm_publication(rendition, epoch, entry).await;
            if let Some(init) = init.as_ref() {
                rendition
                    .output_measurement
                    .lock()
                    .expect("output measurement lock")
                    .observe_finite(
                        rendition,
                        init,
                        epoch,
                        entry,
                        output_measurement::ObservedOutputMember {
                            bytes,
                            digest,
                            publication,
                        },
                    );
            }
            if let Some(producer) = rendition.dir.finite_producer() {
                producer
                    .produced_through
                    .store(u64::from(entry) + 1, Release);
            }
            shared.working_set.fetch_add(bytes, Relaxed);
            rendition.clear_demand(entry);
            shared.pool.satisfy(&rendition.key, entry);
        }
    }
    rendition.kick();
    Ok(())
}
