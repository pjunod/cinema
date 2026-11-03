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

/// Original registered reply only; never a newly launched coordinator.
#[expect(
    clippy::large_enum_variant,
    reason = "Keep the same bounded prepared acknowledgment inline during replay; no new Box allocation beside the original registered owner."
)]
pub(crate) enum AdaptivePrepareReplay {
    Prepared(crate::media_sessions::AdaptiveAttemptPreparedV1),
    Pending,
}

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

/// Original remote registration disposition; the ACK contains no native or
/// capacity authority and is sent only after original P/M is retained.
pub(crate) enum AdaptiveRemoteRegistrationStart {
    Registered(crate::media_sessions::AdaptiveAttemptRegisteredV2),
    Pending,
    Started {
        reply: tokio::sync::oneshot::Receiver<
            Result<crate::media_sessions::AdaptiveAttemptRegisteredV2, String>,
        >,
        deadline: std::time::Instant,
    },
}
/// A refused original grant. Only cluster-clock evidence is retryable: the
/// registration stays ungranted, so the coordinator's Fence reaches this
/// worker's original coordinator, which closes its own P/M.
#[derive(Debug)]
pub(crate) enum RemoteGrantError {
    ClusterClock(plurx_core::cluster::clock::ClockRefusal),
    Refused(String),
}
impl From<String> for RemoteGrantError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}
impl From<&str> for RemoteGrantError {
    fn from(reason: &str) -> Self {
        Self::Refused(reason.to_owned())
    }
}
impl std::fmt::Display for RemoteGrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClusterClock(reason) => write!(
                f,
                "cluster_clock: the cluster clock cannot prove the original grant ({reason})"
            ),
            Self::Refused(reason) => f.write_str(reason),
        }
    }
}
struct RemoteRegistration {
    claim: AttemptClaim,
    nonce: Uuid,
    anchor: plurx_core::cluster::clock::ContinuousClockCapture,
    grant: Option<tokio::sync::oneshot::Sender<plurx_core::cluster::clock::ContinuousDeadline>>,
    ready: bool,
    granted_ms: Option<u32>,
    deadline: Option<plurx_core::cluster::clock::ContinuousDeadline>,
    fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
}
impl RemoteRegistration {
    fn apply_original_grant(
        &mut self,
        claim: &AttemptClaim,
        nonce: Uuid,
        allowance_ms: u32,
    ) -> Result<(), RemoteGrantError> {
        if nonce.is_nil() || allowance_ms == 0 || allowance_ms > 9_750 {
            return Err("deadline_budget: invalid original grant".into());
        }
        if self.claim != *claim || self.nonce != nonce || !self.ready || self.fence.is_some() {
            return Err("remote grant differs from the original live registration".into());
        }
        if let Some(original) = self.granted_ms {
            if original != allowance_ms {
                return Err("remote grant changed its original allowance".into());
            }
            // Exact replay preserves W0/cutoff, even after expiry. It can
            // only replay the already owned original task/result.
        } else {
            // Grant-time cluster policy: the W0 admission evidence must be
            // unchanged and the CURRENT clock policy must admit. After this
            // the deadline closes on local evidence only. A refusal leaves
            // the grant unsent, so the original registration stays fenceable.
            let deadline = self
                .anchor
                .grant_deadline_after(std::time::Duration::from_millis(u64::from(allowance_ms)))
                .map_err(RemoteGrantError::ClusterClock)?;
            let sender = self.grant.take().ok_or("original grant sender is absent")?;
            self.granted_ms = Some(allowance_ms);
            self.deadline = Some(deadline.clone());
            sender
                .send(deadline)
                .map_err(|_| "original grant receiver already closed")?;
        }
        Ok(())
    }
}

pub(super) struct RemoteGrantWait {
    reply: tokio::sync::oneshot::Sender<
        Result<crate::media_sessions::AdaptiveAttemptRegisteredV2, String>,
    >,
    grant: tokio::sync::oneshot::Receiver<plurx_core::cluster::clock::ContinuousDeadline>,
}
/// The P/M factory returns exactly one local selected recipe, with its
/// original application fence retained through actual task destruction.
pub(crate) struct PreparedAdaptiveWorkerRecipe {
    pub(crate) prepared: crate::vodserve::PreparedAdaptiveRecipe,
    pub(crate) settings: crate::vodserve::VodSettings,
    pub(crate) planning_application_limit_bytes: usize,
}

fn checked_caps(capacities: impl IntoIterator<Item = usize>) -> Option<usize> {
    capacities.into_iter().try_fold(0usize, usize::checked_add)
}

/// Requested heap a selected P owner retains for its planning snapshot. A
/// request whose candidate context already holds this SAME `Arc` counts it
/// through `selected_finite_requested_heap_bytes`; charging it again would
/// claim capacity no owner retains. The selected-static worker and its
/// fixture share this rule so both charge the same original obligation.
pub(super) fn retained_planning_snapshot_bytes(
    request: &super::SessionRequest,
    snapshot: &Arc<plurx_core::store::PlaybackPlanningSnapshot>,
) -> Option<usize> {
    let held_by_request = request.candidate_context.as_ref().is_some_and(|context| {
        context
            .planning_snapshot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, snapshot))
    });
    if held_by_request {
        return Some(0);
    }
    (std::mem::size_of::<plurx_core::store::PlaybackPlanningSnapshot>()
        + 2 * std::mem::size_of::<usize>())
    .checked_add(planning_snapshot_requested_heap_bytes(snapshot)?)
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

#[cfg(test)]
struct AdoptCommitPause {
    committed: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

pub(super) struct AttemptEntry {
    #[cfg(test)]
    adopt_commit_pause: Mutex<Option<AdoptCommitPause>>,
    remote_changed: tokio::sync::Notify,
    remote_registration: Mutex<Option<RemoteRegistration>>,
    coordinator_frame_bytes: std::sync::atomic::AtomicUsize,
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
    /// A local create's original continuous preparation cutoff, fixed when
    /// the entry is first reserved inside its create scope; a replay selects
    /// the existing entry and never replaces it. A remote registration keeps
    /// its accepted grant in `remote_registration` instead. Stored inline, so
    /// the `size_of::<AttemptEntry>()` term of every precharged application
    /// total already covers it; the clock guard it shares is node-owned.
    local_cutoff: Option<plurx_core::cluster::clock::ContinuousDeadline>,
    /// The SAME local create's full-allowance cutoff, fixed with
    /// `local_cutoff`. It bounds only Adopt, the activation step the create
    /// keeps its last allowance for; P and R entry stay on `local_cutoff`.
    local_activation_cutoff: Option<plurx_core::cluster::clock::ContinuousDeadline>,
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
    // Original attempted cancellation payload is retained for its charged lifetime;
    // it cannot veto a different committed cleanup-authorizing operation.
    pub(super) _fence: Option<plurx_core::store::adaptive_resource_attempts::AttemptFence>,
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
        local_cutoff: Option<plurx_core::cluster::clock::ContinuousDeadline>,
        local_activation_cutoff: Option<plurx_core::cluster::clock::ContinuousDeadline>,
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
            #[cfg(test)]
            adopt_commit_pause: Mutex::new(None),
            remote_changed: tokio::sync::Notify::new(),
            remote_registration: Mutex::new(None),
            coordinator_frame_bytes: std::sync::atomic::AtomicUsize::new(0),
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
            local_cutoff,
            local_activation_cutoff,
        });
        *vacant = Some(Arc::clone(&entry));
        Ok(entry)
    }

    /// Reserve a fresh LOCAL attempt inside its create scope. Only the actual
    /// create scope proves the remaining monotonic preparation allowance and
    /// its paired continuous cutoff; both are read here for the entry being
    /// reserved, so the local cutoff is fixed exactly once with the slot.
    fn reserve_local(
        &self,
        claim: &AttemptClaim,
        deadline: std::time::Instant,
    ) -> Result<(Arc<AttemptEntry>, std::time::Instant), String> {
        let original_cutoff = crate::media_pool::create_preparation_deadline()
            .ok_or("insufficient_evidence: original local create deadline is absent")?
            .into_std();
        let deadline = deadline.min(original_cutoff);
        if std::time::Instant::now() >= deadline {
            return Err("deadline_budget: original local preparation allowance expired".into());
        }
        let entry = self.reserve(
            claim.key.attempt_id,
            claim.key.request.request_incarnation_id,
            claim.worker_instance_id,
            claim.serving_generation,
            crate::media_pool::create_continuous_entry_deadline(),
            crate::media_pool::create_continuous_activation_deadline(),
        )?;
        Ok((entry, deadline))
    }

    pub(super) fn get(&self, attempt_id: Uuid) -> Option<Arc<AttemptEntry>> {
        self.slots
            .lock()
            .ok()?
            .iter()
            .flatten()
            .find_map(|entry| (entry.attempt_id == attempt_id).then(|| Arc::clone(entry)))
    }

    fn drain_registered(&self) -> Result<usize, crate::serving_fence::OwnedWorkUnknown> {
        let slots = self
            .slots
            .try_lock()
            .map_err(|_| crate::serving_fence::OwnedWorkUnknown::AdaptiveRegistryUnavailable)?;
        // Even terminal entries stay blockers until original task joins and
        // application destruction permit exact registry removal.
        Ok(slots.iter().flatten().count())
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
    fn remote_requested_bytes(&self) -> Option<usize> {
        let held = self.remote_registration.lock().ok()?;
        let Some(remote) = held.as_ref() else {
            return Some(0);
        };
        claim_requested_heap_bytes(&remote.claim)?
            // Replaces the local frame-channel value, not a new task/holder.
            // Incremental value/alignment only; no Tokio header/native bound.
            .checked_add(std::mem::size_of::<
                Option<plurx_core::cluster::clock::ContinuousDeadline>,
            >())?
            .checked_add(
                std::mem::align_of::<Option<plurx_core::cluster::clock::ContinuousDeadline>>()
                    .checked_sub(1)?,
            )?
            .checked_add(crate::media_sessions::MAX_CONTROL_REQUEST_BYTES)
    }
    fn remote_reply(
        &self,
        claim: &AttemptClaim,
    ) -> Result<Option<crate::media_sessions::AdaptiveAttemptRegisteredV2>, String> {
        let held = self
            .remote_registration
            .lock()
            .map_err(|_| "remote original registration is uncertain")?;
        let remote = held
            .as_ref()
            .ok_or("original attempt has no causal registration")?;
        if &remote.claim != claim {
            return Err("remote registration replay changed its original Claim".into());
        }
        Ok(remote
            .ready
            .then_some(crate::media_sessions::AdaptiveAttemptRegisteredV2 {
                version: crate::media_sessions::ADAPTIVE_CAUSAL_PREPARE_VERSION,
                nonce: remote.nonce,
                attempt_id: self.attempt_id,
                worker_instance_id: self.worker_instance_id,
                serving_generation: self.serving_generation,
            }))
    }
    fn remote_deadline(
        &self,
    ) -> Result<Option<plurx_core::cluster::clock::ContinuousDeadline>, String> {
        self.remote_registration
            .lock()
            .map(|held| held.as_ref().and_then(|remote| remote.deadline.clone()))
            .map_err(|_| "remote original deadline is uncertain".into())
    }
    /// The original continuous entry condition this attempt carries into P
    /// and R: the accepted remote grant, else the local create's own cutoff.
    fn continuous_entry_deadline(
        &self,
    ) -> Result<Option<plurx_core::cluster::clock::ContinuousDeadline>, String> {
        Ok(self
            .remote_deadline()?
            .or_else(|| self.local_cutoff.clone()))
    }
    /// The original continuous bound on this attempt's Adopt: the accepted
    /// remote grant, else the local create's full-allowance cutoff (its
    /// prepare cutoff only if that pairing is somehow absent).
    fn continuous_activation_deadline(
        &self,
    ) -> Result<Option<plurx_core::cluster::clock::ContinuousDeadline>, String> {
        Ok(self.remote_deadline()?.or_else(|| {
            self.local_activation_cutoff
                .clone()
                .or_else(|| self.local_cutoff.clone())
        }))
    }
    fn remote_failure_fence(
        &self,
    ) -> Option<plurx_core::store::adaptive_resource_attempts::AttemptFence> {
        self.remote_registration.lock().ok()?.as_mut()?.fence.take()
    }
    // SAME original coordinator/start latch. Inline frame publication lets
    // the existing frame-channel allocation carry the original grant instead.
    fn spawn_remote_coordinator<F>(self: &Arc<Self>, future: F) -> Result<(), &'static str>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let original = async move {
            if start_rx.await.is_ok() {
                future.await;
            }
        };
        self.coordinator_frame_bytes.store(
            std::mem::size_of_val(&original),
            std::sync::atomic::Ordering::Release,
        );
        let task = tokio::spawn(original);
        if self.install_coordinator(task).is_err() {
            return Err("adaptive attempt already has an original coordinator");
        }
        let _ = start_tx.send(());
        Ok(())
    }
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
    pub(crate) fn adaptive_attempt_drain_count(
        &self,
    ) -> Result<usize, crate::serving_fence::OwnedWorkUnknown> {
        self.adaptive_attempts.drain_registered()
    }

    async fn admit_adaptive_fixed_probe(
        &self,
        entry: &Arc<AttemptEntry>,
        claim: &AttemptClaim,
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        application_metadata_bytes: usize,
        mut deadline: std::time::Instant,
        remote_grant: Option<RemoteGrantWait>,
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
        let application_metadata_bytes = application_metadata_bytes
            .checked_add(
                entry
                    .remote_requested_bytes()
                    .ok_or("remote registration requested capacity overflow")?,
            )
            .ok_or("adaptive planning application capacity overflow")?;
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
        if let Some(wait) = remote_grant {
            // Every refusal here hands the same original P to its cleanup,
            // like the sibling paths below; nothing waits for the reaper.
            let granted = async {
                let reply = {
                    let mut held = entry
                        .remote_registration
                        .lock()
                        .map_err(|_| "original registration is uncertain")?;
                    let remote = held.as_mut().ok_or("original registration is absent")?;
                    remote.ready = true;
                    crate::media_sessions::AdaptiveAttemptRegisteredV2 {
                        version: crate::media_sessions::ADAPTIVE_CAUSAL_PREPARE_VERSION,
                        nonce: remote.nonce,
                        attempt_id: entry.attempt_id,
                        worker_instance_id: entry.worker_instance_id,
                        serving_generation: entry.serving_generation,
                    }
                };
                // ACK only after the SAME complete charged P/M owner and its
                // application lease are retained. No source has been opened.
                let _ = wait.reply.send(Ok(reply));
                let continuous = tokio::time::timeout_at(deadline.into(), wait.grant)
                    .await
                    .map_err(|_| "original remote registration expired without grant")?
                    .map_err(|_| "original remote grant was cancelled")?;
                continuous.entry_remaining().map_err(|error| {
                    format!("original remote clock before source entry: {error}")
                })?;
                deadline = deadline.min(
                    continuous
                        .monotonic_cutoff()
                        .map_err(|error| error.to_string())?,
                );
                owner
                    .retain_continuous_entry_deadline(continuous)
                    .map_err(|error| {
                        format!("retaining original continuous P entry condition: {error}")
                    })?;
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = granted {
                handoff_planning_cleanup(&mut planning);
                return Err(error);
            }
        } else if let Some(local) = entry.local_cutoff.clone() {
            // A local create retains its own original cutoff on the SAME
            // charged P owner before source entry, exactly as a grant does.
            let retained = local
                .monotonic_cutoff()
                .map_err(|error| format!("original local clock before source entry: {error}"))
                .and_then(|cutoff| {
                    deadline = deadline.min(cutoff);
                    owner
                        .retain_continuous_entry_deadline(local)
                        .map_err(|error| {
                            format!("retaining original continuous P entry condition: {error}")
                        })
                });
            if let Err(error) = retained {
                handoff_planning_cleanup(&mut planning);
                return Err(error);
            }
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
    #[expect(
        clippy::too_many_arguments,
        reason = "The original worker recipe factory keeps envelope, claim, entry, frame, deadline and the original remote grant in one admitted frame."
    )]
    pub(super) async fn prepare_adaptive_worker_recipe(
        &self,
        _state: &crate::state::AppState,
        envelope: &mut crate::media_sessions::RemoteStartRequest,
        claim: &AttemptClaim,
        entry: &Arc<AttemptEntry>,
        coordinator_frame_bytes: usize,
        deadline: std::time::Instant,
        remote_grant: Option<RemoteGrantWait>,
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
        if envelope.adaptive_root_intent_digest(claim.binding.key.request_incarnation_id)?
            != claim.binding.intent_digest
        {
            return Err("adaptive worker intent digest differs from the original request".into());
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
                remote_grant,
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

    /// Replay only an already registered original. This API cannot reserve
    /// an entry, start a coordinator, or turn a peer timestamp into permission.
    pub(crate) async fn replay_registered_adaptive_prepare(
        &self,
        request: &crate::media_sessions::AdaptiveAttemptPrepareV1,
    ) -> Result<Option<AdaptivePrepareReplay>, String> {
        if !request.is_valid() {
            return Err("invalid original adaptive preparation".into());
        }
        let attempt_id = request.claim.key.attempt_id;
        let Some(existing) = self.adaptive_attempts.get(attempt_id) else {
            return Ok(None);
        };
        if existing.worker_instance_id() != request.claim.worker_instance_id
            || existing.serving_generation() != request.claim.serving_generation
        {
            return Err("adaptive replay names a different original worker".into());
        }
        self.verify_adaptive_claim_for_phases(
            &request.claim,
            &[AttemptPhase::Provisional, AttemptPhase::Prepared],
        )
        .await
        .map_err(str::to_owned)?;
        if let Some(reply) = self.replay_adaptive_prepared(attempt_id).await {
            return reply.map(AdaptivePrepareReplay::Prepared).map(Some);
        }
        Ok(Some(AdaptivePrepareReplay::Pending))
    }

    pub(crate) async fn replay_remote_adaptive_registration(
        &self,
        request: &crate::media_sessions::AdaptiveAttemptPrepareV1,
    ) -> Result<Option<AdaptiveRemoteRegistrationStart>, String> {
        let Some(entry) = self.adaptive_attempts.get(request.claim.key.attempt_id) else {
            return Ok(None);
        };
        self.verify_adaptive_claim_for_phases(
            &request.claim,
            &[AttemptPhase::Provisional, AttemptPhase::Prepared],
        )
        .await
        .map_err(str::to_owned)?;
        Ok(Some(match entry.remote_reply(&request.claim)? {
            Some(reply) => AdaptiveRemoteRegistrationStart::Registered(reply),
            None => AdaptiveRemoteRegistrationStart::Pending,
        }))
    }

    pub(crate) async fn wait_remote_adaptive_prepared(
        &self,
        attempt_id: Uuid,
    ) -> Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String> {
        let entry = self
            .adaptive_attempts
            .get(attempt_id)
            .ok_or("original remote registration is absent")?;
        loop {
            // Register before inspecting the SAME original state. Cancelling
            // this borrowed response waiter never consumes its task/owner.
            let changed = entry.remote_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(reply) = self.replay_adaptive_prepared(attempt_id).await {
                return reply;
            }
            if !matches!(*entry.state().await, AttemptState::Reserved) {
                return Err("original remote preparation ended without Prepared".into());
            }
            let original = entry
                .remote_deadline()?
                .ok_or("original remote grant is absent")?;
            original
                .entry_remaining()
                .map_err(|error| format!("original remote response deadline: {error}"))?;
            tokio::time::timeout_at(
                original
                    .monotonic_cutoff()
                    .map_err(|error| error.to_string())?
                    .into(),
                changed,
            )
            .await
            .map_err(|_| "original remote preparation remains pending at its cutoff")?;
        }
    }

    pub(crate) async fn begin_remote_adaptive_registration(
        self: &Arc<Self>,
        state: crate::state::AppState,
        request: crate::media_sessions::AdaptiveAttemptPrepareV1,
        restart_admission: crate::serving_fence::RestartAdmission,
    ) -> Result<AdaptiveRemoteRegistrationStart, String> {
        self.verify_adaptive_claim(&request.claim)
            .await
            .map_err(str::to_owned)?;
        if let Some(existing) = self.adaptive_attempts.get(request.claim.key.attempt_id) {
            return Ok(match existing.remote_reply(&request.claim)? {
                Some(reply) => AdaptiveRemoteRegistrationStart::Registered(reply),
                None => AdaptiveRemoteRegistrationStart::Pending,
            });
        }
        // Actual private W0 precedes original registration. This cutoff only
        // expires inert CONTROL work; it grants no source/image/process entry.
        let anchor = state
            .membership
            .clock_guard()
            .capture_continuous()
            .map_err(|error| format!("insufficient_evidence: original remote clock: {error}"))?;
        let control = anchor
            .deadline_after(std::time::Duration::from_millis(9_750))
            .map_err(|error| format!("deadline_budget: original registration: {error}"))?;
        let deadline = control
            .monotonic_cutoff()
            .map_err(|error| error.to_string())?;
        let entry = self
            .adaptive_attempts
            .reserve(
                request.claim.key.attempt_id,
                request.claim.key.request.request_incarnation_id,
                request.claim.worker_instance_id,
                request.claim.serving_generation,
                None,
                None,
            )
            .map_err(str::to_owned)?;
        let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        {
            let mut held = entry
                .remote_registration
                .lock()
                .map_err(|_| "remote registration is uncertain")?;
            if held.is_some() {
                return Err(
                    "remote registration already belongs to an original coordinator".into(),
                );
            }
            *held = Some(RemoteRegistration {
                claim: request.claim.clone(),
                nonce: Uuid::new_v4(),
                anchor,
                grant: Some(grant_tx),
                ready: false,
                granted_ms: None,
                deadline: None,
                fence: None,
            });
        }
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        entry
            .spawn_remote_coordinator(async move {
                let _restart_admission = restart_admission;
                let frame = original
                    .coordinator_frame_bytes
                    .load(std::sync::atomic::Ordering::Acquire);
                let result = manager
                    .run_adaptive_prepare_with_frame(
                        state,
                        request,
                        Arc::clone(&original),
                        deadline,
                        frame,
                        Some(RemoteGrantWait {
                            reply: reply_tx,
                            grant: grant_rx,
                        }),
                    )
                    .await;
                if result.is_err() {
                    let mut state = original.state().await;
                    if matches!(*state, AttemptState::Reserved) {
                        let mut held = original
                            .remote_registration
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(remote) = held.as_mut() {
                            // Common preparation has dropped its request clone;
                            // replace that precharged Claim copy in failed state.
                            *state =
                                AttemptState::PlanningFailed(Box::new(PlanningFailedAttempt {
                                    claim: remote.claim.clone(),
                                    cleanup: None,
                                    fence: remote.fence.take(),
                                    close: None,
                                }));
                        }
                    }
                }
                original.remote_changed.notify_waiters();
            })
            .map_err(str::to_owned)?;
        Ok(AdaptiveRemoteRegistrationStart::Started {
            reply: reply_rx,
            deadline,
        })
    }

    pub(crate) async fn grant_remote_adaptive_registration(
        &self,
        claim: &AttemptClaim,
        nonce: Uuid,
        allowance_ms: u32,
    ) -> Result<AdaptivePrepareReplay, RemoteGrantError> {
        if nonce.is_nil() || allowance_ms == 0 || allowance_ms > 9_750 {
            return Err("deadline_budget: invalid original grant".into());
        }
        self.verify_adaptive_claim_for_phases(
            claim,
            &[AttemptPhase::Provisional, AttemptPhase::Prepared],
        )
        .await
        .map_err(str::to_owned)?;
        let entry = self
            .adaptive_attempts
            .get(claim.key.attempt_id)
            .ok_or("remote grant has no original registration")?;
        {
            let mut held = entry
                .remote_registration
                .lock()
                .map_err(|_| "remote registration is uncertain")?;
            let remote = held
                .as_mut()
                .ok_or("remote grant has no original time anchor")?;
            remote.apply_original_grant(claim, nonce, allowance_ms)?;
        }
        if let Some(reply) = self.replay_adaptive_prepared(claim.key.attempt_id).await {
            return reply
                .map(AdaptivePrepareReplay::Prepared)
                .map_err(RemoteGrantError::Refused);
        }
        Ok(AdaptivePrepareReplay::Pending)
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
        // Only the actual local create scope proves its remaining monotonic
        // preparation allowance and continuous cutoff. Preserve registered
        // replay above even after caller cancellation; refuse fresh
        // registration without that scope.
        let (entry, deadline) = self
            .adaptive_attempts
            .reserve_local(&request.claim, deadline)?;
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
        self.run_adaptive_prepare_with_frame(
            state,
            request,
            entry,
            deadline,
            coordinator_frame_bytes,
            None,
        )
        .await
    }
    async fn run_adaptive_prepare_with_frame(
        &self,
        state: crate::state::AppState,
        mut request: crate::media_sessions::AdaptiveAttemptPrepareV1,
        entry: Arc<AttemptEntry>,
        mut deadline: std::time::Instant,
        coordinator_frame_bytes: usize,
        remote_grant: Option<RemoteGrantWait>,
    ) -> Result<crate::media_sessions::AdaptiveAttemptPreparedV1, String> {
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
                &mut request.recipe,
                &request.claim,
                &entry,
                coordinator_frame_bytes,
                deadline,
                remote_grant,
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
                        fence: entry.remote_failure_fence(),
                        close: None,
                    }));
                }
                return Err(error);
            }
        };
        if let Some(original) = entry.continuous_entry_deadline()? {
            original
                .entry_remaining()
                .map_err(|error| format!("original create preparation expired: {error}"))?;
            deadline = deadline.min(
                original
                    .monotonic_cutoff()
                    .map_err(|error| error.to_string())?,
            );
        }
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
                        fence: entry.remote_failure_fence(),
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
        // The SAME original continuous condition P retained (remote grant or
        // local create cutoff) closes every fresh R arm and native entry.
        let continuous_entry_deadline = entry.continuous_entry_deadline()?;
        let built = self
            .vod
            .build_adaptive_recipe(
                prepared,
                &settings,
                requested_application_bytes,
                continuous_entry_deadline,
            )
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
                // No Store mutation or native adoption is polled before the fence.
                let result = restart_guarded_adoption(
                    authority.clone(),
                    manager.adopt_adaptive_attempt(
                        &original,
                        adoption,
                        preview_json,
                        authority,
                        deadline,
                    ),
                )
                .await;
                let _ = sender.send(result);
            })
            .map_err(str::to_owned)?;
        Ok(receiver)
    }

    /// BLOCKED -> 0 of this worker's own committed Adopt, replaying the SAME
    /// retained DTO until the original deadline. An unknown Store outcome is
    /// retried; a definite refusal, lost serving authority or the deadline
    /// leaves this owner held with its Adopt, exactly as a failed attach does.
    async fn publish_adopted_route(
        &self,
        entry: &AttemptEntry,
        adoption: &plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption,
        authority: &crate::serving_fence::ServingAuthority,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        loop {
            if !authority.is_current(entry.serving_generation()) {
                return Err(
                    "adaptive worker lost its serving authority before publication; owner remains held"
                        .into(),
                );
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(
                    "adaptive adopted route publication reached its deadline; owner remains held"
                        .into(),
                );
            }
            match tokio::time::timeout(
                remaining,
                self.store
                    .publish_adaptive_adoption(adoption, crate::media_sessions::unix_ms()),
            )
            .await
            {
                Ok(Ok(Some(outcome)))
                    if outcome.route.incarnation_id == adoption.activation.incarnation_id
                        && outcome.route.owner_node_id == adoption.worker.node_id
                        && outcome.route.owner_epoch == adoption.key.owner_epoch
                        && outcome.route.state == "active"
                        && outcome.route.publication_ready_at_ms == 0 =>
                {
                    return Ok(())
                }
                Ok(Ok(_)) => {
                    return Err(
                        "adaptive adopted route could not be published; owner remains held".into(),
                    )
                }
                Ok(Err(error)) => tracing::debug!(
                    attempt_id = %entry.attempt_id(),
                    %error,
                    "adaptive adopted route publication is unknown; replaying the same Adopt"
                ),
                Err(_) => continue,
            }
            tokio::time::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(100)),
            )
            .await;
        }
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
        {
            let mut held = entry
                .remote_registration
                .lock()
                .map_err(|_| "original remote registration is uncertain")?;
            if let Some(remote) = held.as_mut().filter(|remote| remote.granted_ms.is_none()) {
                if remote.claim.binding != fence.binding
                    || remote.claim.key != fence.key
                    || remote
                        .fence
                        .as_ref()
                        .is_some_and(|original| original != &fence)
                {
                    return Err("remote cancellation differs from the original registration".into());
                }
                remote.fence = Some(fence);
                // Drop ONLY the original unsent grant. The SAME coordinator
                // joins/closes P/M; this response asserts no native extinction.
                remote.grant.take();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let _ = sender.send(Err(
                    "original ungranted registration cleanup remains pending".into(),
                ));
                return Ok(receiver);
            }
        }
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
        let deadline = original_adopt_deadline(entry, deadline)?;
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
        #[cfg(test)]
        {
            let pause = entry
                .adopt_commit_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                let _ = pause.committed.send(());
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), pause.resume)
                    .await
                    .map_err(|_| "adaptive commit pause exceeded the original deadline")?
                    .map_err(|_| "adaptive commit pause lost its original waiter")?;
            }
        }
        let publication_authority = authority.clone();
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
        // Adopt committed the route BLOCKED. This original owner observed
        // that exact commit and attached its Rendition under the same serving
        // admission, so it alone clears the fence; the ingress cannot read
        // `adopted` before the route is publishable.
        self.publish_adopted_route(entry, original, &publication_authority, deadline)
            .await?;
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
        // A known origin/other worker Fence may supersede the operation
        // retained by this original Rich owner. Store validates the exact
        // committed cleanup operation named below. This logical permission
        // never substitutes for the native/application/planning barriers above,
        // and an existing Close retains its original operation unchanged.
        if !matches!(attempt.phase, AttemptPhase::Cleanup | AttemptPhase::Losing) {
            return Err("adaptive close is not a known losing or fenced attempt".into());
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
        // A known origin/other-worker Fence may win this race. Close still
        // names that exact committed operation, which Store validates as a
        // cleanup-authorizing Fence; native/application cleanup stays required.
        if !matches!(attempt.phase, AttemptPhase::Cleanup | AttemptPhase::Losing) {
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
            _fence: fence,
            close: None,
        }));
        Ok(())
    }

    async fn reconcile_adaptive_loser(&self, entry: &Arc<AttemptEntry>) -> Result<(), String> {
        let mut state = entry.state().await;
        let AttemptState::Rich(rich) = &mut *state else {
            return Ok(());
        };
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
        if attempt.worker_instance_id != rich.claim.worker_instance_id
            || attempt.serving_generation != rich.claim.serving_generation
            || attempt.worker_instance_id != entry.worker_instance_id()
            || attempt.serving_generation != entry.serving_generation()
            || attempt.key.attempt_id != entry.attempt_id()
        {
            return Err("adaptive loser names a different original worker".into());
        }
        if status.disposition == RequestDisposition::Adopted
            && status.winner_attempt_id == Some(attempt.key.attempt_id)
            && matches!(attempt.phase, AttemptPhase::Losing | AttemptPhase::Cleanup)
        {
            return Err("adaptive adopted winner cannot become a native loser".into());
        }
        // An attempted Adopt remains uncertain until this SAME durable row
        // positively enters Losing/Cleanup. Store Adopt requires Winner and
        // Store Fence cannot move an adopted request into Cleanup. Its exact
        // guarded phase therefore excludes even a delayed original Adopt CAS.
        // Keep the original adoption payload and all charges until the normal
        // rich-owner handoff and native/application close barriers below.
        if let Some(original) = rich.adoption.as_ref() {
            if original.binding != status.binding
                || original.key != attempt.key
                || attempt.worker.as_ref() != Some(&original.worker)
            {
                return Err("adaptive loser differs from the retained original Adopt".into());
            }
            // An unresolved Adopt otherwise holds the owner until a Fence
            // arrives. If the origin is gone, nothing else will send one, so
            // past the immutable original deadline the retained expired Fence
            // below applies to this SAME row. Store Fence refuses an adopted
            // request, so a delayed committed Adopt still keeps everything.
            let expired_unresolved =
                matches!(attempt.phase, AttemptPhase::Prepared | AttemptPhase::Winner)
                    && crate::media_sessions::unix_ms() >= rich.claim.binding.original_deadline_ms;
            if !expired_unresolved
                && !matches!(attempt.phase, AttemptPhase::Losing | AttemptPhase::Cleanup)
            {
                return Ok(());
            }
        }
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
        // Retaining a locally attempted Fence does not veto a different
        // positively committed origin Fence. Keep this SAME native owner and
        // hand it to existing cleanup; Store Close later names the committed
        // operation only after all physical/application barriers have drained.
        let should_close = matches!(attempt.phase, AttemptPhase::Losing | AttemptPhase::Cleanup);
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
        self.verify_adaptive_claim_for_phases(claim, &[AttemptPhase::Provisional])
            .await
    }

    // Prepared is allowed only by the replay-only API, never fresh native entry.
    async fn verify_adaptive_claim_for_phases(
        &self,
        claim: &AttemptClaim,
        allowed: &[AttemptPhase],
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
        if !allowed.contains(&attempt.phase)
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

/// The original registered control future owns the admission through adoption;
/// dropping its HTTP receiver cannot release it before that future completes.
async fn restart_guarded_adoption<T>(
    authority: crate::serving_fence::ServingAuthority,
    adoption: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let Some(_restart_admission) = authority.try_restart_admission().await else {
        return Err("adaptive worker is preparing for restart".into());
    };
    adoption.await
}

/// Runs only from the existing selected-store/native finite fixture. The
/// factory supplies the SAME original pending owner, never a DTO close proof.
#[cfg(all(test, feature = "cluster-integration-tests"))]
impl super::TranscodeManager {
    #[expect(
        clippy::too_many_arguments,
        reason = "One real foreign-fence fixture covers plain, refused-Adopt and expired refused-Adopt cases over the same original owner."
    )]
    pub(crate) async fn foreign_fence_cleanup_regression<F, Fut>(
        self: &Arc<Self>,
        claim: AttemptClaim,
        selected_recipe_json: String,
        key_capacity: usize,
        settings: crate::vodserve::VodSettings,
        refused_adopt_authority: Option<crate::serving_fence::ServingAuthority>,
        expire_after_refusal: bool,
        make_pending: F,
    ) where
        F: FnOnce(usize) -> Fut + Send + 'static,
        Fut: Future<
                Output = (
                    PreparedAttemptRendition,
                    plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput,
                ),
            > + Send
            + 'static,
    {
        use plurx_core::store::adaptive_resource_attempts::AttemptFence;
        let runtime = self
            .resource_runtime
            .as_ref()
            .expect("actual selected runtime");
        let baseline = runtime.accounting().expect("actual baseline accounting");
        assert!(matches!(
            self.store
                .claim_media_session_request(
                    &claim.binding.principal,
                    &claim.binding.key.request_id,
                    &claim.binding.request_fingerprint,
                    &claim.binding.playback_id,
                    &claim.binding.key.request_incarnation_id.to_string(),
                    claim.context.now_ms,
                    claim.binding.original_deadline_ms,
                )
                .await
                .expect("real original root request"),
            plurx_core::domain::MediaSessionRequestClaim::Acquired { .. }
        ));
        let initial = match self
            .store
            .claim_adaptive_attempt(&claim)
            .await
            .expect("real Claim")
        {
            AttemptMutation::Applied(status) => status,
            other => panic!("fresh original Claim: {other:?}"),
        };
        let row = initial
            .attempts
            .iter()
            .find(|row| row.key == claim.key)
            .expect("original row");
        let stale = AttemptFence {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: initial.sequence,
                expected_attempt_sequence: row.sequence,
                now_ms: crate::media_sessions::unix_ms(),
            },
            binding: claim.binding.clone(),
            key: claim.key.clone(),
            cancel_request: false,
        };
        let entry = self
            .adaptive_attempts
            .reserve(
                claim.key.attempt_id,
                claim.key.request.request_incarnation_id,
                claim.worker_instance_id,
                claim.serving_generation,
                None,
                None,
            )
            .expect("original bounded worker slot");
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        let original_claim = claim.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        entry
            .spawn_coordinator(
                async move {
                    let actual_frame = frame_rx.await.expect("same registered frame measurement");
                    let application_bytes = projected_application_owner_bytes(
                        &original_claim,
                        &selected_recipe_json,
                        key_capacity,
                        actual_frame,
                        crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES,
                    )
                    .expect("actual recipe and coordinator application projection");
                    let (pending, output_alias) = make_pending(application_bytes).await;
                    manager
                        .record_adaptive_prepared(
                            &original,
                            original_claim,
                            selected_recipe_json,
                            pending,
                            settings,
                            deadline,
                            application_bytes,
                        )
                        .await
                        .expect("same actual pending owner reaches Store Prepared");
                    assert!(
                        prepared_tx.send(output_alias).is_ok(),
                        "real original output alias"
                    );
                    release_rx
                        .await
                        .expect("release same original coordinator frame");
                },
                frame_tx,
            )
            .expect("retain original coordinator before entry");
        let escaped = prepared_rx.await.expect("actual original Prepared");
        let refused_adopt = if let Some(authority) = refused_adopt_authority {
            assert!(authority.is_current(entry.serving_generation()));
            let (adoption, preview_json) = {
                let state = entry.state().await;
                let AttemptState::Rich(rich) = &*state else {
                    panic!("actual original finite owner is prepared")
                };
                let prepared = rich.prepared.as_ref().expect("actual original Prepared");
                let preview_json = rich.preview_json.clone().expect("actual original preview");
                let preview: crate::media_sessions::RemoteStartResponse =
                    serde_json::from_str(&preview_json).expect("same actual prepared preview");
                let mut public = serde_json::to_value(&preview).expect("actual preview JSON");
                public["height"] = serde_json::json!(preview.target_height);
                public["media_origin_ms"] =
                    serde_json::json!((preview.media_origin_seconds * 1_000.0).round() as i64);
                public["delivered_audio"] =
                    serde_json::to_value(&preview.audio_delivery).expect("actual audio delivery");
                public["ladder"] = serde_json::json!([]);
                let public: crate::http::hls::StartResponse =
                    serde_json::from_value(public).expect("real public activation response shape");
                let status = self
                    .store
                    .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
                    .await
                    .expect("actual Prepared status")
                    .expect("same original request");
                let row = status
                    .attempts
                    .iter()
                    .find(|row| row.key == claim.key)
                    .expect("same original prepared row");
                assert_eq!(row.phase, AttemptPhase::Prepared);
                assert_eq!(status.winner_attempt_id, None);
                let now_ms = crate::media_sessions::unix_ms();
                let adoption = plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption {
                    context: MutationContext {
                        operation_id: Uuid::new_v4(),
                        expected_request_sequence: status.sequence,
                        expected_attempt_sequence: row.sequence,
                        now_ms,
                    },
                    binding: claim.binding.clone(),
                    key: claim.key.clone(),
                    worker: prepared.worker.clone(),
                    activation: plurx_core::domain::MediaSessionActivation {
                        incarnation_id: claim.key.route_incarnation_id.to_string(),
                        session_id: claim.key.session_id.to_string(),
                        principal: claim.binding.principal.clone(),
                        playback_id: claim.binding.playback_id.clone(),
                        recovery_epoch: claim.binding.recovery_epoch.to_string(),
                        expected_predecessor_incarnation_id: claim
                            .binding
                            .expected_predecessor_incarnation_id
                            .map(|id| id.to_string()),
                        fence_predecessor: true,
                        request_id: Some(claim.binding.key.request_id.clone()),
                        request_fingerprint: claim.binding.request_fingerprint.clone(),
                        owner_node_id: claim.key.worker_node_id.clone(),
                        recipe_json: rich.selected_recipe_json.clone(),
                        response_json: serde_json::to_string(&public)
                            .expect("canonical public response"),
                        publication_ready_at_ms:
                            plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                        media_origin_ms: (preview.media_origin_seconds * 1_000.0).round() as i64,
                        now_ms,
                        lease_expires_at_ms: claim.binding.original_deadline_ms,
                        expected_desired_revision: claim.binding.expected_desired_revision,
                    },
                };
                (adoption, preview_json)
            };
            let before = runtime.accounting().expect("actual held original R");
            let receiver = self
                .begin_adaptive_adopt(adoption.clone(), preview_json, authority, deadline)
                .expect("same registered original Adopt control");
            assert_eq!(
                receiver
                    .await
                    .expect("actual original Adopt reply")
                    .expect_err(
                        "actual Store must refuse Adopt before Winner, not fabricate activation"
                    ),
                "adaptive Adopt was refused; owner remains held"
            );
            // The reply is not a join. Positively join that SAME control frame
            // before asking the bounded registry for another original control.
            tokio::time::timeout_at(deadline.into(), async {
                loop {
                    if let Some(task) = entry.take_finished_control() {
                        task.await.expect("same original Adopt control joined");
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("original Adopt control join deadline");
            self.reconcile_adaptive_loser(&entry)
                .await
                .expect("nonterminal original Adopt still remains held");
            {
                let state = entry.state().await;
                let AttemptState::Rich(rich) = &*state else {
                    panic!("Store refusal alone cannot close the original owner")
                };
                assert_eq!(rich.adoption.as_ref(), Some(&adoption));
                assert!(rich.fence.is_none());
            }
            let held = runtime
                .accounting()
                .expect("refusal keeps full original charge");
            assert_eq!(held.used, before.used);
            assert_eq!(held.unknown_peak_producers, before.unknown_peak_producers);
            Some(adoption)
        } else {
            None
        };
        let fence_operation = if expire_after_refusal {
            let adoption = refused_adopt
                .as_ref()
                .expect("expiry follows the refused original Adopt");
            // The origin is gone: no Cancel or foreign Fence will arrive.
            // Only the immutable original deadline in the charged Claim lets
            // the existing reap pass fence this SAME owner.
            let remaining_ms =
                claim.binding.original_deadline_ms - crate::media_sessions::unix_ms();
            if remaining_ms >= 0 {
                tokio::time::sleep(std::time::Duration::from_millis(
                    remaining_ms.unsigned_abs() + 50,
                ))
                .await;
            }
            self.reconcile_adaptive_loser(&entry)
                .await
                .expect("expired unresolved Adopt fences the same original owner");
            assert!(matches!(*entry.state().await, AttemptState::Handoff(_)));
            let fenced = self
                .store
                .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
                .await
                .expect("actual post-expiry status")
                .expect("same original request");
            assert_ne!(fenced.disposition, RequestDisposition::Adopted);
            let row = fenced
                .attempts
                .iter()
                .find(|row| row.key == claim.key)
                .expect("same original expired row");
            assert_eq!(row.phase, AttemptPhase::Cleanup);
            let operation = row.last_operation_id;
            assert!(matches!(
                self.store
                    .adopt_adaptive_winner(adoption)
                    .await
                    .expect("delayed same original Adopt after expiry Fence"),
                AttemptAdoption::Refused(_)
            ));
            operation
        } else {
            let receiver = self
                .begin_adaptive_cancel(stale.clone())
                .expect("original control task");
            let cancellation = receiver.await.expect("same cancellation reply");
            if refused_adopt.is_some() {
                assert_eq!(
                    cancellation.expect_err("unresolved original Adopt remains held"),
                    "adaptive Adopt outcome must be reconciled before cancellation"
                );
            } else {
                assert!(
                    cancellation.is_err(),
                    "actual Store Prepared made Fence A stale"
                );
            }
            {
                let state = entry.state().await;
                let AttemptState::Rich(rich) = &*state else {
                    panic!("original Rich owner retained")
                };
                if let Some(adoption) = refused_adopt.as_ref() {
                    assert_eq!(rich.adoption.as_ref(), Some(adoption));
                    assert!(rich.fence.is_none());
                } else {
                    assert_eq!(rich.fence.as_ref(), Some(&stale));
                }
            }
            let status = self
                .store
                .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
                .await
                .expect("fresh real status")
                .expect("same request");
            let row = status
                .attempts
                .iter()
                .find(|row| row.key == claim.key)
                .expect("same worker row");
            assert_eq!(row.phase, AttemptPhase::Prepared);
            let foreign = AttemptFence {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: status.sequence,
                    expected_attempt_sequence: row.sequence,
                    now_ms: crate::media_sessions::unix_ms(),
                },
                binding: claim.binding.clone(),
                key: claim.key.clone(),
                cancel_request: false,
            };
            assert!(matches!(
                self.store
                    .fence_adaptive_attempt(&foreign)
                    .await
                    .expect("real foreign Fence B"),
                AttemptMutation::Applied(_)
            ));
            if let Some(adoption) = refused_adopt.as_ref() {
                // Even the delayed SAME immutable Adopt cannot commit through a
                // positively fenced loser phase; this is an actual Store refusal,
                // never a native close witness or a replacement owner.
                assert!(matches!(
                    self.store
                        .adopt_adaptive_winner(adoption)
                        .await
                        .expect("delayed same original Adopt after real Fence"),
                    AttemptAdoption::Refused(
                        plurx_core::store::adaptive_resource_attempts::AttemptRefusal::WinnerLost
                    )
                ));
                let still = self
                    .store
                    .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
                    .await
                    .expect("actual post-race status")
                    .expect("same original request");
                assert_eq!(still.binding, claim.binding);
                assert_ne!(still.disposition, RequestDisposition::Adopted);
                let row = still
                    .attempts
                    .iter()
                    .find(|row| row.key == claim.key)
                    .expect("same original fenced row");
                assert_eq!(row.phase, AttemptPhase::Cleanup);
                assert_eq!(row.last_operation_id, foreign.context.operation_id);
                let state = entry.state().await;
                let AttemptState::Rich(rich) = &*state else {
                    panic!("original native owner remains held before handoff")
                };
                assert_eq!(rich.adoption.as_ref(), Some(adoption));
            }
            self.reconcile_adaptive_loser(&entry)
                .await
                .expect("same original Rich enters typed cleanup under committed Fence");
            assert!(matches!(*entry.state().await, AttemptState::Handoff(_)));
            foreign.context.operation_id
        };
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                self.reconcile_adaptive_attempt_tasks().await;
                if matches!(*entry.state().await, AttemptState::Closing(_)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original registration task returns compact selector");
        assert!(entry
            .coordinator
            .lock()
            .expect("coordinator lock")
            .is_some());
        assert!(!self
            .finish_adaptive_close(&entry)
            .await
            .expect("still held by original frames"));
        let held = self
            .store
            .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
            .await
            .expect("held status")
            .expect("same request");
        let row = held
            .attempts
            .iter()
            .find(|row| row.key == claim.key)
            .expect("same worker");
        assert_eq!(row.phase, AttemptPhase::Cleanup);
        assert_eq!(row.last_operation_id, fence_operation);
        assert_eq!(
            runtime
                .accounting()
                .expect("real R retained")
                .unknown_peak_producers,
            1
        );
        release_tx.send(()).expect("release same original frame");
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                self.reconcile_adaptive_attempt_tasks().await;
                if entry
                    .coordinator
                    .lock()
                    .expect("coordinator lock")
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("join actual original coordinator");
        assert!(!self
            .finish_adaptive_close(&entry)
            .await
            .expect("actual output alias still held"));
        assert!(
            runtime
                .accounting()
                .expect("alias still charged")
                .unknown_peak_producers
                > 0
        );
        drop(escaped);
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                // Resume the existing original finite closing book as well
                // as the worker's logical Store cleanup cadence.
                self.vod.maintain().await;
                self.reconcile_adaptive_attempt_tasks().await;
                let status = self
                    .store
                    .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
                    .await
                    .expect("original cleanup status")
                    .expect("same request");
                let row = status
                    .attempts
                    .iter()
                    .find(|row| row.key == claim.key)
                    .expect("same worker");
                if row.phase == AttemptPhase::Closed {
                    assert_ne!(
                        row.last_operation_id, fence_operation,
                        "only worker's actual retained Close follows native/app completion"
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual original native/app close permits Store Close using B");
        let settled = runtime.accounting().expect("actual original known refund");
        assert_eq!(settled.used, baseline.used);
        assert_eq!(
            settled.unknown_peak_producers,
            baseline.unknown_peak_producers
        );
    }
}

/// Runs from the same selected-store/native finite fixture as the foreign
/// fence case, but this original owner wins, Adopts, attaches and publishes.
#[cfg(all(test, feature = "cluster-integration-tests"))]
impl super::TranscodeManager {
    pub(crate) async fn adopted_publication_regression<F, Fut>(
        self: &Arc<Self>,
        claim: AttemptClaim,
        selected_recipe_json: String,
        key_capacity: usize,
        settings: crate::vodserve::VodSettings,
        authority: crate::serving_fence::ServingAuthority,
        make_pending: F,
    ) where
        F: FnOnce(usize) -> Fut + Send + 'static,
        Fut: Future<
                Output = (
                    PreparedAttemptRendition,
                    plurx_core::adaptive_resources::runtime::finite_vod::FiniteOutput,
                ),
            > + Send
            + 'static,
    {
        use plurx_core::store::adaptive_resource_attempts::WinnerSelection;
        assert!(matches!(
            self.store
                .claim_media_session_request(
                    &claim.binding.principal,
                    &claim.binding.key.request_id,
                    &claim.binding.request_fingerprint,
                    &claim.binding.playback_id,
                    &claim.binding.key.request_incarnation_id.to_string(),
                    claim.context.now_ms,
                    claim.binding.original_deadline_ms,
                )
                .await
                .expect("real original root request"),
            plurx_core::domain::MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(matches!(
            self.store
                .claim_adaptive_attempt(&claim)
                .await
                .expect("real Claim"),
            AttemptMutation::Applied(_)
        ));
        let entry = self
            .adaptive_attempts
            .reserve(
                claim.key.attempt_id,
                claim.key.request.request_incarnation_id,
                claim.worker_instance_id,
                claim.serving_generation,
                None,
                None,
            )
            .expect("original bounded worker slot");
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::clone(self);
        let original = Arc::clone(&entry);
        let original_claim = claim.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        entry
            .spawn_coordinator(
                async move {
                    let actual_frame = frame_rx.await.expect("same registered frame measurement");
                    let application_bytes = projected_application_owner_bytes(
                        &original_claim,
                        &selected_recipe_json,
                        key_capacity,
                        actual_frame,
                        crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES,
                    )
                    .expect("actual recipe and coordinator application projection");
                    let (pending, output_alias) = make_pending(application_bytes).await;
                    manager
                        .record_adaptive_prepared(
                            &original,
                            original_claim,
                            selected_recipe_json,
                            pending,
                            settings,
                            deadline,
                            application_bytes,
                        )
                        .await
                        .expect("same actual pending owner reaches Store Prepared");
                    assert!(
                        prepared_tx.send(output_alias).is_ok(),
                        "real original output alias"
                    );
                    release_rx
                        .await
                        .expect("release same original coordinator frame");
                },
                frame_tx,
            )
            .expect("retain original coordinator before entry");
        let escaped = prepared_rx.await.expect("actual original Prepared");
        assert_eq!(
            self.adaptive_attempt_local_status(claim.key.attempt_id)
                .await,
            "prepared_owner_held"
        );
        let (worker, preview_json, recipe_json) = {
            let state = entry.state().await;
            let AttemptState::Rich(rich) = &*state else {
                panic!("actual original finite owner is prepared")
            };
            (
                rich.prepared
                    .as_ref()
                    .expect("actual original Prepared")
                    .worker
                    .clone(),
                rich.preview_json.clone().expect("actual original preview"),
                rich.selected_recipe_json.clone(),
            )
        };
        let sequences = |status: &plurx_core::store::adaptive_resource_attempts::RequestStatus| {
            let row = status
                .attempts
                .iter()
                .find(|row| row.key == claim.key)
                .expect("same original row");
            (status.sequence, row.sequence)
        };
        let status = self
            .store
            .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
            .await
            .expect("actual Prepared status")
            .expect("same original request");
        let (request_sequence, attempt_sequence) = sequences(&status);
        let won = match self
            .store
            .select_adaptive_winner(&WinnerSelection {
                context: MutationContext {
                    operation_id: Uuid::new_v4(),
                    expected_request_sequence: request_sequence,
                    expected_attempt_sequence: attempt_sequence,
                    now_ms: crate::media_sessions::unix_ms(),
                },
                binding: claim.binding.clone(),
                key: claim.key.clone(),
                worker: worker.clone(),
            })
            .await
            .expect("real Winner")
        {
            AttemptMutation::Applied(status) => status,
            other => panic!("original Winner: {other:?}"),
        };
        let (request_sequence, attempt_sequence) = sequences(&won);
        let preview: crate::media_sessions::RemoteStartResponse =
            serde_json::from_str(&preview_json).expect("same actual prepared preview");
        let mut public = serde_json::to_value(&preview).expect("actual preview JSON");
        public["height"] = serde_json::json!(preview.target_height);
        public["media_origin_ms"] =
            serde_json::json!((preview.media_origin_seconds * 1_000.0).round() as i64);
        public["delivered_audio"] =
            serde_json::to_value(&preview.audio_delivery).expect("actual audio delivery");
        public["ladder"] = serde_json::json!([]);
        let public: crate::http::hls::StartResponse =
            serde_json::from_value(public).expect("real public activation response shape");
        let now_ms = crate::media_sessions::unix_ms();
        let adoption = plurx_core::store::adaptive_resource_attempts::AdaptiveAdoption {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: request_sequence,
                expected_attempt_sequence: attempt_sequence,
                now_ms,
            },
            binding: claim.binding.clone(),
            key: claim.key.clone(),
            worker,
            activation: plurx_core::domain::MediaSessionActivation {
                incarnation_id: claim.key.route_incarnation_id.to_string(),
                session_id: claim.key.session_id.to_string(),
                principal: claim.binding.principal.clone(),
                playback_id: claim.binding.playback_id.clone(),
                recovery_epoch: claim.binding.recovery_epoch.to_string(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: true,
                request_id: Some(claim.binding.key.request_id.clone()),
                request_fingerprint: claim.binding.request_fingerprint.clone(),
                owner_node_id: claim.key.worker_node_id.clone(),
                recipe_json,
                response_json: serde_json::to_string(&public).expect("canonical public response"),
                publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: (preview.media_origin_seconds * 1_000.0).round() as i64,
                now_ms,
                lease_expires_at_ms: claim.binding.original_deadline_ms,
                expected_desired_revision: claim.binding.expected_desired_revision,
            },
        };
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        *entry
            .adopt_commit_pause
            .lock()
            .expect("original commit pause") = Some(AdoptCommitPause {
            committed: committed_tx,
            resume: resume_rx,
        });
        let reply = self
            .begin_adaptive_adopt(adoption, preview_json, authority, deadline)
            .expect("same registered original Adopt control");
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), committed_rx)
            .await
            .expect("Store Adopt reaches its bounded test barrier")
            .expect("actual Store Adopt committed before attachment");
        let now = crate::media_sessions::unix_ms();
        let routes = self
            .store
            .owned_media_sessions(&claim.key.worker_node_id, now)
            .await
            .expect("actual owned lease inventory after Adopt");
        crate::media_sessions::assert_adaptive_adopt_lease_transition(
            &routes,
            &claim.key.session_id.to_string(),
            self.renewable_session_ids().await,
            now,
        );
        resume_tx.send(()).expect("resume the same adopted worker");
        let start = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), reply)
            .await
            .expect("original Adopt completes within its own deadline")
            .expect("actual original Adopt reply")
            .expect("the original owner adopts, attaches and publishes");
        assert!(
            !crate::media_sessions::settlement_protected_ids()
                .contains(&claim.key.session_id.to_string()),
            "established local worker takes over liveness"
        );
        assert_eq!(start.session_id, claim.key.session_id.to_string());
        let route = self
            .store
            .media_session_route_by_incarnation(&claim.key.route_incarnation_id.to_string())
            .await
            .expect("adopted route read")
            .expect("adopted route");
        assert_eq!(route.state, "active");
        assert_eq!(
            route.publication_ready_at_ms, 0,
            "the worker cleared its own Adopt fence before reporting adopted"
        );
        assert_eq!(route.owner_epoch, claim.key.owner_epoch);
        assert_eq!(
            self.adaptive_attempt_local_status(claim.key.attempt_id)
                .await,
            "adopted"
        );
        let ingress = self
            .store
            .publish_media_session_activation(
                &claim.binding.principal,
                &claim.binding.key.request_id,
                &claim.key.route_incarnation_id.to_string(),
                crate::media_sessions::unix_ms(),
            )
            .await
            .expect("ingress publication")
            .expect("the ingress publishes the worker-published adoption");
        assert_eq!(ingress.incarnation_id, route.incarnation_id);
        release_tx.send(()).expect("release same original frame");
        drop(escaped);
    }
}

/// Adopt is activation, not preparation: it is bounded by the create's full
/// original allowance (or the accepted remote grant), never by the earlier
/// cutoff P and R entry use.
fn original_adopt_deadline(
    entry: &AttemptEntry,
    deadline: std::time::Instant,
) -> Result<std::time::Instant, String> {
    let Some(original) = entry.continuous_activation_deadline()? else {
        return Ok(deadline);
    };
    original
        .entry_remaining()
        .map_err(|error| format!("original create activation deadline: {error}"))?;
    Ok(deadline.min(
        original
            .monotonic_cutoff()
            .map_err(|error| error.to_string())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Control-state fixture only: these DTOs and channel outcomes never
    // stand in for Store admission, real P ownership or native closure.
    fn remote_control_fixture() -> (
        RemoteRegistration,
        tokio::sync::oneshot::Receiver<plurx_core::cluster::clock::ContinuousDeadline>,
    ) {
        use plurx_core::store::adaptive_resource_attempts::{
            AttemptKey, RequestBinding, RequestKey,
        };
        let key = RequestKey {
            request_id: Uuid::new_v4().to_string(),
            request_incarnation_id: Uuid::new_v4(),
        };
        let claim = AttemptClaim {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: 0,
                expected_attempt_sequence: 0,
                now_ms: crate::media_sessions::unix_ms(),
            },
            binding: RequestBinding {
                version: 1,
                key: key.clone(),
                principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                    user_id: 1,
                },
                request_fingerprint: "a".repeat(64),
                playback_id: Uuid::new_v4().to_string(),
                recovery_epoch: Uuid::new_v4(),
                planning_generation: 1,
                planning_source_digest: "b".repeat(64),
                settings_digest: "c".repeat(64),
                intent_digest: "d".repeat(64),
                original_deadline_ms: crate::media_sessions::unix_ms() + 60_000,
                expected_predecessor_incarnation_id: None,
                expected_predecessor_owner_epoch: None,
                expected_desired_revision: None,
            },
            key: AttemptKey {
                request: key,
                attempt_id: Uuid::new_v4(),
                route_incarnation_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                worker_node_id: "fixture-node".into(),
                owner_epoch: 1,
                ordinal: 1,
            },
            worker_instance_id: Uuid::new_v4(),
            serving_generation: 0,
            domains: vec!["processing:fixture-node".into()],
        };
        claim
            .validate()
            .expect("well-formed control data, not native authority");
        let guard = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(false));
        let anchor = guard
            .capture_continuous()
            .expect("actual Linux continuous clock");
        let (grant, receiver) = tokio::sync::oneshot::channel();
        (
            RemoteRegistration {
                claim,
                nonce: Uuid::new_v4(),
                anchor,
                grant: Some(grant),
                ready: false,
                granted_ms: None,
                deadline: None,
                fence: None,
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn original_remote_grant_replay_never_renews_after_real_expiry() {
        let (mut original, receiver) = remote_control_fixture();
        original.ready = true; // control gate premise only, not a P ACK claim
        let claim = original.claim.clone();
        let nonce = original.nonce;
        original
            .apply_original_grant(&claim, nonce, 100)
            .expect("one original grant");
        let delivered = receiver.await.expect("same one-shot receiver");
        let cutoff = delivered
            .monotonic_cutoff()
            .expect("original paired cutoff");
        original
            .apply_original_grant(&claim, nonce, 100)
            .expect("exact duplicate replay");
        assert!(original.apply_original_grant(&claim, nonce, 101).is_err());
        assert!(original
            .apply_original_grant(&claim, Uuid::new_v4(), 100)
            .is_err());
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        original
            .apply_original_grant(&claim, nonce, 100)
            .expect("replay can only return original result");
        assert!(original.grant.is_none());
        assert_eq!(original.granted_ms, Some(100));
        let retained = original
            .deadline
            .as_ref()
            .expect("same retained original deadline");
        assert_eq!(retained.monotonic_cutoff().expect("same cutoff"), cutoff);
        assert!(retained.entry_remaining().is_err());
    }

    #[tokio::test]
    async fn local_attempt_carries_the_create_cutoff_into_entry_deadline() {
        let (fixture, _receiver) = remote_control_fixture();
        let claim = fixture.claim;
        let registry = AdaptiveAttemptRegistry::default();
        let later = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert!(registry.reserve_local(&claim, later).is_err());
        // An unproved replicated cluster: local evidence alone carries the cutoff.
        let clock = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(true));
        let budget = crate::media_pool::CreateStartupBudget::new_with_clock(1_000, &clock);
        let (entry, deadline) = budget
            .scope(async { registry.reserve_local(&claim, later) })
            .await
            .expect("fresh local reservation inside its create");
        let original = budget
            .scope(async { crate::media_pool::create_continuous_entry_deadline() })
            .await
            .expect("the create's own continuous cutoff");
        let carried = entry
            .continuous_entry_deadline()
            .expect("readable entry condition")
            .expect("local create cutoff carried into P and R");
        assert_eq!(
            carried.mint(),
            plurx_core::cluster::clock::ContinuousMint::Local
        );
        let cutoff = original.monotonic_cutoff().expect("original cutoff");
        assert_eq!(carried.monotonic_cutoff().expect("same cutoff"), cutoff);
        assert!(deadline <= cutoff);
        carried
            .entry_remaining()
            .expect("local entry needs no cluster evidence");
        // A replay in another create selects the SAME entry and condition.
        let other = crate::media_pool::CreateStartupBudget::new_with_clock(5_000, &clock);
        let (replay, _) = other
            .scope(async { registry.reserve_local(&claim, later) })
            .await
            .expect("replay of the same attempt");
        assert!(Arc::ptr_eq(&entry, &replay));
        assert_eq!(
            replay
                .continuous_entry_deadline()
                .expect("readable")
                .expect("retained")
                .monotonic_cutoff()
                .expect("same cutoff"),
            cutoff
        );
    }

    #[tokio::test]
    async fn local_adopt_inside_the_activation_allowance_survives_the_prepare_cutoff() {
        let (fixture, _receiver) = remote_control_fixture();
        let claim = fixture.claim;
        let registry = AdaptiveAttemptRegistry::default();
        let later = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let clock = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(true));
        // 600 ms create: P/R entry closes at 350 ms, activation at 600 ms.
        let budget = crate::media_pool::CreateStartupBudget::new_with_clock(600, &clock);
        let (entry, _) = budget
            .scope(async { registry.reserve_local(&claim, later) })
            .await
            .expect("fresh local reservation inside its create");
        let early = original_adopt_deadline(&entry, later).expect("adopt before both cutoffs");
        assert!(early < later && early > std::time::Instant::now());
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            entry
                .continuous_entry_deadline()
                .expect("readable")
                .expect("local P/R cutoff")
                .entry_remaining()
                .is_err(),
            "P and R entry stay closed after the prepare cutoff"
        );
        let adopted = original_adopt_deadline(&entry, later)
            .expect("an Adopt inside the activation allowance is not refused");
        assert_eq!(
            adopted, early,
            "the SAME full-allowance cutoff, never renewed"
        );
        tokio::time::sleep(
            early.saturating_duration_since(std::time::Instant::now())
                + std::time::Duration::from_millis(20),
        )
        .await;
        assert!(
            original_adopt_deadline(&entry, later).is_err(),
            "real expiry of the full allowance still closes Adopt"
        );
    }

    #[tokio::test]
    async fn cluster_clock_grant_refusal_stays_ungranted_so_the_original_fence_reaches_its_cleanup()
    {
        let (mut original, receiver) = remote_control_fixture();
        // A replicated guard whose roster was never proved: grant-time policy
        // is Unknown, exactly the one-voter-down / just-restarted cluster.
        let unproved = Arc::new(plurx_core::cluster::clock::ClusterClockGuard::new(true));
        original.anchor = unproved
            .capture_continuous()
            .expect("local capture needs no cluster evidence");
        original.ready = true; // control gate premise only, not a P ACK claim
        let claim = original.claim.clone();
        let nonce = original.nonce;
        assert!(matches!(
            original.apply_original_grant(&claim, nonce, 500),
            Err(RemoteGrantError::ClusterClock(
                plurx_core::cluster::clock::ClockRefusal::Unknown
            ))
        ));
        assert!(
            original.grant.is_some()
                && original.granted_ms.is_none()
                && original.deadline.is_none(),
            "a refused grant mints nothing and keeps the original registration ungranted"
        );
        // The coordinator's retry Fences this attempt through its worker;
        // the existing ungranted branch drops the unsent grant so the SAME
        // original coordinator closes its own P/M.
        let state = crate::http::internal_media_sessions::output_capture_test_state();
        let entry = state
            .transcode
            .adaptive_attempts
            .reserve(
                claim.key.attempt_id,
                claim.key.request.request_incarnation_id,
                claim.worker_instance_id,
                claim.serving_generation,
                None,
                None,
            )
            .expect("original registration slot");
        *entry.remote_registration.lock().expect("registration") = Some(original);
        let fence = plurx_core::store::adaptive_resource_attempts::AttemptFence {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: 1,
                expected_attempt_sequence: 1,
                now_ms: crate::media_sessions::unix_ms(),
            },
            binding: claim.binding.clone(),
            key: claim.key.clone(),
            cancel_request: false,
        };
        let cancelled = state
            .transcode
            .begin_adaptive_cancel(fence.clone())
            .expect("the original worker accepts its own Fence");
        assert!(
            cancelled.await.expect("cancel answer").is_err(),
            "the answer never asserts native extinction"
        );
        assert!(
            receiver.await.is_err(),
            "the unsent original grant is dropped, so the coordinator closes P/M"
        );
        let held = entry.remote_registration.lock().expect("registration");
        let remote = held.as_ref().expect("same original registration");
        assert_eq!(remote.fence.as_ref(), Some(&fence));
        assert!(remote.grant.is_none() && remote.granted_ms.is_none());
    }

    #[tokio::test]
    async fn actual_late_or_unready_grant_cannot_consume_original_start() {
        let (mut original, mut receiver) = remote_control_fixture();
        let claim = original.claim.clone();
        let nonce = original.nonce;
        assert!(original.apply_original_grant(&claim, nonce, 50).is_err());
        original.ready = true; // pure gate state, never a native receipt
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(original.apply_original_grant(&claim, nonce, 50).is_err());
        assert!(
            original.grant.is_some()
                && original.deadline.is_none()
                && original.granted_ms.is_none()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(original.grant.take());
        assert!(
            receiver.await.is_err(),
            "same original unsent start is dropped, never aborted"
        );
    }

    #[tokio::test]
    async fn original_adopt_task_retains_restart_admission_after_waiter_drop() {
        let monitor = plurx_core::cluster::migration::status::ReplicationMonitor::sqlite();
        let fence = crate::serving_fence::ServingFence::new(monitor.metrics_handle());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let authority = fence.authority();
        let original = tokio::spawn(async move {
            let result = restart_guarded_adoption(authority, async move {
                entered_tx.send(()).expect("entered original adoption");
                release_rx.await.expect("release same original adoption");
                Ok::<_, String>(())
            })
            .await;
            let _ = result_tx.send(result);
        });
        entered_rx.await.expect("original adoption entered");
        drop(result_rx);
        fence
            .begin_restart_preparation_until(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_millis() as u64
                    + 60_000,
            )
            .await
            .expect("fence");
        assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
        let polled = std::sync::atomic::AtomicBool::new(false);
        let denied = restart_guarded_adoption(fence.authority(), async {
            polled.store(true, std::sync::atomic::Ordering::Release);
            Ok::<_, String>(())
        })
        .await;
        assert!(denied.is_err());
        assert!(!polled.load(std::sync::atomic::Ordering::Acquire));
        release_tx.send(()).expect("release original task");
        original.await.expect("same original task joined");
        assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    }

    #[tokio::test]
    async fn registered_attempt_starts_once_and_replay_selects_same_owner() {
        let registry = AdaptiveAttemptRegistry::default();
        let attempt = Uuid::new_v4();
        let request = Uuid::new_v4();
        let worker = Uuid::new_v4();
        let entry = registry
            .reserve(attempt, request, worker, 7, None, None)
            .expect("reserve");
        let replay = registry
            .reserve(attempt, request, worker, 7, None, None)
            .expect("replay");
        assert!(Arc::ptr_eq(&entry, &replay));
        assert!(registry
            .reserve(attempt, request, Uuid::new_v4(), 7, None, None)
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
