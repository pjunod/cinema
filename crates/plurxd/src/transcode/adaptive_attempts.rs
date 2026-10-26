//! Original local owners of Store-coordinated finite software attempts.
//!
//! Store rows coordinate a winner, but cannot close a native owner. This
//! bounded controller registers the original attempt before its coordinator
//! first waits, then keeps a charged rich owner or its exact compact closing
//! selector until the corresponding physical and Store results are known.

use std::future::Future;
use std::sync::{Arc, Mutex};

use plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease;
use plurx_core::adaptive_resources::runtime::planning_probe::{
    FixedPlanningProbeCleanup, FixedPlanningProbeOwner,
};
use plurx_core::store::adaptive_resource_attempts::{
    selected_recipe_digest, AttemptAdoption, AttemptClaim, AttemptClose, AttemptMutation,
    AttemptPhase, MutationContext, PreparedAttempt, RequestDisposition, WorkerBinding,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::vodserve::{
    PreparedAttemptCleanup, PreparedAttemptCleanupTask, PreparedAttemptRendition,
};

/// The fixed node controller can refuse a new provisional worker without
/// growing a native-owning registry. This limit is independent of Store's
/// three-attempt-per-request ordinal bound.
const MAX_LOCAL_ATTEMPTS: usize = 64;

/// Explicit requested-heap policy for the temporary P/M-to-R recipe. This
/// pays for a measured prepared recipe before R admission; it is not an RSS
/// or allocator upper bound.
pub(super) const PLANNING_TO_R_HANDOFF_BYTES: usize = 16 * 1024 * 1024;

#[expect(
    clippy::large_enum_variant,
    reason = "Keep the original prepared acknowledgment inline in the bounded coordinator handoff; boxing introduces a new allocation beside the measured owner frame."
)]
pub(crate) enum AdaptivePrepareStart {
    Prepared(crate::media_sessions::AdaptiveAttemptPreparedV1),
    Pending,
    Started(
        tokio::sync::oneshot::Receiver<
            Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String>,
        >,
    ),
}

/// The P/M factory returns exactly one local selected recipe. The original
/// coordinator's charged fence stays in its registered entry until the
/// actual JoinHandle has been joined and its application frame destroyed.
pub(crate) struct PreparedAdaptiveWorkerRecipe {
    pub(crate) prepared: crate::vodserve::PreparedAdaptiveRecipe,
    pub(crate) settings: crate::vodserve::VodSettings,
    pub(crate) planning_application_limit_bytes: usize,
}

fn checked_caps(capacities: impl IntoIterator<Item = usize>) -> Option<usize> {
    capacities.into_iter().try_fold(0usize, usize::checked_add)
}

/// The P coordinator keeps the actual planning statement through fresh
/// probe refinement. Count its owned application capacities rather than
/// treating the original JSON wire length as a memory reservation. The
/// pinned Rust 1.97.1 BTreeMap (B=6) has at most 11 key/value slots and 12
/// child pointers per node; entries+1 maximally sized nodes conservatively
/// covers its private node layout. Allocator rounding remains outside this
/// requested-capacity claim.
pub(super) fn planning_snapshot_requested_heap_bytes(
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
) -> Option<usize> {
    let mut bytes = snapshot
        .file
        .requested_heap_bytes()?
        .checked_add(snapshot.probe_json.as_ref().map_or(0, String::capacity))?
        .checked_add(snapshot.item_title.as_ref().map_or(0, String::capacity))?;
    let node_bytes = std::mem::size_of::<(String, String)>()
        .checked_mul(11)?
        .checked_add(std::mem::size_of::<usize>().checked_mul(16)?)?;
    bytes = bytes.checked_add(
        snapshot
            .settings
            .len()
            .checked_add(1)?
            .checked_mul(node_bytes)?,
    )?;
    for (key, value) in &snapshot.settings {
        bytes = bytes
            .checked_add(key.capacity())?
            .checked_add(value.capacity())?;
    }
    Some(bytes)
}

fn control_binding_bytes(
    binding: &plurx_core::store::adaptive_resource_attempts::RequestBinding,
) -> Option<usize> {
    binding.principal.local_user_id()?;
    checked_caps([
        binding.key.request_id.capacity(),
        binding.request_fingerprint.capacity(),
        binding.playback_id.capacity(),
        binding.planning_source_digest.capacity(),
        binding.settings_digest.capacity(),
        binding.intent_digest.capacity(),
    ])
}

fn control_key_bytes(
    key: &plurx_core::store::adaptive_resource_attempts::AttemptKey,
) -> Option<usize> {
    checked_caps([
        key.request.request_id.capacity(),
        key.worker_node_id.capacity(),
    ])
}

fn claim_requested_heap_bytes(claim: &AttemptClaim) -> Option<usize> {
    let mut bytes = control_binding_bytes(&claim.binding)?
        .checked_add(control_key_bytes(&claim.key)?)?
        .checked_add(
            claim
                .domains
                .capacity()
                .checked_mul(std::mem::size_of::<String>())?,
        )?;
    for domain in &claim.domains {
        bytes = bytes.checked_add(domain.capacity())?;
    }
    Some(bytes)
}

/// Daemon-owned application heap retained across the original admitted P
/// collector. Core adds its own native/report/plan components to this value.
/// The caller must register the original coordinator before entering P; this
/// uses the compiler-measured frame, not a wire-size proxy.
fn planning_application_metadata_bytes(
    envelope: &crate::media_sessions::RemoteStartRequest,
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    claim: &AttemptClaim,
    coordinator_frame_bytes: usize,
) -> Option<usize> {
    checked_caps([
        envelope.requested_heap_bytes()?,
        planning_snapshot_requested_heap_bytes(snapshot)?,
        claim_requested_heap_bytes(claim)?,
        coordinator_frame_bytes,
        std::mem::size_of::<AttemptEntry>(),
        std::mem::size_of::<PlanningFailedAttempt>(),
        2 * std::mem::size_of::<usize>(), // original Arc entry header
        32 * 1024,                        // bounded canonical selected-route JSON backing
        PLANNING_TO_R_HANDOFF_BYTES,
    ])
}

pub(super) fn fixed_planning_request(
    manager: &super::TranscodeManager,
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    recipe_source_binding: &str,
    application_metadata_bytes: usize,
) -> Result<
    plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeRequest,
    String,
> {
    use plurx_core::adaptive_resources::runtime::planning_probe::{
        FixedPlanningProbeRequest, FixedPlanningSourceRequest,
    };
    let catalog = plurx_core::transcode::DecodeCatalogMetadata::from_media_file(&snapshot.file)
        .map_err(|error| format!("adaptive decoder catalog is invalid: {error}"))?;
    Ok(FixedPlanningProbeRequest {
        source: FixedPlanningSourceRequest {
            path: snapshot.file.path.clone(),
            media_file_id: snapshot.file.id.to_string(),
            scanner_size: snapshot.file.size,
            scanner_mtime: snapshot.file.mtime,
        },
        recipe_source_binding: recipe_source_binding.to_owned(),
        probe_program_locator: crate::ffmpeg::bound_ffprobe_bin(),
        cache: Arc::clone(&manager.decode_facts),
        catalog,
        selected_stream: crate::decode_facts::ProbeStreamSelection::LegacyVideoOrdinal(0),
        work: plurx_core::process::ChildWork::realtime("adaptive fixed planning probe"),
        ffmpeg_program: crate::ffmpeg::ffmpeg_bin(),
        application_metadata_bytes,
    })
}

fn adoption_requested_bytes(
    adoption: &plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption,
) -> Option<usize> {
    let activation = &adoption.activation;
    checked_caps([
        control_binding_bytes(&adoption.binding)?,
        control_key_bytes(&adoption.key)?,
        adoption.worker.node_id.capacity(),
        adoption.worker.planning_source_digest.capacity(),
        adoption.worker.selected_recipe_digest.capacity(),
        adoption.worker.prepared_handle_digest.capacity(),
        adoption.worker.output_digest.capacity(),
        activation.incarnation_id.capacity(),
        activation.session_id.capacity(),
        activation.playback_id.capacity(),
        activation.recovery_epoch.capacity(),
        activation
            .expected_predecessor_incarnation_id
            .as_ref()
            .map_or(0, String::capacity),
        activation.request_id.as_ref().map_or(0, String::capacity),
        activation.request_fingerprint.capacity(),
        activation.owner_node_id.capacity(),
        activation.recipe_json.capacity(),
        activation.response_json.capacity(),
    ])
}

fn fence_requested_bytes(
    fence: &plurx_core::store::adaptive_resource_attempts::AttemptFence,
) -> Option<usize> {
    checked_caps([
        control_binding_bytes(&fence.binding)?,
        control_key_bytes(&fence.key)?,
    ])
}

/// Requested application capacity carried across the request→R boundary.
/// The claim's binding/key allocations and selected route JSON move into the
/// original rich owner; its domains and deep ingress recipe are destroyed
/// before R admission. New fixed digest strings are projected by their exact
/// hex lengths and checked against actual capacities after construction.
/// The caller supplies the compiler-derived original coordinator frame size;
/// allocator rounding and Tokio's private task header are separate overhead.
pub(super) fn projected_application_owner_bytes(
    claim: &AttemptClaim,
    selected_recipe_json: &String,
    rendition_key_capacity: usize,
    coordinator_frame_bytes: usize,
    control_frame_bytes: usize,
) -> Option<usize> {
    claim.binding.principal.local_user_id()?;
    let binding = &claim.binding;
    let key = &claim.key;
    let strings = [
        binding.key.request_id.capacity(),
        binding.request_fingerprint.capacity(),
        binding.playback_id.capacity(),
        binding.planning_source_digest.capacity(),
        binding.settings_digest.capacity(),
        binding.intent_digest.capacity(),
        key.request.request_id.capacity(),
        key.worker_node_id.capacity(),
        selected_recipe_json.capacity(),
    ];
    let mut bytes = std::mem::size_of::<AttemptEntry>()
        .checked_add(2 * std::mem::size_of::<usize>())? // Arc entry header
        .checked_add(std::mem::size_of::<RichAttempt>())?
        .checked_add(std::mem::size_of::<HandoffAttempt>())?
        .checked_add(std::mem::size_of::<ClosingAttempt>())?
        .checked_add(crate::vodserve::prepared_attempt_cleanup_frame_bytes())?
        .checked_add(
            claim
                .domains
                .capacity()
                .checked_mul(std::mem::size_of::<String>())?,
        )?
        .checked_add(rendition_key_capacity)?
        .checked_add(coordinator_frame_bytes)?
        .checked_add(control_frame_bytes)?
        // One dormant-plan object and its canonical JSON are constructed
        // before Prepared; the retained original and one response copy can
        // coexist. Each is separately capped by ADAPTIVE_PREVIEW_BYTES.
        .checked_add(4 * crate::media_sessions::ADAPTIVE_PREVIEW_BYTES)?
        // One authenticated control DTO can outlive its HTTP waiter inside
        // the retained control task. There is only one such task slot. Its
        // actual requested capacities are checked before registration.
        .checked_add(crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES)?;
    for capacity in strings {
        bytes = bytes.checked_add(capacity)?;
    }
    for domain in &claim.domains {
        bytes = bytes.checked_add(domain.capacity())?;
    }
    // WorkerBinding repeats node ID and planning source digest, then owns
    // selected-recipe, prepared-handle and output SHA-256 hex values.
    bytes
        // The immutable Prepared (and later, mutually exclusive Close)
        // owns one additional binding/key copy beside the original Claim.
        .checked_add(control_binding_bytes(binding)?)?
        .checked_add(control_key_bytes(key)?)?
        .checked_add(key.worker_node_id.capacity())?
        .checked_add(binding.planning_source_digest.capacity())?
        .checked_add(3 * 64)
}

pub(super) struct AdaptiveAttemptRegistry {
    slots: Mutex<[Option<Arc<AttemptEntry>>; MAX_LOCAL_ATTEMPTS]>,
}

pub(super) struct AttemptEntry {
    attempt_id: Uuid,
    request_incarnation_id: Uuid,
    worker_instance_id: Uuid,
    serving_generation: u64,
    state: tokio::sync::Mutex<AttemptState>,
    /// Registered before the admitted P collector first enters native work.
    /// The compact cleanup remains the SAME original P/M selector through R
    /// ownership and is never replaced after an uncertain result.
    planning: tokio::sync::Mutex<PlanningState>,
    planning_application_lease: Mutex<Option<FiniteApplicationLease>>,
    coordinator: Mutex<Option<JoinHandle<()>>>,
    control: Mutex<Option<JoinHandle<()>>>,
    /// A compact post-rich fence. The manager drops it only after the actual
    /// coordinator and control futures have both been joined.
    finishing_lease: Mutex<Option<FiniteApplicationLease>>,
    charged_control_frame_bytes: std::sync::atomic::AtomicUsize,
}

enum PlanningState {
    Empty,
    Original(FixedPlanningProbeOwner),
    Closing(FixedPlanningProbeCleanup),
}

fn handoff_planning_cleanup(planning: &mut PlanningState) {
    let previous = std::mem::replace(planning, PlanningState::Empty);
    *planning = match previous {
        PlanningState::Original(owner) => PlanningState::Closing(owner.into_cleanup()),
        other => other,
    };
}

async fn finish_original_planning(
    planning: &mut PlanningState,
    deadline: std::time::Instant,
) -> Result<(), String> {
    match planning {
        PlanningState::Original(owner) => owner
            .finish_native(deadline)
            .await
            .map_err(|error| format!("closing adaptive fixed planning native owner: {error}"))?,
        PlanningState::Closing(_) => return Ok(()),
        PlanningState::Empty => {
            return Err("adaptive fixed planning original owner is absent".into());
        }
    }
    handoff_planning_cleanup(planning);
    Ok(())
}

/// `Rich` owns the sole M4 application fence. A transition to `Closing`
/// destroys every rich field before the fence can be dropped; the existing
/// finite closing owner remains registered independently. No Store DTO here
/// grants a native-close or ledger-refund fact.
pub(super) enum AttemptState {
    Reserved,
    PlanningFailed(Box<PlanningFailedAttempt>),
    Rich(Box<RichAttempt>),
    Handoff(Box<HandoffAttempt>),
    Closing(Box<ClosingAttempt>),
    Adopted,
    Closed,
    Unknown,
}

/// Store Prepared was never committed. A failed post-admission R builder
/// returns its exact original finite closing selector here; Store Close waits
/// both that R and the P/M book's positive closure.
pub(super) struct PlanningFailedAttempt {
    claim: AttemptClaim,
    cleanup: Option<PreparedAttemptCleanup>,
    fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
    close: Option<AttemptClose>,
}

/// Allocated only after the original core role has charged this requested
/// capacity. The compact pre-admission entry never allocates this payload.
pub(super) struct RichAttempt {
    pub(super) pending: PreparedAttemptRendition,
    pub(super) settings: crate::vodserve::VodSettings,
    pub(super) selected_recipe_json: String,
    pub(super) preview_json: Option<String>,
    /// Retained before any status read or native input acquisition. A failed
    /// pre-Prepared coordinator still has the exact provisional Store row to
    /// fence and the original finite owner to close.
    pub(super) claim: AttemptClaim,
    /// The exact original mutation payload, retained across an Unknown Store
    /// reply so the same UUID and sequence are replayed instead of reminted.
    pub(super) prepared: Option<PreparedAttempt>,
    pub(super) fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
    pub(super) adoption: Option<plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption>,
    // Last: destroy the pending owner and strings before releasing the
    // registered application fence.
    pub(super) application_lease: FiniteApplicationLease,
}

pub(super) struct ClosingAttempt {
    pub(super) cleanup: PreparedAttemptCleanup,
    pub(super) claim: AttemptClaim,
    pub(super) fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
    pub(super) close: Option<AttemptClose>,
}

/// The original registration task is retained here before it first awaits.
/// Neither cancellation of a control waiter nor a failed join drops the only
/// selector for the already acquired finite owner.
pub(super) struct HandoffAttempt {
    pub(super) task: PreparedAttemptCleanupTask,
    pub(super) claim: AttemptClaim,
    pub(super) fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
    // Last: the registration task and its payload die before this charge.
    pub(super) application_lease: FiniteApplicationLease,
}

impl Default for AdaptiveAttemptRegistry {
    fn default() -> Self {
        Self {
            slots: Mutex::new(std::array::from_fn(|_| None)),
        }
    }
}

impl AdaptiveAttemptRegistry {
    /// Claim one inline slot before scheduling any native-owning work. A
    /// replay selects only the exact same local process and serving epoch.
    pub(super) fn reserve(
        &self,
        attempt_id: Uuid,
        request_incarnation_id: Uuid,
        worker_instance_id: Uuid,
        serving_generation: u64,
    ) -> Result<Arc<AttemptEntry>, &'static str> {
        if attempt_id.is_nil() || request_incarnation_id.is_nil() || worker_instance_id.is_nil() {
            return Err("adaptive attempt has a nil identity");
        }
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "adaptive attempt registry is uncertain")?;
        if let Some(existing) = slots
            .iter()
            .flatten()
            .find(|entry| entry.attempt_id == attempt_id)
        {
            return if existing.request_incarnation_id == request_incarnation_id
                && existing.worker_instance_id == worker_instance_id
                && existing.serving_generation == serving_generation
            {
                Ok(Arc::clone(existing))
            } else {
                Err("adaptive attempt identity collided with an existing owner")
            };
        }
        let vacant = slots
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or("adaptive attempt owner registry is full")?;
        let entry = Arc::new(AttemptEntry {
            attempt_id,
            request_incarnation_id,
            worker_instance_id,
            serving_generation,
            state: tokio::sync::Mutex::new(AttemptState::Reserved),
            planning: tokio::sync::Mutex::new(PlanningState::Empty),
            planning_application_lease: Mutex::new(None),
            coordinator: Mutex::new(None),
            control: Mutex::new(None),
            finishing_lease: Mutex::new(None),
            charged_control_frame_bytes: std::sync::atomic::AtomicUsize::new(0),
        });
        *vacant = Some(Arc::clone(&entry));
        Ok(entry)
    }

    pub(super) fn get(&self, attempt_id: Uuid) -> Option<Arc<AttemptEntry>> {
        self.slots
            .lock()
            .ok()?
            .iter()
            .flatten()
            .find_map(|entry| (entry.attempt_id == attempt_id).then(|| Arc::clone(entry)))
    }

    pub(super) fn entries(&self) -> [Option<Arc<AttemptEntry>>; MAX_LOCAL_ATTEMPTS] {
        self.slots
            .lock()
            .map(|slots| std::array::from_fn(|index| slots[index].clone()))
            .unwrap_or_else(|_| std::array::from_fn(|_| None))
    }

    /// Remove only a terminal exact entry. The caller has already joined its
    /// original coordinator and verified known Store/physical disposition.
    pub(super) fn remove_terminal(&self, entry: &Arc<AttemptEntry>) -> Result<(), &'static str> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "adaptive attempt registry is uncertain")?;
        let Some(slot) = slots.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, entry))
        }) else {
            return Err("adaptive attempt original registry entry disappeared");
        };
        *slot = None;
        Ok(())
    }
}

impl AttemptEntry {
    async fn finish_adaptive_fixed_planning(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        let mut planning = self.planning.lock().await;
        finish_original_planning(&mut planning, deadline).await
    }

    async fn planning_sealed_for_logical_close(&self) -> Result<bool, String> {
        let mut planning = self.planning.lock().await;
        if matches!(*planning, PlanningState::Original(_)) {
            // A pure planning failure still owns the original P collector.
            // Retry its same native close before sealing the M report; an
            // uncertain result leaves the original owner and full charge.
            finish_original_planning(
                &mut planning,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await?;
        }
        match &mut *planning {
            PlanningState::Empty => Ok(true),
            PlanningState::Original(_) => unreachable!("original P close was attempted above"),
            PlanningState::Closing(cleanup) => {
                cleanup
                    .seal_for_logical_close(
                        std::time::Instant::now() + std::time::Duration::from_secs(1),
                    )
                    .await
                    .map_err(|error| {
                        format!("sealing original adaptive planning payload: {error}")
                    })?;
                Ok(true)
            }
        }
    }

    pub(super) fn attempt_id(&self) -> Uuid {
        self.attempt_id
    }

    pub(super) fn worker_instance_id(&self) -> Uuid {
        self.worker_instance_id
    }

    pub(super) fn serving_generation(&self) -> u64 {
        self.serving_generation
    }

    pub(super) async fn state(&self) -> tokio::sync::MutexGuard<'_, AttemptState> {
        self.state.lock().await
    }

    /// The task was spawned only after this entry was visible in the fixed
    /// table. Its handle stays here through request cancellation and is joined
    /// by the existing manager maintenance cadence.
    pub(super) fn install_coordinator(&self, task: JoinHandle<()>) -> Result<(), JoinHandle<()>> {
        let Ok(mut held) = self.coordinator.lock() else {
            return Err(task);
        };
        if held.is_some() {
            return Err(task);
        }
        *held = Some(task);
        Ok(())
    }

    pub(super) fn take_finished_coordinator(&self) -> Option<JoinHandle<()>> {
        let mut held = self.coordinator.lock().ok()?;
        if held.as_ref().is_some_and(JoinHandle::is_finished) {
            held.take()
        } else {
            None
        }
    }

    pub(super) fn take_finished_control(&self) -> Option<JoinHandle<()>> {
        let mut held = self.control.lock().ok()?;
        if held.as_ref().is_some_and(JoinHandle::is_finished) {
            held.take()
        } else {
            None
        }
    }

    pub(super) fn spawn_control<F>(self: &Arc<Self>, future: F) -> Result<(), &'static str>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let original = async move {
            if start_rx.await.is_ok() {
                future.await;
            }
        };
        if std::mem::size_of_val(&original)
            > self
                .charged_control_frame_bytes
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("adaptive control frame exceeds its charged partition");
        }
        let task = tokio::spawn(original);
        let Ok(mut held) = self.control.lock() else {
            return Err("adaptive control registry is uncertain");
        };
        if held.is_some() {
            return Err("adaptive control already owns the original task slot");
        }
        *held = Some(task);
        let _ = start_tx.send(());
        Ok(())
    }

    pub(super) fn release_finished_application(&self) -> Result<(), &'static str> {
        let coordinator = self
            .coordinator
            .lock()
            .map_err(|_| "coordinator hold is uncertain")?;
        let control = self
            .control
            .lock()
            .map_err(|_| "control hold is uncertain")?;
        if coordinator.is_some() || control.is_some() {
            return Ok(());
        }
        let mut lease = self
            .finishing_lease
            .lock()
            .map_err(|_| "application hold is uncertain")?;
        lease.take();
        // The P/M controller still owns its compact Claim/Fence/Close record
        // until the logical StoreClose is known. A native seal is not a memory
        // refund: only terminal state and joined original tasks release this
        // application's one-shot M fence.
        if self
            .state
            .try_lock()
            .is_ok_and(|state| matches!(*state, AttemptState::Closed | AttemptState::Adopted))
        {
            self.planning_application_lease
                .lock()
                .map_err(|_| "planning application hold is uncertain")?
                .take();
        }
        Ok(())
    }

    pub(super) fn no_original_tasks_or_application(&self) -> bool {
        self.coordinator.lock().is_ok_and(|held| held.is_none())
            && self.control.lock().is_ok_and(|held| held.is_none())
            && self
                .planning_application_lease
                .lock()
                .is_ok_and(|held| held.is_none())
            && self.finishing_lease.lock().is_ok_and(|held| held.is_none())
    }

    /// The start latch keeps the original task inert until its JoinHandle is
    /// retained by the already registered entry. Failure cannot enter native
    /// work and never requires abort-as-closure.
    pub(super) fn spawn_coordinator<F>(
        self: &Arc<Self>,
        future: F,
        frame_tx: tokio::sync::oneshot::Sender<usize>,
    ) -> Result<(), &'static str>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let original = async move {
            if start_rx.await.is_ok() {
                future.await;
            }
        };
        let frame_bytes = std::mem::size_of_val(&original);
        let task = tokio::spawn(original);
        if self.install_coordinator(task).is_err() {
            return Err("adaptive attempt already has an original coordinator");
        }
        let _ = frame_tx.send(frame_bytes);
        let _ = start_tx.send(());
        Ok(())
    }
}

impl super::TranscodeManager {
    async fn admit_adaptive_fixed_probe(
        &self,
        entry: &Arc<AttemptEntry>,
        claim: &AttemptClaim,
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        application_metadata_bytes: usize,
        deadline: std::time::Instant,
    ) -> Result<
        (
            plurx_core::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease,
            usize,
        ),
        String,
    > {
        let runtime = self
            .resource_runtime
            .as_ref()
            .ok_or("adaptive fixed planning runtime is unavailable")?;
        runtime
            .refresh_admission_capacity(deadline)
            .await
            .map_err(|error| format!("observing adaptive planning admission: {error:?}"))?;
        let request = fixed_planning_request(
            self,
            snapshot,
            &claim.binding.planning_source_digest,
            application_metadata_bytes,
        )?;
        let mut planning = entry.planning.lock().await;
        if !matches!(*planning, PlanningState::Empty) {
            return Err("adaptive attempt already registered an original planning owner".into());
        }
        // Admission is synchronous. Register its sole original selector in
        // the already visible attempt entry before the first P native await.
        let owner = runtime
            .prepare_fixed_planning_probe(request, deadline)
            .map_err(|error| format!("admitting adaptive fixed planning: {error}"))?;
        *planning = PlanningState::Original(owner);
        let PlanningState::Original(owner) = &mut *planning else {
            unreachable!()
        };
        let planning_lease = match owner.take_application_lease(application_metadata_bytes) {
            Ok(lease) => lease,
            Err(error) => {
                handoff_planning_cleanup(&mut planning);
                return Err(format!("retaining adaptive planning coordinator: {error}"));
            }
        };
        {
            let mut held = entry
                .planning_application_lease
                .lock()
                .map_err(|_| "adaptive planning application registry is uncertain")?;
            if held.is_some() {
                return Err("adaptive planning application lease was already installed".into());
            }
            *held = Some(planning_lease);
        }
        if let Err(error) = owner.prepare_inputs(deadline).await {
            handoff_planning_cleanup(&mut planning);
            return Err(format!("acquiring adaptive fixed planning inputs: {error}"));
        }
        let report = match owner.collect_and_refine(deadline).await {
            Ok(report) => report,
            Err(error) => {
                handoff_planning_cleanup(&mut planning);
                return Err(format!(
                    "collecting adaptive fixed planning report: {error}"
                ));
            }
        };
        let actual_limit = report
            .application_limit_bytes()
            .and_then(|value| usize::try_from(value).ok());
        if actual_limit.is_none_or(|limit| limit < application_metadata_bytes) {
            handoff_planning_cleanup(&mut planning);
            return Err(
                "adaptive planning application partition is smaller than its admitted request"
                    .into(),
            );
        }
        // Keep the original P owner and Unknown peak active while the fresh
        // report is parsed and the pure recipe's actual capacity is checked.
        Ok((report, application_metadata_bytes))
    }

    /// The Store's scanner row and authenticated peer intent are both
    /// negative checks. The admitted P collector supplies the sole fresh
    /// source/probe observation; no daemon pathname open or legacy FFprobe
    /// may precede it on this selected path.
    pub(super) async fn prepare_adaptive_worker_recipe(
        &self,
        _state: &crate::state::AppState,
        envelope: &crate::media_sessions::RemoteStartRequest,
        claim: &AttemptClaim,
        entry: &Arc<AttemptEntry>,
        coordinator_frame_bytes: usize,
        deadline: std::time::Instant,
    ) -> Result<PreparedAdaptiveWorkerRecipe, String> {
        if !envelope.is_valid()
            || envelope.principal != claim.binding.principal
            || envelope.request.file_id <= 0
            || envelope.request.playback_id != claim.binding.playback_id
            || envelope.incarnation_id != claim.key.route_incarnation_id.to_string()
            || std::time::Instant::now() >= deadline
        {
            return Err("adaptive worker intent or original deadline is invalid".into());
        }
        let user_id = envelope
            .principal
            .local_user_id()
            .filter(|id| *id > 0)
            .ok_or("adaptive worker requires an authorized local principal")?;
        self.store
            .get_user(user_id)
            .await
            .map_err(|error| format!("reading adaptive local principal: {error}"))?
            .ok_or("adaptive local principal is no longer present")?;
        let snapshot = self
            .store
            .playback_planning_snapshot(envelope.request.file_id, &super::QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| format!("reading adaptive source planning row: {error}"))?
            .ok_or("adaptive source file is absent")?;
        if snapshot.file.id != envelope.request.file_id
            || snapshot.file.size != envelope.source_size
            || snapshot.file.mtime != envelope.source_mtime
        {
            return Err("adaptive source scanner row changed before native admission".into());
        }
        let application_metadata_bytes = planning_application_metadata_bytes(
            envelope,
            &snapshot,
            claim,
            coordinator_frame_bytes,
        )
        .ok_or("adaptive planning application capacity is unsupported or overflowed")?;
        let settings = self
            .vod_settings(&envelope.request)
            .await?
            .ok_or("adaptive VOD presentation is disabled")?;
        // This one call is the P boundary supplied by the core fixed-probe
        // integration. It reserves P/M before opening either original source
        // or executable, and returns the same charged M while retaining P.
        let (report, requested_bytes) = self
            .admit_adaptive_fixed_probe(
                entry,
                claim,
                &snapshot,
                application_metadata_bytes,
                deadline,
            )
            .await?;
        let encoding = self
            .resolve_adaptive_encoding_from_probe(
                &envelope.request,
                &snapshot.file,
                &snapshot,
                report,
            )
            .await?;
        let mut file = snapshot.file;
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            envelope.request.audio_offset_ms.clamp(-15_000, 15_000)
        };
        let mut recipe = crate::vodserve::VodRecipeRequest {
            root_admission: None,
            measured_candidate: None,
            retained_capture: crate::vodserve::RetainedOutputCapture::New,
            request: &envelope.request,
            encoding: Some(encoding),
            soundtrack: None,
            companion: None,
        };
        let prepared = self
            .vod
            .resolve_adaptive_recipe(&mut recipe, &file, &settings)
            .await?;
        Ok(PreparedAdaptiveWorkerRecipe {
            prepared,
            settings,
            planning_application_limit_bytes: requested_bytes,
        })
    }

    pub(crate) async fn begin_adaptive_prepare(
        self: &Arc<Self>,
        state: crate::state::AppState,
        request: crate::media_sessions::AdaptiveAttemptPrepareV1,
        deadline: std::time::Instant,
        restart_admission: crate::serving_fence::RestartAdmission,
    ) -> Result<AdaptivePrepareStart, String> {
        self.verify_adaptive_claim(&request.claim)
            .await
            .map_err(str::to_owned)?;
        let attempt_id = request.claim.key.attempt_id;
        if let Some(existing) = self.adaptive_attempts.get(attempt_id) {
            if existing.worker_instance_id() != request.claim.worker_instance_id
                || existing.serving_generation() != request.claim.serving_generation
            {
                return Err("adaptive replay names a different original worker".into());
            }
            if let Some(reply) = self.replay_adaptive_prepared(attempt_id).await {
                return reply.map(AdaptivePrepareStart::Prepared);
            }
            return Ok(AdaptivePrepareStart::Pending);
        }
        let entry = self
            .adaptive_attempts
            .reserve(
                attempt_id,
                request.claim.key.request.request_incarnation_id,
                request.claim.worker_instance_id,
                request.claim.serving_generation,
            )
            .map_err(str::to_owned)?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        entry
            .spawn_coordinator(
                async move {
                    let _restart_admission = restart_admission;
                    let result = manager
                        .run_adaptive_prepare(state, request, original, deadline, frame_rx)
                        .await;
                    let _ = reply_tx.send(result);
                },
                frame_tx,
            )
            .map_err(str::to_owned)?;
        Ok(AdaptivePrepareStart::Started(reply_rx))
    }

    async fn run_adaptive_prepare(
        &self,
        state: crate::state::AppState,
        request: crate::media_sessions::AdaptiveAttemptPrepareV1,
        entry: Arc<AttemptEntry>,
        deadline: std::time::Instant,
        frame_rx: tokio::sync::oneshot::Receiver<usize>,
    ) -> Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String> {
        let coordinator_frame_bytes = match frame_rx.await {
            Ok(bytes) => bytes,
            Err(_) => {
                let mut state = entry.state().await;
                *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                    claim: request.claim,
                    cleanup: None,
                    fence: None,
                    close: None,
                }));
                return Err("adaptive original coordinator frame was not registered".into());
            }
        };
        if std::time::Instant::now() >= deadline
            || !state
                .serving
                .authority()
                .is_current(entry.serving_generation())
        {
            let mut state = entry.state().await;
            *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                claim: request.claim,
                cleanup: None,
                fence: None,
                close: None,
            }));
            return Err("adaptive planning lost its original deadline or serving authority".into());
        }
        let trial_readiness = self
            .vod
            .observe_adaptive_trial_readiness(deadline, None)
            .await;
        tracing::debug!(
            target: "plurxd::adaptive",
            refusal = ?trial_readiness.refusal,
            active_vod_sessions = trial_readiness.active_vod_sessions,
            accepted_control_sessions = trial_readiness.accepted_control_sessions,
            "adaptive trial readiness advisory; fixed admission remains authoritative"
        );
        // The factory admits the original fixed P collector before source or
        // executable open and retains P through pure recipe construction.
        // No legacy prepare_vod_encoding_with_source or unowned FFprobe enters.
        let planned_result = self
            .prepare_adaptive_worker_recipe(
                &state,
                &request.recipe,
                &request.claim,
                &entry,
                coordinator_frame_bytes,
                deadline,
            )
            .await;
        let planned = match planned_result {
            Ok(planned) => planned,
            Err(error) => {
                let mut state = entry.state().await;
                if matches!(*state, AttemptState::Reserved) {
                    *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                        claim: request.claim,
                        cleanup: None,
                        fence: None,
                        close: None,
                    }));
                }
                return Err(error);
            }
        };
        let pre_r = (|| -> Result<(String, usize), String> {
            let prepared_heap = planned
                .prepared
                .requested_heap_bytes()
                .ok_or("adaptive recipe application capacity overflow")?;
            if prepared_heap > PLANNING_TO_R_HANDOFF_BYTES {
                return Err(
                    "adaptive prepared recipe exceeds its explicit planning-to-R allowance".into(),
                );
            }
            let live_handoff_bytes = checked_caps([
                coordinator_frame_bytes,
                request
                    .recipe
                    .requested_heap_bytes()
                    .ok_or("adaptive worker envelope has unsupported application allocations")?,
                claim_requested_heap_bytes(&request.claim)
                    .ok_or("adaptive Claim application capacity overflow")?,
                prepared_heap,
                32 * 1024, // exact selected-route backing allocation
            ])
            .ok_or("adaptive planning handoff capacity overflow")?;
            if live_handoff_bytes > planned.planning_application_limit_bytes {
                return Err("adaptive planning handoff exceeds its precharged M partition".into());
            }
            let selected_recipe_json = planned.prepared.selected_recipe_json(&request.recipe)?;
            let requested_application_bytes = projected_application_owner_bytes(
                &request.claim,
                &selected_recipe_json,
                planned.prepared.key_capacity(),
                coordinator_frame_bytes,
                crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES,
            )
            .ok_or("adaptive application capacity overflow")?;
            Ok((selected_recipe_json, requested_application_bytes))
        })();
        let (selected_recipe_json, requested_application_bytes) = match pre_r {
            Ok(values) => values,
            Err(error) => {
                let mut state = entry.state().await;
                if matches!(*state, AttemptState::Reserved) {
                    *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                        claim: request.claim,
                        cleanup: None,
                        fence: None,
                        close: None,
                    }));
                }
                return Err(error);
            }
        };
        if let Err(error) = entry.finish_adaptive_fixed_planning(deadline).await {
            let mut state = entry.state().await;
            if matches!(*state, AttemptState::Reserved) {
                *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                    claim: request.claim,
                    cleanup: None,
                    fence: None,
                    close: None,
                }));
            }
            return Err(error);
        }
        let PreparedAdaptiveWorkerRecipe {
            prepared,
            settings,
            planning_application_limit_bytes: _,
        } = planned;
        let built = self
            .vod
            .build_adaptive_recipe(prepared, &settings, requested_application_bytes)
            .await;
        let pending = match built {
            Ok(pending) => pending,
            Err(crate::vodserve::AdaptiveBuildFailure { message, cleanup }) => {
                let mut state = entry.state().await;
                if matches!(*state, AttemptState::Reserved) {
                    *state = AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                        claim: request.claim,
                        cleanup,
                        fence: None,
                        close: None,
                    }));
                }
                return Err(message);
            }
        };
        self.record_adaptive_prepared(
            &entry,
            request.claim,
            selected_recipe_json,
            pending,
            settings,
            deadline,
            requested_application_bytes,
        )
        .await
    }

    pub(crate) fn begin_adaptive_adopt(
        self: &Arc<Self>,
        adoption: plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption,
        preview_json: String,
        authority: crate::serving_fence::ServingAuthority,
        deadline: std::time::Instant,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<crate::vodserve::VodStart, String>>, String>
    {
        if preview_json.len() > crate::media_sessions::ADAPTIVE_PREVIEW_BYTES
            || adoption_requested_bytes(&adoption)
                .and_then(|bytes| bytes.checked_add(preview_json.capacity()))
                .is_none_or(|bytes| {
                    bytes > crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES
                })
        {
            return Err("adaptive Adopt exceeds its charged control-payload partition".into());
        }
        let entry = self
            .adaptive_attempts
            .get(adoption.key.attempt_id)
            .ok_or("adaptive Adopt has no local original owner")?;
        if adoption.worker.worker_instance_id != entry.worker_instance_id()
            || adoption.worker.serving_generation != entry.serving_generation()
        {
            return Err("adaptive Adopt targets a different process owner".into());
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        entry
            .spawn_control(async move {
                let result = manager
                    .adopt_adaptive_attempt(&original, adoption, preview_json, authority, deadline)
                    .await;
                let _ = sender.send(result);
            })
            .map_err(str::to_owned)?;
        Ok(receiver)
    }

    pub(crate) fn begin_adaptive_cancel(
        self: &Arc<Self>,
        fence: plurx_core::store::adaptive_resource_attempts::AttemptFence,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        if fence_requested_bytes(&fence)
            .is_none_or(|bytes| bytes > crate::media_sessions::MAX_CONTROL_REQUEST_BYTES)
        {
            return Err("adaptive Fence exceeds its charged control-payload partition".into());
        }
        let entry = self
            .adaptive_attempts
            .get(fence.key.attempt_id)
            .ok_or("adaptive Fence has no local original owner")?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        entry
            .spawn_control(async move {
                let result = manager.cancel_adaptive_attempt(&original, fence).await;
                let _ = sender.send(result);
            })
            .map_err(str::to_owned)?;
        Ok(receiver)
    }

    /// Store Adopt is replayed from the one retained DTO; only a known exact
    /// result lets this worker publish the already prepared Rendition.
    pub(super) async fn adopt_adaptive_attempt(
        &self,
        entry: &Arc<AttemptEntry>,
        adoption: plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption,
        preview_json: String,
        authority: crate::serving_fence::ServingAuthority,
        deadline: std::time::Instant,
    ) -> Result<crate::vodserve::VodStart, String> {
        if !authority.is_current(entry.serving_generation())
            || std::time::Instant::now() >= deadline
        {
            return Err("adaptive worker lost its original serving authority or deadline".into());
        }
        let mut state = entry.state().await;
        let AttemptState::Rich(rich) = &mut *state else {
            return Err("adaptive Adopt has no original prepared owner".into());
        };
        let prepared = rich
            .prepared
            .as_ref()
            .ok_or("adaptive Prepared identity is absent")?;
        if rich.fence.is_some()
            || adoption.binding != prepared.binding
            || adoption.key != prepared.key
            || adoption.worker != prepared.worker
            || adoption.key.attempt_id != entry.attempt_id()
            || adoption.activation.recipe_json != rich.selected_recipe_json
            || adoption.activation.session_id != adoption.key.session_id.to_string()
            || adoption.activation.incarnation_id != adoption.key.route_incarnation_id.to_string()
        {
            return Err("adaptive Adopt differs from the original selected worker".into());
        }
        if rich
            .adoption
            .as_ref()
            .is_some_and(|original| original != &adoption)
        {
            return Err("adaptive Adopt changed its original mutation".into());
        }
        let route: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&rich.selected_recipe_json)
                .map_err(|error| format!("decoding exact adaptive route: {error}"))?;
        if !route.is_valid()
            || route.incarnation_id != adoption.activation.incarnation_id
            || route.principal != adoption.binding.principal
            || route.request.playback_id != adoption.binding.playback_id
        {
            return Err("adaptive Adopt route differs from locally selected intent".into());
        }
        if rich.preview_json.as_deref() != Some(preview_json.as_str()) {
            return Err("adaptive Adopt changed the original prepared preview".into());
        }
        let preview: crate::media_sessions::RemoteStartResponse =
            serde_json::from_str(&preview_json)
                .map_err(|error| format!("decoding adaptive activation preview: {error}"))?;
        if preview.session_id != adoption.key.session_id.to_string() || !preview.vod {
            return Err("adaptive activation preview differs from its fixed VOD session".into());
        }
        let public: crate::http::hls::StartResponse =
            serde_json::from_str(&adoption.activation.response_json)
                .map_err(|error| format!("decoding canonical adaptive start response: {error}"))?;
        if public.session_id != preview.session_id
            || public.playlist_url != preview.playlist_url
            || public.duration_ms != preview.duration_ms
            || public.delivered_audio != preview.audio_delivery
            || public.start_seconds != preview.start_seconds
            || public.media_origin_ms
                != Some((preview.media_origin_seconds * 1000.0).round() as i64)
            || public.height != preview.target_height
            || public.encoder != preview.encoder
            || public.vod != preview.vod
        {
            return Err("adaptive activation response differs from the prepared preview".into());
        }
        if preview.duration_ms != Some(rich.pending.planned_duration_ms()) {
            return Err(
                "adaptive activation preview duration differs from the original plan".into(),
            );
        }
        let user_id = adoption
            .binding
            .principal
            .local_user_id()
            .ok_or("adaptive selected worker requires a local user")?;
        let user = self
            .store
            .get_user(user_id)
            .await
            .map_err(|error| format!("reading adaptive viewer: {error}"))?
            .ok_or("adaptive viewer disappeared before Adopt")?;
        if rich.adoption.is_none() {
            rich.adoption = Some(adoption);
        }
        let original = rich.adoption.as_ref().expect("stored exact Adopt");
        let outcome = match self.store.adopt_adaptive_winner(original).await {
            Ok(AttemptAdoption::Applied(outcome) | AttemptAdoption::Replay(outcome)) => outcome,
            Ok(AttemptAdoption::Refused(_)) => {
                return Err("adaptive Adopt was refused; owner remains held".into())
            }
            Err(error) => {
                return Err(format!(
                    "adaptive Adopt is unknown; original mutation remains held: {error}"
                ))
            }
        };
        if outcome.route.incarnation_id != original.activation.incarnation_id
            || outcome.route.session_id != original.activation.session_id
            || outcome.route.principal != original.binding.principal
            || outcome.route.owner_node_id != original.worker.node_id
            || outcome.route.recipe_json != rich.selected_recipe_json
            || outcome.route.state != "active"
            || outcome.route.publication_ready_at_ms
                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        {
            return Err("adaptive Adopt committed a different route".into());
        }
        let start = rich
            .pending
            .attach_after_store_adopt(
                &self.vod,
                &route,
                &rich.settings,
                &user.username,
                original.key.session_id.to_string(),
                authority,
                entry.serving_generation(),
                deadline,
            )
            .await?;
        if start.session_id != original.key.session_id.to_string() {
            return Err("adaptive attachment returned a different session".into());
        }
        if preview.duration_ms != Some(start.duration_ms) {
            return Err(
                "adaptive activation preview duration differs from the attached plan".into(),
            );
        }
        let AttemptState::Rich(rich) = std::mem::replace(&mut *state, AttemptState::Adopted) else {
            unreachable!();
        };
        let RichAttempt {
            application_lease, ..
        } = *rich;
        let mut held = entry
            .finishing_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_some() {
            std::mem::forget(application_lease);
            return Err("adaptive Adopt application fence was already retained".into());
        }
        *held = Some(application_lease);
        Ok(start)
    }

    /// Only the actual finite closing owner's known result and destruction of
    /// all charged M4 task frames allow the Store's logical Close. The close
    /// record is retained before its await and replayed with the same UUID.
    async fn finish_adaptive_close(&self, entry: &Arc<AttemptEntry>) -> Result<bool, String> {
        let mut state = entry.state().await;
        let AttemptState::Closing(closing) = &mut *state else {
            return Ok(false);
        };
        if !closing.cleanup.known_closed()
            || !entry.planning_sealed_for_logical_close().await?
            || entry
                .finishing_lease
                .lock()
                .map_err(|_| "adaptive application fence is uncertain")?
                .is_some()
        {
            return Ok(false);
        }
        let status = self
            .store
            .adaptive_request_status(&closing.claim.binding.key, &closing.claim.binding.principal)
            .await
            .map_err(|error| format!("reading adaptive close status: {error}"))?
            .ok_or("adaptive request disappeared before close")?;
        status
            .validate()
            .map_err(|error| format!("invalid adaptive close status: {error}"))?;
        if status.binding != closing.claim.binding {
            return Err("adaptive close binding changed".into());
        }
        let attempt = status
            .attempts
            .iter()
            .find(|attempt| attempt.key == closing.claim.key)
            .ok_or("adaptive attempt disappeared before close")?;
        if attempt.phase == AttemptPhase::Closed {
            if closing
                .close
                .as_ref()
                .is_some_and(|close| attempt.last_operation_id == close.context.operation_id)
            {
                *state = AttemptState::Closed;
                return Ok(true);
            }
            return Err("adaptive attempt closed by an unrecognized operation".into());
        }
        if !matches!(attempt.phase, AttemptPhase::Cleanup | AttemptPhase::Losing)
            || closing
                .fence
                .as_ref()
                .is_some_and(|fence| attempt.last_operation_id != fence.context.operation_id)
        {
            return Err("adaptive close is not the exact losing or fenced attempt".into());
        }
        if closing.close.is_none() {
            closing.close = Some(AttemptClose {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: status.sequence,
                    expected_attempt_sequence: attempt.sequence,
                    now_ms: crate::media_sessions::unix_ms(),
                },
                binding: closing.claim.binding.clone(),
                key: closing.claim.key.clone(),
                worker_instance_id: closing.claim.worker_instance_id,
                serving_generation: closing.claim.serving_generation,
                cleanup_operation_id: attempt.last_operation_id,
            });
        }
        let original = closing.close.as_ref().expect("stored exact Close");
        let closed = self.store.close_adaptive_attempt(original).await;
        match closed {
            Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status))
                if status.binding == original.binding
                    && status.attempts.iter().any(|attempt| {
                        attempt.key == original.key
                            && attempt.phase == AttemptPhase::Closed
                            && attempt.last_operation_id == original.context.operation_id
                    }) =>
            {
                *state = AttemptState::Closed;
                Ok(true)
            }
            Ok(_) => Err("adaptive Close was refused; exact owner remains registered".into()),
            Err(error) => Err(format!(
                "adaptive Close is unknown; exact operation remains registered: {error}"
            )),
        }
    }

    /// A failed P/M preparation never admitted finite R. Fence the exact
    /// provisional Store row, then close it only after the original P/M
    /// native and metadata book has positively settled. Unknown Store replies
    /// retain the same operation and local selector for the next reap pass.
    async fn finish_failed_adaptive_planning(
        &self,
        entry: &Arc<AttemptEntry>,
    ) -> Result<(), String> {
        let mut state = entry.state().await;
        let AttemptState::PlanningFailed(failed) = &mut *state else {
            return Ok(());
        };
        if entry
            .coordinator
            .lock()
            .map_err(|_| "adaptive coordinator hold is uncertain")?
            .is_some()
        {
            return Ok(());
        }
        let status = self
            .store
            .adaptive_request_status(&failed.claim.binding.key, &failed.claim.binding.principal)
            .await
            .map_err(|error| format!("reading failed adaptive planning row: {error}"))?
            .ok_or("failed adaptive planning row disappeared")?;
        status
            .validate()
            .map_err(|error| format!("invalid failed adaptive planning row: {error}"))?;
        if status.binding != failed.claim.binding {
            return Err("failed adaptive planning binding changed".into());
        }
        let attempt = status
            .attempts
            .iter()
            .find(|row| row.key == failed.claim.key)
            .ok_or("failed adaptive planning attempt disappeared")?;
        if attempt.phase == AttemptPhase::Closed {
            if failed
                .close
                .as_ref()
                .is_some_and(|original| attempt.last_operation_id == original.context.operation_id)
            {
                *state = AttemptState::Closed;
                return Ok(());
            }
            return Err("failed adaptive planning was closed by an unrecognized operation".into());
        }
        if attempt.phase == AttemptPhase::Provisional {
            if failed.fence.is_none() {
                failed.fence = Some(
                    plurx_core::store::adaptive_resource_attempts::AttemptFence {
                        context: MutationContext {
                            operation_id: Uuid::new_v4(),
                            expected_request_sequence: status.sequence,
                            expected_attempt_sequence: attempt.sequence,
                            now_ms: crate::media_sessions::unix_ms(),
                        },
                        binding: failed.claim.binding.clone(),
                        key: failed.claim.key.clone(),
                        cancel_request: false,
                    },
                );
            }
            let original = failed
                .fence
                .as_ref()
                .expect("retained failed-planning Fence");
            match self.store.fence_adaptive_attempt(original).await {
                Ok(AttemptMutation::Applied(committed) | AttemptMutation::Replay(committed))
                    if committed.binding == original.binding
                        && committed.attempts.iter().any(|row| {
                            row.key == original.key
                                && row.phase == AttemptPhase::Cleanup
                                && row.last_operation_id == original.context.operation_id
                        }) =>
                {
                    return Ok(())
                }
                Ok(_) => {
                    return Err(
                        "failed-planning Fence was refused; original owner remains held".into(),
                    )
                }
                Err(error) => return Err(format!("failed-planning Fence is unknown: {error}")),
            }
        }
        if !matches!(attempt.phase, AttemptPhase::Cleanup | AttemptPhase::Losing)
            || failed
                .fence
                .as_ref()
                .is_some_and(|fence| attempt.last_operation_id != fence.context.operation_id)
        {
            return Err("failed adaptive planning is not a fenced or losing attempt".into());
        }
        if !entry.planning_sealed_for_logical_close().await?
            || failed
                .cleanup
                .as_ref()
                .is_some_and(|owner| !owner.known_closed())
        {
            return Ok(());
        }
        if failed.close.is_none() {
            failed.close = Some(AttemptClose {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: status.sequence,
                    expected_attempt_sequence: attempt.sequence,
                    now_ms: crate::media_sessions::unix_ms(),
                },
                binding: failed.claim.binding.clone(),
                key: failed.claim.key.clone(),
                worker_instance_id: failed.claim.worker_instance_id,
                serving_generation: failed.claim.serving_generation,
                cleanup_operation_id: attempt.last_operation_id,
            });
        }
        let original = failed
            .close
            .as_ref()
            .expect("retained failed-planning Close");
        match self.store.close_adaptive_attempt(original).await {
            Ok(AttemptMutation::Applied(committed) | AttemptMutation::Replay(committed))
                if committed.binding == original.binding
                    && committed.attempts.iter().any(|row| {
                        row.key == original.key
                            && row.phase == AttemptPhase::Closed
                            && row.last_operation_id == original.context.operation_id
                    }) =>
            {
                *state = AttemptState::Closed;
                Ok(())
            }
            Ok(_) => Err("failed-planning Close was refused; original owner remains held".into()),
            Err(error) => Err(format!("failed-planning Close is unknown: {error}")),
        }
    }

    /// Fence the exact Store attempt, then hand its original finite owner to
    /// the existing closing registry. The control task calling this method is
    /// retained on `AttemptEntry`; HTTP cancellation cannot drop the owner.
    pub(super) async fn cancel_adaptive_attempt(
        &self,
        entry: &Arc<AttemptEntry>,
        fence: plurx_core::store::adaptive_resource_attempts::AttemptFence,
    ) -> Result<(), String> {
        fence
            .context
            .validate()
            .map_err(|error| format!("invalid adaptive Fence: {error}"))?;
        let mut state = entry.state().await;
        let AttemptState::Rich(rich) = &mut *state else {
            return Err("adaptive attempt has no prepared finite owner to fence".into());
        };
        if rich.adoption.is_some() {
            return Err("adaptive Adopt outcome must be reconciled before cancellation".into());
        }
        if fence.binding != rich.claim.binding
            || fence.key != rich.claim.key
            || fence.key.attempt_id != entry.attempt_id()
            || rich.claim.worker_instance_id != entry.worker_instance_id()
        {
            return Err("adaptive Fence targets a different original owner".into());
        }
        if rich
            .fence
            .as_ref()
            .is_some_and(|original| original != &fence)
        {
            return Err("adaptive Fence changed its original operation".into());
        }
        if rich.fence.is_none() {
            rich.fence = Some(fence);
        }
        let original = rich.fence.as_ref().expect("stored exact Fence");
        let result = self.store.fence_adaptive_attempt(original).await;
        let status = match result {
            Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status)) => status,
            Ok(_) => return Err("adaptive Fence was refused; original owner remains held".into()),
            Err(error) => {
                return Err(format!(
                    "adaptive Fence is unknown; original owner remains held: {error}"
                ))
            }
        };
        if status.binding != original.binding
            || !status.attempts.iter().any(|attempt| {
                attempt.key == original.key
                    && matches!(attempt.phase, AttemptPhase::Cleanup | AttemptPhase::Losing)
            })
        {
            return Err("adaptive Fence committed a different attempt state".into());
        }
        drop(state);
        self.handoff_adaptive_loser(entry).await
    }

    async fn handoff_adaptive_loser(&self, entry: &Arc<AttemptEntry>) -> Result<(), String> {
        let mut state = entry.state().await;
        if !matches!(*state, AttemptState::Rich(_)) {
            return Err("adaptive loser has no original prepared owner".into());
        }
        let AttemptState::Rich(rich) = std::mem::replace(&mut *state, AttemptState::Unknown) else {
            unreachable!();
        };
        let RichAttempt {
            pending,
            settings: _,
            selected_recipe_json,
            preview_json: _,
            claim,
            prepared: _,
            fence,
            adoption: _,
            application_lease,
        } = *rich;
        drop(selected_recipe_json);
        let task = pending.begin_abandon()?;
        *state = AttemptState::Handoff(Box::new(HandoffAttempt {
            task,
            claim,
            fence,
            application_lease,
        }));
        Ok(())
    }

    async fn reconcile_adaptive_handoff(&self, entry: &Arc<AttemptEntry>) -> Result<(), String> {
        let mut state = entry.state().await;
        let AttemptState::Handoff(handoff) = &mut *state else {
            return Ok(());
        };
        if !handoff.task.is_finished() {
            return Ok(());
        }
        if entry
            .finishing_lease
            .lock()
            .map_err(|_| "adaptive application fence is uncertain")?
            .is_some()
        {
            return Err("adaptive original application fence was already retained".into());
        }
        let cleanup = handoff.task.join().await?;
        let AttemptState::Handoff(handoff) = std::mem::replace(&mut *state, AttemptState::Unknown)
        else {
            unreachable!();
        };
        let HandoffAttempt {
            claim,
            fence,
            application_lease,
            ..
        } = *handoff;
        let mut held = entry
            .finishing_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(application_lease);
        *state = AttemptState::Closing(Box::new(ClosingAttempt {
            cleanup,
            claim,
            fence,
            close: None,
        }));
        Ok(())
    }

    async fn reconcile_adaptive_loser(&self, entry: &Arc<AttemptEntry>) -> Result<(), String> {
        let mut state = entry.state().await;
        let AttemptState::Rich(rich) = &mut *state else {
            return Ok(());
        };
        if rich.adoption.is_some() {
            return Ok(());
        }
        let status = self
            .store
            .adaptive_request_status(&rich.claim.binding.key, &rich.claim.binding.principal)
            .await
            .map_err(|error| format!("reading adaptive loser: {error}"))?
            .ok_or("adaptive request disappeared while owner remained live")?;
        status
            .validate()
            .map_err(|error| format!("invalid adaptive loser status: {error}"))?;
        if status.binding != rich.claim.binding {
            return Err("adaptive loser binding changed".into());
        }
        let attempt = status
            .attempts
            .iter()
            .find(|attempt| attempt.key == rich.claim.key)
            .ok_or("adaptive loser attempt disappeared")?;
        if attempt.phase == AttemptPhase::Provisional
            && rich.prepared.is_some()
            && entry
                .coordinator
                .lock()
                .map_err(|_| "adaptive coordinator hold is uncertain")?
                .is_none()
        {
            let original = rich.prepared.as_ref().expect("retained Prepared operation");
            match self.store.record_adaptive_prepared(original).await {
                Ok(AttemptMutation::Applied(committed) | AttemptMutation::Replay(committed))
                    if committed.binding == original.binding
                        && committed.attempts.iter().any(|row| {
                            row.key == original.key
                                && row.phase == AttemptPhase::Prepared
                                && row.worker.as_ref() == Some(&original.worker)
                        }) =>
                {
                    return Ok(())
                }
                Ok(AttemptMutation::Refused(_)) => {
                    // A known refusal has no committed Prepared operation to
                    // replay. The next pass fences the still-provisional row.
                    rich.prepared = None;
                    return Ok(());
                }
                Ok(_) => {
                    return Err(
                        "adaptive Prepared replay conflicts with its original Store row".into(),
                    )
                }
                Err(error) => {
                    return Err(format!(
                        "adaptive Prepared is unknown; original operation remains held: {error}"
                    ))
                }
            }
        }
        // A finite R may have been admitted before Store Prepared. Once its
        // original coordinator has joined, fence the provisional row with a
        // retained operation before closing the original native owner.
        if rich.prepared.is_none() && attempt.phase == AttemptPhase::Provisional {
            if entry
                .coordinator
                .lock()
                .map_err(|_| "adaptive coordinator hold is uncertain")?
                .is_some()
            {
                return Ok(());
            }
            if rich.fence.is_none() {
                rich.fence = Some(
                    plurx_core::store::adaptive_resource_attempts::AttemptFence {
                        context: MutationContext {
                            operation_id: Uuid::new_v4(),
                            expected_request_sequence: status.sequence,
                            expected_attempt_sequence: attempt.sequence,
                            now_ms: crate::media_sessions::unix_ms(),
                        },
                        binding: rich.claim.binding.clone(),
                        key: rich.claim.key.clone(),
                        cancel_request: false,
                    },
                );
            }
            let original = rich.fence.as_ref().expect("retained provisional Fence");
            match self.store.fence_adaptive_attempt(original).await {
                Ok(AttemptMutation::Applied(committed) | AttemptMutation::Replay(committed))
                    if committed.binding == original.binding
                        && committed.attempts.iter().any(|row| {
                            row.key == original.key
                                && row.phase == AttemptPhase::Cleanup
                                && row.last_operation_id == original.context.operation_id
                        }) => {}
                Ok(_) => return Err("adaptive provisional Fence was refused; original owner remains held".into()),
                Err(error) => return Err(format!("adaptive provisional Fence is unknown; original operation remains held: {error}")),
            }
            drop(state);
            return self.handoff_adaptive_loser(entry).await;
        }
        // The origin may disappear after Prepared or Winner without sending
        // Cancel. The immutable original deadline is already in the charged
        // Claim. On the existing reap cadence, retain one exact Fence
        // operation and hand off the same native owner only after the Store
        // positively records Cleanup. No new timer or route is minted.
        if matches!(attempt.phase, AttemptPhase::Prepared | AttemptPhase::Winner)
            && crate::media_sessions::unix_ms() >= rich.claim.binding.original_deadline_ms
        {
            if rich.fence.is_none() {
                rich.fence = Some(
                    plurx_core::store::adaptive_resource_attempts::AttemptFence {
                        context: MutationContext {
                            operation_id: Uuid::new_v4(),
                            expected_request_sequence: status.sequence,
                            expected_attempt_sequence: attempt.sequence,
                            now_ms: crate::media_sessions::unix_ms(),
                        },
                        binding: rich.claim.binding.clone(),
                        key: rich.claim.key.clone(),
                        cancel_request: false,
                    },
                );
            }
            let original = rich
                .fence
                .as_ref()
                .expect("retained expired adaptive Fence");
            match self.store.fence_adaptive_attempt(original).await {
                Ok(AttemptMutation::Applied(committed) | AttemptMutation::Replay(committed))
                    if committed.binding == original.binding
                        && committed.attempts.iter().any(|row| {
                            row.key == original.key
                                && row.phase == AttemptPhase::Cleanup
                                && row.last_operation_id == original.context.operation_id
                        }) => {}
                Ok(_) => {
                    return Err(
                        "expired adaptive Fence was refused; original owner remains held".into(),
                    )
                }
                Err(error) => {
                    return Err(format!(
                    "expired adaptive Fence is unknown; original operation remains held: {error}"
                ))
                }
            }
            drop(state);
            return self.handoff_adaptive_loser(entry).await;
        }
        let should_close = attempt.phase == AttemptPhase::Losing
            || attempt.phase == AttemptPhase::Cleanup
                && rich
                    .fence
                    .as_ref()
                    .is_none_or(|fence| attempt.last_operation_id == fence.context.operation_id);
        drop(state);
        if should_close {
            self.handoff_adaptive_loser(entry).await?;
        }
        Ok(())
    }

    /// A retry never re-enters P/R: it replays the original Prepared UUID and
    /// payload from the registered rich owner. `None` means its original
    /// coordinator has not reached Store Prepared yet.
    pub(crate) async fn replay_adaptive_prepared(
        &self,
        attempt_id: Uuid,
    ) -> Option<Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String>> {
        let entry = self.adaptive_attempts.get(attempt_id)?;
        let state = entry.state().await;
        let AttemptState::Rich(rich) = &*state else {
            return None;
        };
        let prepared = rich.prepared.as_ref()?;
        let preview_json = match &rich.preview_json {
            Some(preview) => preview.clone(),
            None => return Some(Err("adaptive original preview is absent".into())),
        };
        let result = self.store.record_adaptive_prepared(prepared).await;
        Some(match result {
            Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status))
                if status.binding == prepared.binding
                    && status.attempts.iter().any(|attempt| {
                        attempt.key == prepared.key
                            && attempt.worker.as_ref() == Some(&prepared.worker)
                            && attempt.phase == AttemptPhase::Prepared
                    }) =>
            {
                Ok(crate::media_sessions::AdaptiveAttemptPreparedV1 {
                    version: crate::media_sessions::ADAPTIVE_ATTEMPT_VERSION,
                    worker: prepared.worker.clone(),
                    selected_recipe_json: rich.selected_recipe_json.clone(),
                    preview_json,
                })
            }
            Ok(_) => Err("adaptive Prepared replay conflicts with the Store row".into()),
            Err(error) => Err(format!("adaptive Prepared replay remains unknown: {error}")),
        })
    }

    /// Install the charged original owner and its immutable Store Prepared
    /// operation before the first Store await. A lost peer reply replays that
    /// exact operation from this same entry; it never mints a replacement.
    #[expect(
        clippy::too_many_arguments,
        reason = "This atomic handoff binds the original entry, claim, recipe, settings, deadline and charged size before Store await; no replacement owner or heap argument bundle is introduced."
    )]
    pub(super) async fn record_adaptive_prepared(
        &self,
        entry: &Arc<AttemptEntry>,
        claim: AttemptClaim,
        selected_recipe_json: String,
        mut pending: PreparedAttemptRendition,
        settings: crate::vodserve::VodSettings,
        deadline: std::time::Instant,
        projected_application_bytes: usize,
    ) -> Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String> {
        let actual = pending
            .requested_heap_bytes()
            .and_then(|bytes| bytes.checked_add(selected_recipe_json.capacity()))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<RichAttempt>()))
            .ok_or("adaptive owner capacity overflow")?;
        if actual > projected_application_bytes {
            return Err("adaptive owner exceeds its charged application partition".into());
        }
        let application_lease = pending.take_application_lease()?;
        entry.charged_control_frame_bytes.store(
            crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES,
            std::sync::atomic::Ordering::Release,
        );
        let mut state = entry.state().await;
        if !matches!(*state, AttemptState::Reserved) {
            return Err("adaptive original owner was already installed".into());
        }
        *state = AttemptState::Rich(Box::new(RichAttempt {
            pending,
            settings,
            selected_recipe_json,
            preview_json: None,
            claim,
            prepared: None,
            fence: None,
            adoption: None,
            application_lease,
        }));
        let AttemptState::Rich(rich) = &mut *state else {
            unreachable!()
        };
        let preview = rich.pending.planned_preview(
            &rich.claim.key.session_id.to_string(),
            entry.serving_generation(),
        )?;
        rich.preview_json = Some(preview.bounded_adaptive_preview_json()?);
        if std::time::Instant::now() >= deadline
            || crate::media_sessions::unix_ms() >= rich.claim.binding.original_deadline_ms
            || entry.attempt_id() != rich.claim.key.attempt_id
            || entry.worker_instance_id() != rich.claim.worker_instance_id
            || entry.serving_generation() != rich.claim.serving_generation
        {
            return Err("adaptive preparation lost its original deadline or worker".into());
        }
        let status = self
            .store
            .adaptive_request_status(&rich.claim.binding.key, &rich.claim.binding.principal)
            .await
            .map_err(|error| format!("reading adaptive attempt before Prepared: {error}"))?
            .ok_or("adaptive claim disappeared before Prepared")?;
        status
            .validate()
            .map_err(|error| format!("invalid adaptive status: {error}"))?;
        let attempt = status
            .attempts
            .iter()
            .find(|attempt| attempt.key == rich.claim.key)
            .ok_or("adaptive attempt disappeared before Prepared")?;
        if status.binding != rich.claim.binding
            || status.disposition != RequestDisposition::Open
            || attempt.phase != AttemptPhase::Provisional
            || attempt.worker_instance_id != rich.claim.worker_instance_id
        {
            return Err("adaptive attempt changed before Prepared".into());
        }
        let (prepared_handle_digest, output_digest) =
            rich.pending.prepare_worker_binding(deadline).await?;
        let recipe_digest = selected_recipe_digest(&rich.selected_recipe_json)
            .map_err(|error| format!("adaptive route digest: {error}"))?;
        let worker = WorkerBinding {
            node_id: rich.claim.key.worker_node_id.clone(),
            worker_instance_id: rich.claim.worker_instance_id,
            serving_generation: rich.claim.serving_generation,
            planning_generation: rich.claim.binding.planning_generation,
            planning_source_digest: rich.claim.binding.planning_source_digest.clone(),
            selected_recipe_digest: recipe_digest,
            prepared_handle_digest,
            output_digest,
            prepared_expires_at_ms: rich.claim.binding.original_deadline_ms,
        };
        let prepared = PreparedAttempt {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: status.sequence,
                expected_attempt_sequence: attempt.sequence,
                now_ms: crate::media_sessions::unix_ms(),
            },
            binding: rich.claim.binding.clone(),
            key: rich.claim.key.clone(),
            worker: worker.clone(),
        };
        prepared
            .worker
            .validate(&prepared.binding, &prepared.key)
            .map_err(|error| format!("invalid adaptive Prepared: {error}"))?;
        prepared
            .context
            .validate()
            .map_err(|error| format!("invalid adaptive Prepared context: {error}"))?;
        rich.prepared = Some(prepared);
        let AttemptState::Rich(rich) = &*state else {
            unreachable!()
        };
        let original = rich
            .prepared
            .as_ref()
            .expect("immutable Prepared operation");
        match self.store.record_adaptive_prepared(original).await {
            Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status))
                if status.binding == original.binding
                    && status.attempts.iter().any(|attempt| {
                        attempt.key == original.key
                            && attempt.worker.as_ref() == Some(&worker)
                            && attempt.phase == AttemptPhase::Prepared
                    }) =>
            {
                Ok(crate::media_sessions::AdaptiveAttemptPreparedV1 {
                    version: crate::media_sessions::ADAPTIVE_ATTEMPT_VERSION,
                    worker,
                    selected_recipe_json: rich.selected_recipe_json.clone(),
                    preview_json: rich
                        .preview_json
                        .clone()
                        .ok_or("adaptive original preview is absent")?,
                })
            }
            Ok(_) => Err("adaptive Prepared was refused or its committed row differs".into()),
            Err(error) => Err(format!("adaptive Prepared outcome is unknown: {error}")),
        }
    }

    /// Store Claim is a committed, blocked starting route. A peer DTO is only
    /// an assertion until both the exact row and route are read back here.
    pub(crate) async fn verify_adaptive_claim(
        &self,
        claim: &AttemptClaim,
    ) -> Result<(), &'static str> {
        claim.validate().map_err(|_| "invalid adaptive claim")?;
        let status = self
            .store
            .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
            .await
            .map_err(|_| "adaptive Store status is unavailable")?
            .ok_or("adaptive claim is absent")?;
        status
            .validate()
            .map_err(|_| "adaptive Store status is invalid")?;
        if status.binding != claim.binding || status.disposition != RequestDisposition::Open {
            return Err("adaptive claim binding or disposition changed");
        }
        let attempt = status
            .attempts
            .iter()
            .find(|attempt| attempt.key == claim.key)
            .ok_or("adaptive attempt is absent")?;
        if attempt.phase != AttemptPhase::Provisional
            || attempt.worker_instance_id != claim.worker_instance_id
            || attempt.serving_generation != claim.serving_generation
            || attempt.domains != claim.domains
        {
            return Err("adaptive attempt is no longer provisional");
        }
        let route = self
            .store
            .media_session_route_by_incarnation(&claim.key.route_incarnation_id.to_string())
            .await
            .map_err(|_| "adaptive starting route is unavailable")?
            .ok_or("adaptive starting route is absent")?;
        if route.session_id != claim.key.session_id.to_string()
            || route.principal != claim.binding.principal
            || route.owner_node_id != claim.key.worker_node_id
            || route.owner_epoch != claim.key.owner_epoch
            || route.state != "starting"
            || route.publication_ready_at_ms
                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
            || route.lease_expires_at_ms != claim.binding.original_deadline_ms
        {
            return Err("adaptive starting route differs from Store Claim");
        }
        Ok(())
    }

    pub(crate) async fn adaptive_attempt_local_status(&self, attempt_id: Uuid) -> &'static str {
        let Some(entry) = self.adaptive_attempts.get(attempt_id) else {
            return "not_registered";
        };
        let state = entry.state().await;
        match &*state {
            AttemptState::Reserved => "reserved",
            AttemptState::PlanningFailed(_) => "planning_cleanup_held",
            AttemptState::Rich(_) => "prepared_owner_held",
            AttemptState::Handoff(_) => "native_cleanup_registering",
            AttemptState::Closing(closing) if closing.cleanup.known_closed() => "native_closed",
            AttemptState::Closing(_) => "closing_owner_held",
            AttemptState::Adopted => "adopted",
            AttemptState::Closed => "closed",
            AttemptState::Unknown => "unknown_held",
        }
    }

    /// Reuse the manager's existing repair cadence. A completed original task
    /// is joined before its entry can be considered terminal; a panic leaves
    /// the exact row visible as Unknown, never as a cleanup receipt.
    pub(super) async fn reconcile_adaptive_attempt_tasks(&self) {
        if let Some(runtime) = self.resource_runtime.as_ref() {
            // The core book holds each original P/M operation independently
            // of a completed Store attempt. Use the existing reap cadence,
            // with one bounded pass and no new watchdog.
            runtime
                .maintain_planning_cleanup(
                    std::time::Instant::now() + std::time::Duration::from_millis(500),
                )
                .await;
            if let Err(error) = runtime
                .maintain_failed_copy_admissions(
                    std::time::Instant::now() + std::time::Duration::from_millis(500),
                )
                .await
            {
                tracing::warn!(%error,
                    "original failed copy admission remains in the core cleanup book");
            }
        }
        for entry in self.adaptive_attempts.entries().into_iter().flatten() {
            if let Some(task) = entry.take_finished_coordinator() {
                if let Err(error) = task.await {
                    tracing::error!(attempt_id = %entry.attempt_id(), %error,
                        "adaptive attempt coordinator ended without a known disposition");
                    let mut state = entry.state().await;
                    if matches!(*state, AttemptState::Reserved) {
                        *state = AttemptState::Unknown;
                    }
                }
            }
            if let Some(task) = entry.take_finished_control() {
                if let Err(error) = task.await {
                    tracing::error!(attempt_id = %entry.attempt_id(), %error,
                        "adaptive control ended without a known disposition");
                }
            }
            if let Err(error) = entry.release_finished_application() {
                tracing::warn!(attempt_id = %entry.attempt_id(), error,
                    "adaptive application fence remains held");
            }
            if let Err(error) = self.reconcile_adaptive_loser(&entry).await {
                tracing::warn!(attempt_id = %entry.attempt_id(), %error,
                    "adaptive loser retains its original finite owner");
            }
            if let Err(error) = self.reconcile_adaptive_handoff(&entry).await {
                tracing::warn!(attempt_id = %entry.attempt_id(), %error,
                    "adaptive cleanup registration remains held");
            }
            if let Err(error) = self.finish_failed_adaptive_planning(&entry).await {
                tracing::warn!(attempt_id = %entry.attempt_id(), %error,
                    "adaptive failed planning retains its exact P/M and Store operations");
            }
            match self.finish_adaptive_close(&entry).await {
                Ok(_) => {
                    let terminal = matches!(
                        *entry.state().await,
                        AttemptState::Closed | AttemptState::Adopted
                    );
                    if terminal && entry.no_original_tasks_or_application() {
                        if let Err(error) = self.adaptive_attempts.remove_terminal(&entry) {
                            tracing::warn!(attempt_id = %entry.attempt_id(), error,
                                "terminal adaptive attempt remains in registry");
                        }
                    }
                }
                Err(error) => tracing::warn!(attempt_id = %entry.attempt_id(), %error,
                    "adaptive attempt retains its original cleanup operation"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registered_attempt_starts_once_and_replay_selects_same_owner() {
        let registry = AdaptiveAttemptRegistry::default();
        let attempt = Uuid::new_v4();
        let request = Uuid::new_v4();
        let worker = Uuid::new_v4();
        let entry = registry
            .reserve(attempt, request, worker, 7)
            .expect("reserve");
        let replay = registry
            .reserve(attempt, request, worker, 7)
            .expect("replay");
        assert!(Arc::ptr_eq(&entry, &replay));
        assert!(registry
            .reserve(attempt, request, Uuid::new_v4(), 7)
            .is_err());
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        entry
            .spawn_coordinator(
                async move {
                    let _ = entered_tx.send(());
                },
                frame_tx,
            )
            .expect("retain original coordinator before entry");
        assert!(frame_rx.await.expect("measured original frame") > 0);
        entered_rx.await.expect("entered after registration");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(task) = entry.take_finished_coordinator() {
                    break tokio::time::timeout(std::time::Duration::from_secs(1), task)
                        .await
                        .expect("original task join deadline")
                        .expect("original task joined");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original task finished");
        assert!(entry.no_original_tasks_or_application());
    }
}
