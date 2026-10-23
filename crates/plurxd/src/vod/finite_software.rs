//! Ordinary software VOD owns one concrete finite producer through retirement.
//! Native resources remain inside core; events follow durable publication and
//! the daemon manifest is only a serving index, never settlement authority.
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteClientStateLease, FiniteFile, FiniteMetadataLease, FiniteWriterOutcome,
    SoftwareVodControl, SoftwareVodEvent, SoftwareVodOwner,
};
use plurx_core::transcode::{DecodeBackend, Encoder};
use std::sync::atomic::AtomicU8;

const PREPARED: u8 = 0;
const RUNNING: u8 = 1;
const RETIRING: u8 = 2;
const RETAINED: u8 = 3;
const FAILED: u8 = 4;
const PREPARING: u8 = 6;
const DRIVER_UNREGISTERED: u8 = 0;
const DRIVER_REGISTERED: u8 = 1;
const DRIVER_RETURNED: u8 = 2;
const DRIVER_ABORTED: u8 = 3;
const IDENTITY_METADATA_BYTES: u64 = 64 * 1024;
const ENCODED_MARKER_BYTES: u64 = 4 * 1024;

/// Persistent application owner. Stage is diagnostic/control serialization;
/// only core's original receipts and known journal commits release resources.
pub(crate) struct FiniteSoftwareProducer {
    owner: Mutex<SoftwareVodOwner>,
    // Captured before publication. Source-spec paths are only removed and
    // source Vec capacities never grow; no live owner lock is needed while
    // the original driver holds it through production and retirement.
    owner_heap_bound: Option<usize>,
    control: SoftwareVodControl,
    stage: AtomicU8,
    produced_through: AtomicU64,
    suspended: AtomicBool,
    driver_state: AtomicU8,
    driver_change: Notify,
}

/// Registered before spawn. A dropped task ends daemon observations but is
/// distinct from a successful return and never proves native closure.
pub(super) struct FiniteDriverCompletion {
    producer: Arc<FiniteSoftwareProducer>,
    returned: bool,
}

impl FiniteDriverCompletion {
    fn returned(&mut self) {
        self.returned = true;
        self.producer.driver_state.store(DRIVER_RETURNED, Release);
        self.producer.driver_change.notify_waiters();
    }
}

impl Drop for FiniteDriverCompletion {
    fn drop(&mut self) {
        if !self.returned {
            self.producer.driver_state.store(DRIVER_ABORTED, Release);
            self.producer.driver_change.notify_waiters();
        }
    }
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
            owner_heap_bound: owner.requested_heap_bytes(),
            owner: Mutex::new(owner),
            stage: AtomicU8::new(PREPARED),
            produced_through: AtomicU64::new(0),
            suspended: AtomicBool::new(false),
            driver_state: AtomicU8::new(DRIVER_UNREGISTERED),
            driver_change: Notify::new(),
        }
    }
    /// Dynamic application heap retained by the one Arc producer. Its owner
    /// helper excludes the path allocation shared with FiniteOutput; the
    /// directory counts that allocation exactly once.
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        Self::projected_heap_bytes(self.owner_heap_bound?)
    }

    pub(super) fn projected_heap_bytes(owner_heap: usize) -> Option<usize> {
        std::mem::size_of::<Self>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(owner_heap)
    }

    pub(super) fn begin_driver(self: &Arc<Self>) -> FiniteDriverCompletion {
        assert_eq!(
            self.driver_state.compare_exchange(
                DRIVER_UNREGISTERED,
                DRIVER_REGISTERED,
                AcqRel,
                Acquire,
            ),
            Ok(DRIVER_UNREGISTERED),
            "finite producer has exactly one registered driver"
        );
        FiniteDriverCompletion {
            producer: Arc::clone(self),
            returned: false,
        }
    }

    pub(super) fn driver_never_registered(&self) -> bool {
        self.driver_state.load(Acquire) == DRIVER_UNREGISTERED
    }

    /// Wait only for daemon event production to end. Native root/task closure
    /// is established separately by the original core owner operation.
    pub(super) async fn wait_driver_quiescent(&self, deadline: Instant) -> io::Result<bool> {
        loop {
            let changed = self.driver_change.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.driver_state.load(Acquire) {
                DRIVER_RETURNED => return Ok(true),
                DRIVER_ABORTED => return Ok(false),
                DRIVER_REGISTERED => {}
                _ => {
                    return Err(io::Error::other(
                        "finite producer driver was never registered",
                    ))
                }
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "finite producer driver remains active",
                    )
                })?;
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
                    RETAINED => {}
                    RETIRING => {
                        owner
                            .retire_output(deadline)
                            .await
                            .map_err(io::Error::other)?;
                        self.stage.store(RETAINED, Release);
                    }
                    _ => {
                        // A terminal finite role needs the detached closing
                        // ticket after this application owner has dropped.
                        // The shared closing owner performs that transaction.
                        return Err(io::Error::other(
                            "finite cancellation requires its closing owner",
                        ));
                    }
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
    /// A completed S cannot start another publication for a missing file.
    /// The live R stages still allow ordinary materialization waits.
    pub(crate) fn retained_without_producer(&self) -> bool {
        self.stage.load(Acquire) == RETAINED
    }
    pub(crate) async fn prepare_terminate(
        &self,
        deadline: Instant,
    ) -> io::Result<plurx_core::adaptive_resources::runtime::finite_vod::FiniteCloseTicket> {
        self.control.request_stop()?;
        let owner = self.owner.lock().await;
        if self.stage.load(Acquire) == RETIRING {
            owner
                .retire_output(deadline)
                .await
                .map_err(io::Error::other)?;
            self.stage.store(RETAINED, Release);
        }
        if self.stage.load(Acquire) == RETAINED {
            owner
                .output()
                .prepare_eviction(deadline)
                .await
                .map_err(io::Error::other)
        } else {
            owner
                .prepare_cancel(deadline)
                .await
                .map_err(io::Error::other)
        }
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

fn attach_metadata_partitions(
    mut rendition: Arc<Rendition>,
    metadata: FiniteMetadataLease,
    client: FiniteClientStateLease,
) -> Result<Arc<Rendition>, (String, Arc<Rendition>)> {
    let checked = (|| {
        let value = Arc::get_mut(&mut rendition)
            .ok_or("finite rendition was shared before metadata accounting")?;
        // Install both sole top leases before allocating either client table.
        // They follow the tables in Rendition's field order, so aliases retain
        // their charge through the final owner Drop.
        value.finite_metadata_lease = Some(metadata);
        value.finite_client_lease = Some(client);
        let client = value
            .finite_client_lease
            .as_ref()
            .ok_or("finite client-state lease disappeared")?;
        let (readers, demands) = super::client_state::finite_tables(value.plan.len(), client)
            .map_err(|error| format!("finite client tables exceed their partition: {error}"))?;
        *value.readers.get_mut() = readers;
        *value.demand_since.get_mut().expect("finite demand lock") = demands;
        let actual = value
            .requested_retained_metadata_bytes()
            .ok_or("finite rendition metadata capacity could not be measured")?;
        value
            .finite_metadata_lease
            .as_ref()
            .ok_or("finite rendition metadata lease disappeared")?
            .validate_actual(actual)
            .map_err(|error| {
                format!("finite rendition metadata exceeded its reservation: {error}")
            })?;
        Ok::<(), String>(())
    })();
    match checked {
        Ok(()) => Ok(rendition),
        Err(error) => Err((error, rendition)),
    }
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
    let output_path = shared.base.join(key);
    let projected_metadata = finite_metadata::projected_requested_heap_bytes(
        &recipe,
        index.as_ref(),
        &plan,
        key,
        &output_path,
    )
    .ok_or("finite rendition metadata projection overflowed or is unsupported")?;
    let object_version = finite_profile::source_version(&recipe)?;
    let segment_count = u32::try_from(plan.len())
        .map_err(|_| "finite software segment count overflows".to_owned())?;
    let media_file_id = recipe.file.id.to_string();
    let recovered = runtime
        .recovered_software_output(key, &media_file_id, &object_version, segment_count)
        .await
        .map_err(|error| format!("recovered finite software output is unresolved: {error}"))?;
    if let Some(output) = recovered {
        let projected_metadata = projected_metadata
            .checked_add(
                output
                    .requested_heap_bytes()
                    .ok_or("recovered finite output metadata overflowed")?,
            )
            .and_then(|bytes| {
                bytes.checked_add(finite_metadata::closing_owner_heap_bytes(key.len())?)
            })
            .ok_or("recovered finite metadata projection overflowed")?;
        let (metadata_lease, client_lease) = output
            .reserve_metadata_partitions(
                projected_metadata,
                finite_profile::CLIENT_STATE_BYTES as usize,
            )
            .await
            .map_err(|error| format!("reserving recovered finite rendition metadata: {error}"))?;
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
        let rendition = Shared::construct_rendition(
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
        );
        // A failed recovered fit leaves the original protected S unchanged;
        // this daemon metadata owner and its lease simply fall away.
        return attach_metadata_partitions(rendition, metadata_lease, client_lease)
            .map_err(|(error, _rendition)| error);
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
    let accounting = runtime
        .accounting()
        .map_err(|error| format!("reading finite admission: {error:?}"))?;
    // Retained/shared execution domains keep their own commitments. Plan
    // encoder work against the unspent hard CPU budget; reservation rechecks
    // this under the ledger lock, so concurrent preparation cannot overspend
    // the same headroom. This is not an idle-CPU or history prediction.
    let mut capacity = accounting.capacity;
    capacity.cpu_ns_per_second = match capacity.cpu_ns_per_second {
        plurx_core::process::observation::Metric::Known(cpu) => {
            plurx_core::process::observation::Metric::Known(
                cpu.saturating_sub(accounting.used.cpu_ns_per_second),
            )
        }
        unknown => unknown,
    };
    if matches!(
        capacity.cpu_ns_per_second,
        plurx_core::process::observation::Metric::Known(0)
    ) {
        return Err(crate::transcode::vod_refusal_error(
            "vod_family_capacity",
            "finite software admission refused: no unspent hard CPU budget",
        ));
    }
    let request = finite_profile::compile(
        shared,
        key,
        &recipe,
        index.as_ref(),
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
    let output = owner.output();
    // The original producer reservation is already charged. Measure its
    // retained application allocations before reserving metadata or building
    // any daemon owner; the actual post-build fit uses these same objects.
    let projection = (|| {
        projected_metadata
            .checked_add(output.requested_heap_bytes()?)?
            .checked_add(FiniteSoftwareProducer::projected_heap_bytes(
                owner.requested_heap_bytes()?,
            )?)?
            .checked_add(finite_metadata::closing_owner_heap_bytes(key.len())?)
    })();
    let Some(projected_metadata) = projection else {
        // Preparation already owns a charged journal operation. Even a
        // metadata-fit refusal must retain that exact owner for cleanup.
        shared
            .retain_unattached_finite(
                key,
                None,
                output,
                Arc::new(FiniteSoftwareProducer::new(owner)),
            )
            .await;
        return Err("finite owner metadata projection overflowed".into());
    };
    let (metadata_lease, client_lease) = match output
        .reserve_metadata_partitions(
            projected_metadata,
            finite_profile::CLIENT_STATE_BYTES as usize,
        )
        .await
    {
        Ok(leases) => leases,
        Err(error) => {
            shared
                .retain_unattached_finite(
                    key,
                    None,
                    output,
                    Arc::new(FiniteSoftwareProducer::new(owner)),
                )
                .await;
            return Err(format!("reserving finite rendition metadata: {error}"));
        }
    };
    let dir = RenditionDir::from_finite_producer(owner);
    let manifest = Manifest::new(plan.clone());
    let rendition = Shared::construct_rendition(
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
    );
    match attach_metadata_partitions(rendition, metadata_lease, client_lease) {
        Ok(rendition) => Ok(rendition),
        Err((error, rendition)) => {
            let producer = rendition
                .dir
                .finite_producer()
                .expect("fresh finite directory retains its original producer");
            shared
                .retain_unattached_finite(key, Some(rendition), output, producer)
                .await;
            Err(error)
        }
    }
}

pub(super) async fn run(
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    producer: Arc<FiniteSoftwareProducer>,
    mut completion: FiniteDriverCompletion,
) {
    run_body(shared, rendition, producer).await;
    completion.returned();
}

async fn run_body(
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
        // This driver still owns the rendition's metadata lease. Final
        // cancellation waits for that lease, so only the existing compact
        // cleanup owner may settle AFTER this driver returns and drops it.
        let _ = producer.request_stop();
        return;
    }
    producer.stage.store(PREPARING, Release);
    note_resource_phase(
        &rendition,
        plurx_core::resource_observation::WorkPhase::Preparing,
    );
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
        let _ = producer.request_stop();
        // A partial start may already have a writer blocked on this original
        // event channel. Drain it while joining; awaiting stop/join alone here
        // would strand the writer behind its still-live receiver.
        let _ = join_and_observe(&shared, &rendition, &producer, &owner, &mut events).await;
        return;
    }
    producer.stage.store(RUNNING, Release);
    let completion = join_and_observe(&shared, &rendition, &producer, &owner, &mut events).await;
    // The original join drained the terminal notifications. Destroy the
    // receiver before S so completed producer event-channel allocations do
    // not survive the return of producer-only memory.
    drop(events);
    let success = completion.as_ref().is_ok_and(|completion| {
        completion
            .root_status
            .is_some_and(|status| status.success())
            && completion.writer.is_some_and(|writer| {
                writer.outcome == FiniteWriterOutcome::Ran && writer.completed_output
            })
    });
    if success && rendition.failure().is_none() {
        if let Err(error) = rendition.validate_retained_metadata().await {
            fail(
                &shared,
                &rendition,
                &producer,
                format!("finite retained metadata does not fit: {error}"),
            );
        }
    }
    if !success || rendition.failure().is_some() {
        fail(
            &shared,
            &rendition,
            &producer,
            format!("finite producer did not complete: {completion:?}"),
        );
        // The original root/writer have joined above. Their physical closure
        // remains in core; leave settlement to the existing cleanup owner once
        // this driver and the remaining client metadata actually drop.
        return;
    }
    // A completed output has no missing entry to rematerialize. Abort every
    // surviving clock before S; its original task slot remains charged until
    // Tokio actually drops the cancelled future.
    rendition.clear_all_demands();
    producer.stage.store(RETIRING, Release);
    note_resource_phase(
        &rendition,
        plurx_core::resource_observation::WorkPhase::Retiring,
    );
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

async fn join_and_observe(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    producer: &FiniteSoftwareProducer,
    owner: &SoftwareVodOwner,
    events: &mut tokio::sync::mpsc::Receiver<SoftwareVodEvent>,
) -> Result<
    plurx_core::adaptive_resources::runtime::finite_vod::FiniteProducerCompletion,
    plurx_core::adaptive_resources::runtime::finite_vod::FiniteVodError,
> {
    let completion = {
        let joined = owner.join_producer();
        tokio::pin!(joined);
        let mut channel_open = true;
        loop {
            tokio::select! {
                completion = &mut joined => break completion,
                event = events.recv(), if channel_open => match event {
                    Some(event) => if let Err(error) = observe(shared, rendition, event).await {
                        fail(shared, rendition, producer, format!("indexing finite output: {error}"));
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
        if let Err(error) = observe(shared, rendition, event).await {
            fail(
                shared,
                rendition,
                producer,
                format!("indexing finite output: {error}"),
            );
        }
    }
    completion
}

fn fail(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    producer: &FiniteSoftwareProducer,
    cause: String,
) {
    producer.stage.store(FAILED, Release);
    note_resource_phase(
        rendition,
        plurx_core::resource_observation::WorkPhase::Retiring,
    );
    let _ = producer.request_stop();
    record_failure(
        shared,
        rendition,
        crate::playback_control::ProducerDecisionReason::ProducerWriteFailed,
        cause,
    );
}

fn note_resource_phase(rendition: &Rendition, phase: plurx_core::resource_observation::WorkPhase) {
    if let Some(encoding) = &rendition.recipe.encoding {
        encoding.note_resource_phase(rendition.gen_epoch.load(Acquire), phase);
    }
}

async fn observe(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    event: SoftwareVodEvent,
) -> io::Result<()> {
    match event {
        SoftwareVodEvent::Initialized(identity) => {
            rendition.identity.lock().await.identity = Some(identity);
            rendition.clear_demand(INIT_DEMAND_INDEX);
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
                if let (Some(encoding), Some(planned)) =
                    (&rendition.recipe.encoding, rendition.plan.entry(entry))
                {
                    // Only this fresh durably published event advances useful
                    // output. Retained/cache reads never enter this window.
                    // The first output establishes the producing baseline,
                    // excluding startup; a held producer is not spare capacity.
                    let phase = if producer.suspended.load(Acquire) {
                        plurx_core::resource_observation::WorkPhase::BufferPaced
                    } else {
                        plurx_core::resource_observation::WorkPhase::Producing
                    };
                    encoding.note_finite_resource_segment(
                        epoch,
                        entry,
                        ticks_to_ms(planned.end_ticks(), rendition.timescale),
                        phase,
                    );
                }
            }
            shared.working_set.fetch_add(bytes, Relaxed);
            rendition.clear_demand(entry);
            shared.pool.satisfy(&rendition.key, entry);
            drop(manifest);
        }
    }
    rendition.kick();
    Ok(())
}
