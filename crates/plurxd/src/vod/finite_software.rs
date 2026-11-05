//! Ordinary software VOD owns one concrete finite producer through retirement.
//! Native resources remain inside core; events follow durable publication and
//! the daemon manifest is only a serving index, never settlement authority.
pub(crate) use super::shared::FiniteBuildCleanup;
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteApplicationLease, FiniteClientStateLease, FiniteFile, FiniteMetadataLease, FiniteOutput,
    FiniteWriterOutcome, SoftwareVodControl, SoftwareVodEvent, SoftwareVodOwner,
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

fn speculative_capacity_refusal(refusal: plurx_core::adaptive_resources::AdmissionRefusal) -> bool {
    use plurx_core::adaptive_resources::AdmissionRefusal;
    matches!(
        refusal,
        AdmissionRefusal::InsufficientEvidence
            | AdmissionRefusal::HardLimit(_)
            | AdmissionRefusal::TrialAlreadyHeld
            | AdmissionRefusal::LedgerFull
    )
}

/// Persistent application owner. Stage is diagnostic/control serialization;
/// only core's original receipts and known journal commits release resources.
pub(crate) struct FiniteSoftwareProducer {
    owner: Mutex<SoftwareVodOwner>,
    generation_epoch: u64,
    cache_grant: std::sync::Mutex<Option<crate::playback_cache::FiniteCacheReservation>>,
    compatibility: std::sync::Mutex<Option<crate::vodencode::FiniteSoftwareCompatibility>>,
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
    // Registration precedes Store Prepared. Publication authorizes ordinary
    // entry separately and transfers the exact original task handle here.
    driver_activated: AtomicBool,
    installed_driver: Mutex<OriginalDriver>,
    collector_slot: std::sync::Mutex<
        Option<plurx_core::adaptive_resources::runtime::ordinary_finite_collector::OrdinarySlot>,
    >,
    #[cfg(all(test, feature = "cluster-integration-tests"))]
    panic_driver_after_return: AtomicBool,
}

/// The successful callback is separate from actual task/F destruction. Keep
/// a consumed failed handle so no later waiter can reinterpret absence as join.
enum OriginalDriver {
    NeverInstalled,
    Pending(tokio::task::JoinHandle<()>),
    Joined(tokio::task::Id),
    Failed(tokio::task::JoinHandle<()>),
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
        #[cfg(all(test, feature = "cluster-integration-tests"))]
        if self.returned && self.producer.panic_driver_after_return.load(Acquire) {
            panic!("original finite driver destructor after returned callback");
        }
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
    pub(crate) fn new(
        owner: SoftwareVodOwner,
        cache_grant: crate::playback_cache::FiniteCacheReservation,
        compatibility: crate::vodencode::FiniteSoftwareCompatibility,
    ) -> Self {
        Self {
            control: owner.control(),
            generation_epoch: owner.generation_epoch(),
            owner_heap_bound: owner.requested_heap_bytes(),
            owner: Mutex::new(owner),
            cache_grant: std::sync::Mutex::new(Some(cache_grant)),
            compatibility: std::sync::Mutex::new(Some(compatibility)),
            stage: AtomicU8::new(PREPARED),
            produced_through: AtomicU64::new(0),
            suspended: AtomicBool::new(false),
            driver_state: AtomicU8::new(DRIVER_UNREGISTERED),
            driver_change: Notify::new(),
            driver_activated: AtomicBool::new(false),
            installed_driver: Mutex::new(OriginalDriver::NeverInstalled),
            collector_slot: std::sync::Mutex::new(None),
            #[cfg(all(test, feature = "cluster-integration-tests"))]
            panic_driver_after_return: AtomicBool::new(false),
        }
    }
    pub(super) fn bind_collector_slot(
        &self,
        slot: Option<
            plurx_core::adaptive_resources::runtime::ordinary_finite_collector::OrdinarySlot,
        >,
    ) {
        *self.collector_slot.lock().expect("original collector slot") = slot;
    }
    pub(super) fn collector_slot(
        &self,
    ) -> Option<plurx_core::adaptive_resources::runtime::ordinary_finite_collector::OrdinarySlot>
    {
        self.collector_slot
            .lock()
            .expect("original collector slot")
            .clone()
    }
    pub(super) fn original_driver_finished(&self) -> bool {
        let Ok(driver) = self.installed_driver.try_lock() else {
            return false;
        };
        match &*driver {
            OriginalDriver::NeverInstalled => false,
            OriginalDriver::Pending(task) | OriginalDriver::Failed(task) => task.is_finished(),
            OriginalDriver::Joined(_) => true,
        }
    }
    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn original_driver_id_now_for_test(&self) -> Option<tokio::task::Id> {
        let driver = self
            .installed_driver
            .try_lock()
            .expect("original dormant driver custody");
        match &*driver {
            OriginalDriver::NeverInstalled => None,
            OriginalDriver::Pending(task) | OriginalDriver::Failed(task) => Some(task.id()),
            OriginalDriver::Joined(id) => Some(*id),
        }
    }
    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn abort_original_driver_now_for_test(&self) {
        let driver = self
            .installed_driver
            .try_lock()
            .expect("original dormant driver custody");
        if let OriginalDriver::Pending(task) = &*driver {
            task.abort();
        }
    }
    pub(crate) fn generation_epoch(&self) -> u64 {
        self.generation_epoch
    }
    pub(super) fn take_cache_grant(&self) -> Option<crate::playback_cache::FiniteCacheReservation> {
        self.cache_grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub(super) fn take_compatibility(
        &self,
    ) -> Option<crate::vodencode::FiniteSoftwareCompatibility> {
        self.compatibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    fn compatibility_reservation(&self) -> Option<crate::vodencode::EncodePermit> {
        self.compatibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(crate::vodencode::FiniteSoftwareCompatibility::reservation)
    }
    fn commit_retained_cache(&self) {
        if let Some(compatibility) = self.take_compatibility() {
            compatibility.known_retired();
        }
        if let Some(grant) = self.take_cache_grant() {
            grant.retained();
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
            .checked_add(owner_heap)?
            .checked_add(crate::vodencode::FiniteSoftwareCompatibility::requested_heap_bytes()?)
    }

    /// Resolve and observe the original media and held executable before a
    /// Store Prepared result. This does not enter the output root or launch a
    /// producer; a later winner starts the same already observed cells.
    pub(crate) async fn prepare_adaptive_inputs(
        &self,
        deadline: Instant,
    ) -> Result<[u8; 32], String> {
        let mut owner = self.owner.lock().await;
        owner
            .prepare_inputs(deadline)
            .await
            .map_err(|error| format!("opening finite attempt inputs: {error}"))?;
        owner
            .prepared_handle_digest()
            .map_err(|error| format!("binding finite attempt owner: {error}"))
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

    pub(super) fn activate_driver(&self) {
        self.driver_activated.store(true, Release);
    }

    /// Retain the original handle before either publication or abandonment.
    /// This does not authorize native entry or create a synthetic demand wake.
    pub(super) fn retain_original_driver(&self, driver: tokio::task::JoinHandle<()>) {
        let mut installed = self
            .installed_driver
            .try_lock()
            .expect("original driver installation has no concurrent join");
        assert!(
            matches!(*installed, OriginalDriver::NeverInstalled),
            "original finite driver is retained once"
        );
        *installed = OriginalDriver::Pending(driver);
    }

    pub(super) fn install_original_driver(&self, driver: tokio::task::JoinHandle<()>) {
        self.retain_original_driver(driver);
        self.activate_driver();
    }

    /// Keep the exact handle in its owner while awaiting it. Cancelling a
    /// cleanup waiter releases only this lock, never the registered task.
    /// A callback can precede a destructor panic; only successful original
    /// task join produces Joined, and every failed join remains failed.
    pub(super) async fn join_installed_driver(&self) -> io::Result<()> {
        let mut installed = self.installed_driver.lock().await;
        match &mut *installed {
            OriginalDriver::NeverInstalled if self.driver_never_registered() => Ok(()),
            OriginalDriver::NeverInstalled => Err(io::Error::other(
                "registered original finite driver has not been retained",
            )),
            OriginalDriver::Joined(original_id) => {
                tracing::trace!(driver = %original_id, "original finite driver already joined");
                Ok(())
            }
            OriginalDriver::Failed(original) => Err(io::Error::other(format!(
                "original finite driver {} join previously failed",
                original.id()
            ))),
            OriginalDriver::Pending(driver) => {
                let original_id = driver.id();
                match driver.await {
                    Ok(()) => {
                        *installed = OriginalDriver::Joined(original_id);
                        Ok(())
                    }
                    Err(error) => {
                        let OriginalDriver::Pending(original) =
                            std::mem::replace(&mut *installed, OriginalDriver::NeverInstalled)
                        else {
                            unreachable!("the same original driver remains under its join lock")
                        };
                        *installed = OriginalDriver::Failed(original);
                        Err(io::Error::other(error))
                    }
                }
            }
        }
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) async fn installed_driver_id_for_test(&self) -> Option<tokio::task::Id> {
        match &*self.installed_driver.lock().await {
            OriginalDriver::Pending(driver) | OriginalDriver::Failed(driver) => Some(driver.id()),
            OriginalDriver::NeverInstalled | OriginalDriver::Joined(_) => None,
        }
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) async fn original_driver_joined_for_test(
        &self,
        original_id: tokio::task::Id,
    ) -> bool {
        matches!(*self.installed_driver.lock().await,
            OriginalDriver::Joined(joined_id) if joined_id == original_id)
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn panic_original_driver_after_return_for_test(&self) {
        self.panic_driver_after_return.store(true, Release);
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) async fn hold_original_owner_for_test(
        &self,
    ) -> tokio::sync::MutexGuard<'_, SoftwareVodOwner> {
        self.owner.lock().await
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn driver_dormant_for_test(&self) -> bool {
        self.driver_state.load(Acquire) == DRIVER_REGISTERED
            && !self.driver_activated.load(Acquire)
            && self.stage.load(Acquire) == PREPARED
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn driver_waiting_for_demand_for_test(&self) -> bool {
        self.driver_state.load(Acquire) == DRIVER_REGISTERED && self.stage.load(Acquire) == PREPARED
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
                    ));
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
                        self.commit_retained_cache();
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
    rendition: Arc<Rendition>,
    metadata: FiniteMetadataLease,
    client: FiniteClientStateLease,
) -> Result<Arc<Rendition>, (String, Arc<Rendition>)> {
    attach_metadata_partitions_for(rendition, metadata, client, false)
}

fn attach_metadata_partitions_for(
    mut rendition: Arc<Rendition>,
    metadata: FiniteMetadataLease,
    client: FiniteClientStateLease,
    collected_ordinary: bool,
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
            .ok_or("finite rendition metadata capacity could not be measured")?
            .checked_add(projected_driver_future_bytes_for(collected_ordinary))
            .ok_or("finite driver frame capacity overflowed")?;
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

/// The selected path owns one `run` task and the existing closing driver.
/// Measure those concrete layouts, including the installed-task join, without
/// constructing a dummy owner or entering it.
/// Tokio's private task allocation/header and allocator rounding are separate
/// from this application-owned future frame.
pub(super) fn projected_driver_future_bytes() -> usize {
    projected_driver_future_bytes_for(false)
}

fn projected_driver_future_bytes_for(collected_ordinary: bool) -> usize {
    fn frame_bytes<F: std::future::Future<Output = ()>>(
        _: fn(
            Arc<Shared>,
            Arc<Rendition>,
            Arc<FiniteSoftwareProducer>,
            FiniteDriverCompletion,
        ) -> F,
    ) -> usize {
        std::mem::size_of::<F>()
    }
    type BuildFutureFactory<F> = fn(
        &'static Arc<Shared>,
        &'static str,
        Option<FragmentIndex>,
        Recipe,
        SegmentPlan,
        &'static VodSettings,
        Option<usize>,
        Option<plurx_core::cluster::clock::ContinuousDeadline>,
        &'static mut Option<super::shared::PreparedAttemptCleanup>,
        &'static mut Option<plurx_core::resource_policy::TrialRefusal>,
    ) -> F;
    fn build_frame_bytes<
        F: std::future::Future<
            Output = Result<(Arc<Rendition>, Option<FiniteApplicationLease>), String>,
        >,
    >(
        _: BuildFutureFactory<F>,
    ) -> usize {
        std::mem::size_of::<F>()
    }
    // The concrete builder already contains its metadata-hook/timeout child
    // frame, compatibility wait, finalized u32 and same-grant locals. Count
    // that actual layout once, beside the original driver future. Plain tuple
    // fields add no heap; private executor/timer/allocator peaks remain Unknown.
    frame_bytes(run)
        .checked_add(build_frame_bytes(build_inner))
        .and_then(|bytes| {
            bytes.checked_add(if collected_ordinary {
                0
            } else {
                super::shared::finite_closing_frame_bytes()
            })
        })
        .expect("compiler-projected finite frames fit usize")
}

async fn reserve_metadata_until(
    shared: &Shared,
    key: &str,
    output: &FiniteOutput,
    metadata_bytes: usize,
    client_bytes: usize,
    deadline: Instant,
) -> Result<(FiniteMetadataLease, FiniteClientStateLease), String> {
    tokio::time::timeout_at(deadline.into(), async {
        shared
            .hooks
            .get()
            .before_finite_metadata_reservation(key)
            .await;
        output
            .reserve_metadata_partitions(metadata_bytes, client_bytes)
            .await
    })
    .await
    .map_err(|_| "finite metadata reservation deadline elapsed".to_owned())?
    .map_err(|error| format!("reserving finite rendition metadata: {error}"))
}

pub(super) async fn build(
    shared: &Arc<Shared>,
    key: &str,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    plan: SegmentPlan,
    settings: &VodSettings,
) -> Result<Arc<Rendition>, String> {
    let mut ignored = None;
    let mut ignored_trial_refusal = None;
    build_inner(
        shared,
        key,
        index,
        recipe,
        plan,
        settings,
        None,
        None,
        &mut ignored,
        &mut ignored_trial_refusal,
    )
    .await
    .map(|(rendition, _)| rendition)
}

/// An adaptive attempt retains a separate precharged application owner until
/// its original worker entry and future have actually been destroyed.
#[expect(
    clippy::too_many_arguments,
    reason = "The adaptive attempt builder carries the original application bytes and remote cutoff beside the fixed recipe inputs."
)]
pub(super) async fn build_attempt(
    shared: &Arc<Shared>,
    key: &str,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    plan: SegmentPlan,
    settings: &VodSettings,
    application_owner_bytes: usize,
    continuous_entry_deadline: Option<plurx_core::cluster::clock::ContinuousDeadline>,
) -> Result<(Arc<Rendition>, FiniteApplicationLease), super::shared::AdaptiveBuildFailure> {
    let mut cleanup = None;
    let mut trial_refusal = None;
    let result = build_inner(
        shared,
        key,
        index,
        recipe,
        plan,
        settings,
        Some(application_owner_bytes),
        continuous_entry_deadline,
        &mut cleanup,
        &mut trial_refusal,
    )
    .await;
    let (rendition, lease) = result.map_err(|message| super::shared::AdaptiveBuildFailure {
        message,
        trial_refusal,
        cleanup,
    })?;
    Ok((
        rendition,
        lease.expect("attempt build takes its application lease"),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "The fixed recipe, immutable plan and original failed-attempt cleanup selector share one admitted construction frame without a new heap request."
)]
async fn build_inner(
    shared: &Arc<Shared>,
    key: &str,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    plan: SegmentPlan,
    settings: &VodSettings,
    application_owner_bytes: Option<usize>,
    continuous_entry_deadline: Option<plurx_core::cluster::clock::ContinuousDeadline>,
    failed_attempt_cleanup: &mut Option<super::shared::PreparedAttemptCleanup>,
    failed_attempt_trial_refusal: &mut Option<plurx_core::resource_policy::TrialRefusal>,
) -> Result<(Arc<Rendition>, Option<FiniteApplicationLease>), String> {
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
    if application_owner_bytes.is_some() && recovered.is_some() {
        return Err("adaptive attempt requires a fresh finite producer owner".into());
    }
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
        let (manifest, identity) = completed_output_state(
            &output,
            &recipe,
            &plan,
            segment_count,
            Instant::now() + settings.materialize_budget,
        )
        .await?;
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
            .map(|rendition| (rendition, None))
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
    let admission_deadline = Instant::now() + settings.materialize_budget;
    // Adaptive creates already reserved activation upstream. A fresh local
    // materialization budget must never extend their original policy cutoff.
    let original_policy_cutoff = continuous_entry_deadline
        .as_ref()
        .and_then(|cutoff| cutoff.monotonic_cutoff().ok())
        .unwrap_or_else(Instant::now);
    let epoch = runtime
        .refresh_admission_capacity(admission_deadline)
        .await
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
        if application_owner_bytes.is_some() {
            *failed_attempt_trial_refusal = Some(runtime.finite_unprofiled_trial_refusal(
                continuous_entry_deadline.as_ref(),
                original_policy_cutoff,
                Instant::now(),
            ));
        }
        return Err(crate::transcode::vod_refusal_error(
            "vod_family_capacity",
            "finite software admission refused: no unspent hard CPU budget",
        ));
    }
    let (mut request, finalized_encoder_threads) = finite_profile::compile(
        shared,
        key,
        &recipe,
        index.as_ref(),
        &plan,
        epoch,
        capacity,
        software_pool_threads,
    )?;
    if let Some(bytes) = application_owner_bytes {
        request
            .charge_application_owner_bytes(bytes)
            .map_err(|error| format!("charging adaptive attempt owner: {error}"))?;
    }
    request.continuous_entry_deadline = continuous_entry_deadline;
    // This aliases the same selected request for legacy compatibility only.
    // The core CPU demand above came from hard total capacity; do not subtract
    // this thread-pool claim again or add it as another physical core role.
    let compatibility = recipe
        .encoding
        .as_ref()
        .expect("selected finite encoding")
        .admit_finite_compatibility(
            software_pool_threads,
            finalized_encoder_threads,
            false,
            admission_deadline,
        )
        .await
        .ok_or_else(|| {
            if application_owner_bytes.is_some() {
                *failed_attempt_trial_refusal = Some(runtime.finite_trial_refusal(
                    &request,
                    original_policy_cutoff,
                    Instant::now(),
                ));
            }
            crate::transcode::vod_refusal_error(
                "vod_family_capacity",
                "finite software compatibility pool is occupied",
            )
        })?;
    // Reserve before the cleanup-map lock: global pressure calls back into
    // owners. No R/output IO exists while this borrowed await is pending.
    let cache_grant = tokio::time::timeout_at(
        admission_deadline.into(),
        crate::playback_cache::reserve_finite(
            &shared.base.join(&request.rendition_id),
            request.output.reserved_storage_bytes,
        ),
    )
    .await
    .map_err(|_| "finite operator cache quota deadline elapsed".to_owned())?
    .map_err(|error| format!("reserving finite operator cache quota: {error}"))?;
    // No selected source/root/marker has been opened. Core synchronously
    // reserves the complete role and retains its prepare operation before return.
    // Acquire the original cleanup slot before R acceptance. The synchronous
    // owner-to-directory install below cannot be cancelled between acceptance
    // and publication of that same owner in the existing closing registry.
    let build_slot = tokio::time::timeout_at(
        admission_deadline.into(),
        shared.reserve_finite_build_slot(key),
    )
    .await
    .map_err(|_| "finite build cleanup slot deadline elapsed".to_owned())??;
    let collected_ordinary =
        application_owner_bytes.is_none() && build_slot.collector_slot().is_some();
    if collected_ordinary {
        // Only ordinary cleanup really executes in the independently paid
        // service. Builder/producer-driver and adaptive allowances stay R/S.
        let moved = finite_metadata::closing_owner_heap_bytes(key.len())
            .and_then(|bytes| bytes.checked_add(super::shared::finite_closing_frame_bytes()))
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or("collected finite metadata partition overflowed")?;
        request.output.retained_metadata_bytes = request
            .output
            .retained_metadata_bytes
            .checked_sub(moved)
            .ok_or("collected finite metadata partition is inconsistent")?;
        let plurx_core::process::observation::Metric::Known(resources) =
            &mut request.demand.resources
        else {
            return Err("collected finite memory request is unknown".into());
        };
        resources.peak_memory_bytes = resources
            .peak_memory_bytes
            .checked_sub(moved)
            .ok_or("collected finite memory partition is inconsistent")?;
    }
    let cache_grant = crate::playback_cache::FiniteCacheReservation::enter(cache_grant);
    let compatibility = compatibility.enter();
    // This is only a refusal candidate. Successful conservative admission
    // below ignores it; policy cannot turn unknown peaks into a trial permit.
    let speculative_refusal = application_owner_bytes
        .map(|_| runtime.finite_trial_refusal(&request, original_policy_cutoff, Instant::now()));
    let prepared = if let Some(slot) = build_slot.collector_slot() {
        runtime.prepare_collected_software_vod(request, slot, admission_deadline)
    } else {
        runtime.prepare_software_vod(request, admission_deadline)
    };
    let owner = match prepared {
        Ok(owner) => owner,
        Err(error) => {
            if error.definitely_before_reservation() {
                cache_grant.known_closed();
                compatibility.known_retired();
                if error
                    .admission_refusal()
                    .is_some_and(speculative_capacity_refusal)
                {
                    *failed_attempt_trial_refusal = speculative_refusal;
                }
            }
            return Err(if let Some(refusal) = error.admission_refusal() {
                crate::transcode::vod_refusal_error(
                    "vod_family_capacity",
                    format!("finite software admission refused: {refusal:?}"),
                )
            } else {
                format!("preparing finite software VOD: {error}")
            });
        }
    };
    if let Some(hint) = owner.history_hint() {
        use plurx_core::resource_policy::{assess_finite_observed_service, FiniteObservedService};
        let observed = assess_finite_observed_service(
            hint.startup,
            hint.steady,
            hint.durable_units,
            admission_deadline.saturating_duration_since(Instant::now()),
        );
        match observed {
            FiniteObservedService::PriorAboveRemaining(duration) => {
                tracing::info!(target: "plurxd::vodserve", rendition = %key,
                    observed_service_ms = duration.as_millis(),
                    startup_recent = hint.startup_recent,
                    steady_recent = hint.steady_recent,
                    "exact-mix prior exceeds the remaining local materialization window; hard admission remains authoritative");
            }
            FiniteObservedService::PriorWithinRemaining(duration) => {
                tracing::debug!(target: "plurxd::vodserve", rendition = %key,
                    observed_service_ms = duration.as_millis(),
                    startup_recent = hint.startup_recent,
                    steady_recent = hint.steady_recent,
                    "exact-mix prior is advisory; startup and cleanup remain unbounded");
            }
            FiniteObservedService::Unknown => {}
        }
    }
    let application_lease = application_owner_bytes
        .map(|bytes| owner.take_application_lease(bytes))
        .transpose();
    let dir = build_slot.install(owner, cache_grant, compatibility);
    let output = dir
        .finite_output()
        .expect("installed original finite output");
    let producer = dir
        .finite_producer()
        .expect("installed original finite producer");
    let record_cleanup = |failed: &mut Option<super::shared::PreparedAttemptCleanup>| {
        if application_owner_bytes.is_some() {
            *failed = Some(
                dir.finite_build_cleanup_selector()
                    .expect("unpublished finite directory retains its original cleanup arm"),
            );
        }
    };
    let application_lease = match application_lease {
        Ok(lease) => lease,
        Err(error) => {
            record_cleanup(failed_attempt_cleanup);
            return Err(format!("taking adaptive attempt owner lease: {error}"));
        }
    };
    // These are the same allocations already projected before complete R
    // admission. Both the original owner and dormant close arm now survive a
    // cancelled metadata waiter in the map, without an async registration gap.
    let projection = (|| {
        projected_metadata
            .checked_add(output.requested_heap_bytes()?)?
            .checked_add(producer.requested_heap_bytes()?)?
            .checked_add(if collected_ordinary {
                0
            } else {
                finite_metadata::closing_owner_heap_bytes(key.len())?
            })
    })();
    let Some(projected_metadata) = projection else {
        record_cleanup(failed_attempt_cleanup);
        return Err("finite owner metadata projection overflowed".into());
    };
    let (metadata_lease, client_lease) = match reserve_metadata_until(
        shared,
        key,
        &output,
        projected_metadata,
        finite_profile::CLIENT_STATE_BYTES as usize,
        admission_deadline,
    )
    .await
    {
        Ok(leases) => leases,
        Err(error) => {
            record_cleanup(failed_attempt_cleanup);
            return Err(error);
        }
    };

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
    // Continuous/family attachment adopts this SAME entitlement, rather than
    // charging another compatibility permit. The producer retains its worker
    // claim so this alias cannot enter a second native generation.
    let compatibility_alias = producer
        .compatibility_reservation()
        .expect("fresh finite producer retains original compatibility claim");
    let same_compatibility = {
        let mut binding = rendition.retained_admission.lock_binding();
        binding.current().is_none()
            && binding
                .bind(compatibility_alias.clone())
                .same_reservation(&compatibility_alias)
    };
    if !same_compatibility {
        if application_owner_bytes.is_some() {
            *failed_attempt_cleanup = Some(
                rendition
                    .dir
                    .finite_build_cleanup_selector()
                    .expect("failed fresh finite binding retains original cleanup arm"),
            );
        }
        return Err("fresh finite compatibility entitlement is not the original claim".into());
    }
    match attach_metadata_partitions_for(
        rendition,
        metadata_lease,
        client_lease,
        collected_ordinary,
    ) {
        Ok(rendition) => Ok((rendition, application_lease)),
        Err((error, rendition)) => {
            if application_owner_bytes.is_some() {
                *failed_attempt_cleanup = Some(
                    rendition
                        .dir
                        .finite_build_cleanup_selector()
                        .expect("failed unpublished finite rendition retains its cleanup arm"),
                );
            }
            // Final directory Drop arms the already installed original owner.
            Err(error)
        }
    }
}

/// Read the complete immutable serving state of an existing finite S: its
/// identity, same-boot encoded process marker and every planned segment
/// length. This opens counted reads only; it never acquires or closes S.
async fn completed_output_state(
    output: &FiniteOutput,
    recipe: &Recipe,
    plan: &SegmentPlan,
    segment_count: u32,
    deadline: Instant,
) -> Result<(Manifest, InitIdentity), String> {
    let identity_len = output
        .file_len(FiniteFile::IdentityMetadata)
        .await
        .map_err(|error| format!("reading finite output identity length: {error}"))?
        .ok_or("finite output identity is missing")?;
    let mut identity_file = ReadyFile::Finite {
        output: output.clone(),
        file: FiniteFile::IdentityMetadata,
        len: identity_len,
    };
    let identity_bytes = identity_file
        .read_all_bounded(IDENTITY_METADATA_BYTES, deadline)
        .await
        .map_err(|error| format!("reading finite output identity: {error}"))?;
    let identity =
        init::decode_identity(&identity_bytes).ok_or("finite output identity is malformed")?;
    drop(identity_bytes);
    if !output.has_encoded_process_metadata() {
        return Err("finite output process marker is absent".into());
    }
    let marker_len = output
        .file_len(FiniteFile::EncodedProcessMetadata)
        .await
        .map_err(|error| format!("reading finite output marker length: {error}"))?
        .ok_or("finite output process marker is missing")?;
    let mut marker_file = ReadyFile::Finite {
        output: output.clone(),
        file: FiniteFile::EncodedProcessMetadata,
        len: marker_len,
    };
    let marker = marker_file
        .read_all_bounded(ENCODED_MARKER_BYTES, deadline)
        .await
        .map_err(|error| format!("reading finite output process marker: {error}"))?;
    let current = recipe
        .encoding
        .as_ref()
        .ok_or("finite output encoding is missing")?
        .engine
        .stable_software_digest()
        .ok_or("same-boot finite software engine identity unavailable")?;
    if std::str::from_utf8(&marker).ok().map(str::trim) != Some(current) {
        return Err("finite output output belongs to another boot or engine".into());
    }
    let mut manifest = Manifest::new(plan.clone());
    for entry in 0..segment_count {
        let len = output
            .file_len(FiniteFile::Segment(entry))
            .await
            .map_err(|error| format!("reading finite output segment {entry}: {error}"))?
            .ok_or_else(|| format!("finite output segment {entry} is missing"))?;
        if !manifest.materialize(entry, len, now_ms()) {
            return Err("finite output segment is outside the immutable plan".into());
        }
    }
    Ok((manifest, identity))
}

/// The serving rendition for a session that an exposed retained S captured
/// before any R was admitted. The retained registry alone owns that S: this
/// rendition borrows its counted output in the recovered-S shape, with no
/// producer, build slot or cache admission of its own. Its metadata and
/// reader partitions are charged to the settled S's own role.
pub(super) async fn retained_carrier(
    shared: &Shared,
    key: &str,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    plan: SegmentPlan,
    settings: &VodSettings,
    output: FiniteOutput,
) -> Result<Arc<Rendition>, String> {
    let segment_count = u32::try_from(plan.len())
        .map_err(|_| "finite software segment count overflows".to_owned())?;
    let deadline = Instant::now() + settings.materialize_budget;
    // The carrier's daemon metadata and reader tables are charged to the
    // settled S's own role partitions, like a recovered S. The carrier never
    // mints a closing owner, so no closing-owner heap is projected.
    let projected_metadata = finite_metadata::projected_requested_heap_bytes(
        &recipe,
        index.as_ref(),
        &plan,
        key,
        &shared.base.join(key),
    )
    .and_then(|bytes| bytes.checked_add(output.requested_heap_bytes()?))
    .ok_or("retained carrier metadata projection overflowed or is unsupported")?;
    let (metadata_lease, client_lease) = reserve_metadata_until(
        shared,
        key,
        &output,
        projected_metadata,
        finite_profile::CLIENT_STATE_BYTES as usize,
        deadline,
    )
    .await?;
    let (manifest, identity) =
        completed_output_state(&output, &recipe, &plan, segment_count, deadline).await?;
    let rendition = Shared::construct_rendition(
        key,
        RenditionDir::from_retained_finite(output),
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
    attach_metadata_partitions(rendition, metadata_lease, client_lease)
        .map_err(|(error, _rendition)| error)
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
    loop {
        // Register before the closed check: abandonment may happen before
        // this newly registered task receives its first poll.
        let changed = rendition.wake.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if rendition.closed.load(Acquire) {
            break;
        }
        changed.await;
        if rendition.closed.load(Acquire) || producer.driver_activated.load(Acquire) {
            break;
        }
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
    owner.invalidate_policy_service(Instant::now());
    let deadline = Instant::now() + rendition.materialize_budget;
    let retired = async {
        owner.freeze_output(deadline).await?;
        owner.retire_output(deadline).await
    }
    .await;
    match retired {
        Ok(_) => {
            producer.commit_retained_cache();
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
                    Some(event) => if let Err(error) = observe(shared, rendition, owner, event).await {
                        fail(shared, rendition, producer, format!("indexing finite output: {error}"));
                        owner.invalidate_policy_service(Instant::now());
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
        if let Err(error) = observe(shared, rendition, owner, event).await {
            fail(
                shared,
                rendition,
                producer,
                format!("indexing finite output: {error}"),
            );
            owner.invalidate_policy_service(Instant::now());
        }
    }
    // Joined production no longer supplies active service stability. Losing
    // that evidence never claims a native close or manufactures a cooldown.
    owner.invalidate_policy_service(Instant::now());
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

fn observe_policy_service(rendition: &Rendition, owner: &SoftwareVodOwner) {
    let now = Instant::now();
    if let Some((sample, _)) = rendition
        .recipe
        .encoding
        .as_ref()
        .and_then(|encoding| encoding.resource_service_sample(now))
    {
        // The accepted sample carries its original event time. Polls and
        // retained reads cannot refresh class stability or prove headroom.
        owner.observe_policy_service(sample, now);
    }
}

async fn observe(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    owner: &SoftwareVodOwner,
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
                    observe_policy_service(rendition, owner);
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
