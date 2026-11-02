//! Frozen encoded-VOD recipes and per-generation foreground capacity.

#[cfg(target_os = "linux")]
#[path = "dv_runtime.rs"]
pub(crate) mod dv_runtime;

#[cfg(any(target_os = "linux", test))]
#[path = "dv_report_lifetime.rs"]
pub(crate) mod dv_report_lifetime;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plurx_core::domain::MediaFile;
use plurx_core::segplan::SourceIdentity;
use plurx_core::transcode::{
    vod_pipe_args_with_source_clock, Pacing, ResolvedTranscode, TranscodeExecution,
    TranscodeOptions, VodFrameGrid,
};
use sha2::{Digest, Sha256};

#[cfg(any(target_os = "linux", test))]
pub(crate) mod dv_failed_episode {
    use plurx_core::transcode::dv_processing::{DvDestination, DvDigest};
    use sha2::{Digest, Sha256};
    fn ledger() -> &'static std::sync::Mutex<std::collections::VecDeque<DvDigest>> {
        static LEDGER: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<DvDigest>>> =
            std::sync::OnceLock::new();
        LEDGER.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
    }
    pub(crate) fn key(
        playback: &str,
        source: &str,
        backend: &DvDigest,
        settings: i64,
        destination: DvDestination,
    ) -> DvDigest {
        let bytes = serde_json::to_vec(&(
            "plurx.dv.failed-playback-episode.v1",
            playback,
            source,
            backend,
            settings,
            destination,
        ))
        .expect("DV episode identity");
        DvDigest::new(hex::encode(Sha256::digest(bytes))).expect("DV episode digest")
    }
    pub(crate) fn failed(key: &DvDigest) -> bool {
        ledger().lock().expect("DV failed episodes").contains(key)
    }
    pub(crate) fn refuse(key: &DvDigest) {
        let mut ledger = ledger().lock().expect("DV failed episodes");
        if !ledger.contains(key) {
            if ledger.len() == 128 {
                ledger.pop_front();
            }
            ledger.push_back(key.clone());
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn failed_later_window_excludes_fresh_create_without_predecessor_fields() {
            let backend = DvDigest::new("12".repeat(32)).expect("valid focused control");
            let first = key(
                "failure-after-first-window",
                "source-a",
                &backend,
                41,
                DvDestination::Hdr10,
            );
            assert!(
                !failed(&first),
                "the initial completed window can select processing"
            );
            refuse(&first);
            let fresh = key(
                "failure-after-first-window",
                "source-a",
                &backend,
                41,
                DvDestination::Hdr10,
            );
            assert!(
                failed(&fresh),
                "fresh creation must use the ordinary route after later refusal"
            );
            for changed in [
                key(
                    "new-playback",
                    "source-a",
                    &backend,
                    41,
                    DvDestination::Hdr10,
                ),
                key(
                    "failure-after-first-window",
                    "source-b",
                    &backend,
                    41,
                    DvDestination::Hdr10,
                ),
                key(
                    "failure-after-first-window",
                    "source-a",
                    &backend,
                    42,
                    DvDestination::Hdr10,
                ),
                key(
                    "failure-after-first-window",
                    "source-a",
                    &backend,
                    41,
                    DvDestination::Profile81,
                ),
                key(
                    "failure-after-first-window",
                    "source-a",
                    &DvDigest::new("34".repeat(32)).expect("valid focused control"),
                    41,
                    DvDestination::Hdr10,
                ),
            ] {
                assert!(!failed(&changed));
            }
        }
    }
}

#[cfg(test)]
use crate::seam_hooks::AsyncPause;
use crate::seam_hooks::{HookFuture, HookReady};

use crate::admission::{
    Admissions, HwSlot, LiveWait, PoolSnapshot, Priority, SwPermit, TranscodeResourceEstimate,
    Workload,
};

/// Freeze the encoder allowance without reducing whole-pipeline accounting.
pub(crate) fn frozen_software_threads(work: &Workload<'_>, budget: usize) -> u32 {
    work.software_threads().min(budget).max(1) as u32
}

/// The CPU threads a frozen encoder needs from the software budget before
/// any admission can succeed, whatever else the pool holds.
pub(crate) fn frozen_admission_floor(
    resources: &TranscodeResourceEstimate,
    software_threads: Option<u32>,
) -> usize {
    if resources.hardware_slot {
        // Hardware output ignores the software encoder cap. Its CPU decode
        // and filter estimate remains the operative policy floor.
        resources.cpu_threads
    } else {
        resources
            .cpu_threads
            .min(software_threads.map_or(resources.cpu_threads, |threads| threads as usize))
    }
}

/// Current policy must still allow the frozen encoder. The shared admission
/// pool owns the conservative pipeline claim and its isolated oversize rule.
pub(crate) fn try_admit_frozen_bundle(
    admissions: &Admissions,
    hardware_limit: usize,
    software_budget: usize,
    resources: &TranscodeResourceEstimate,
    software_threads: Option<u32>,
    priority: Priority,
    claim: Option<u64>,
) -> Result<crate::admission::TranscodePermit, bool> {
    if frozen_admission_floor(resources, software_threads) > software_budget {
        return Err(true);
    }
    admissions
        .try_admit_bundle_claiming(hardware_limit, software_budget, resources, priority, claim)
        .ok_or(false)
}

/// A first processed window is foreground preparation. Register its demand
/// before waiting so background producers actually yield their owned permits.
/// Never discount a still-running producer or extend the caller's deadline.
///
/// Waiting is only for capacity that was asked to yield: while background
/// work owns a permit, it is told to and the wait is for its release. When
/// nothing background holds capacity, what blocks is foreground work that
/// yields to nobody, and the answer is the immediate refusal it always was,
/// so the ordinary start this preparation would enhance keeps its budget.
#[cfg(any(target_os = "linux", test))]
pub(crate) async fn admit_frozen_preparation(
    admissions: &Admissions,
    hardware_limit: usize,
    software_budget: usize,
    resources: &TranscodeResourceEstimate,
    software_threads: Option<u32>,
    deadline: Instant,
) -> Result<crate::admission::TranscodePermit, bool> {
    if Instant::now() >= deadline {
        return Err(false);
    }
    let mut waiting = admissions.queue_for_slot();
    loop {
        if Instant::now() >= deadline {
            return Err(false);
        }
        match try_admit_frozen_bundle(
            admissions,
            hardware_limit,
            software_budget,
            resources,
            software_threads,
            Priority::Live,
            None,
        ) {
            Ok(permit) if Instant::now() < deadline => return Ok(permit),
            Ok(_) => return Err(false),
            Err(true) => return Err(true),
            Err(false) if Instant::now() >= deadline => return Err(false),
            Err(false) if !admissions.background_is_active() => return Err(false),
            Err(false) => {
                waiting.mark_capacity_blocked();
                tokio::time::sleep_until(
                    deadline
                        .min(Instant::now() + crate::transcode::ADMISSION_POLL)
                        .into(),
                )
                .await;
            }
        }
    }
}

/// Receipt shared with the exact predecessor handoff that reserved capacity.
#[derive(Clone)]
pub(crate) struct HandoffClaim {
    token: u64,
    admitted: Arc<std::sync::atomic::AtomicBool>,
}

/// Resolved once before attachment. A restart cannot silently change encoder,
/// grade, cadence, rate control, tracks, or burn pixels under an immutable URI.
pub(crate) struct Encoding {
    pub source_object_version: String,
    pub source_clock: plurx_core::transcode::VodSourceClock,
    pub plan: ResolvedTranscode,
    /// Worker-selected processing decision frozen with this rendition. A saved
    /// switch or catalog DV label cannot create a selected backend plan.
    pub dv_processing: plurx_core::transcode::dv_processing::DvSelection,
    #[cfg(target_os = "linux")]
    pub dv_runtime: Option<Arc<dv_runtime::RuntimeRecipe>>,
    /// A soundtrack producer owns its AAC recipe independently of video.
    pub shared_audio: Option<plurx_core::transcode::VodSharedAudioRecipe>,
    pub resources: TranscodeResourceEstimate,
    pub options: TranscodeOptions,
    pub grid: VodFrameGrid,
    /// Saved operator choice, frozen for this rendition and hashed into identity.
    pub reorder_frames: bool,
    pub subtitle: Option<Arc<std::fs::File>>,
    pub subtitle_digest: Option<String>,
    pub ffmpeg_build: String,
    pub executable: crate::ffmpeg::EncodedExecutable,
    pub engine: crate::ffmpeg::EncodedEngine,
    pub admissions: Admissions,
    pub store: Arc<dyn plurx_core::store::Store>,
    /// True only while this rendition belongs to an uncommitted prepared
    /// successor. Admission reads it at each producer start so a committed VOD
    /// session becomes ordinary foreground work without rebuilding its recipe.
    pub speculative: std::sync::atomic::AtomicBool,
    /// Voluntary Auto probes cannot borrow the incumbent's resources.
    pub nonpreemptive_trial: bool,
    pub candidate_recipe: Option<[u8; 32]>,
    pub production_proofs: Arc<CandidateProductionProofs>,
    pub active_production: Mutex<ActiveProductionWindow>,

    pub queued: Mutex<Option<LiveWait>>,
    pub policy_retry: std::sync::atomic::AtomicBool,
    /// Set while a refused prepared successor has asked its own viewer's
    /// predecessor to give back an encoder permit. A speculative rendition
    /// registers no pool waiter, so without this its driver would not retry
    /// until the next GET arrived — after the permit it asked for had been
    /// released, and possibly after the client's request budget had expired.
    pub handoff_wait: std::sync::atomic::AtomicBool,
    /// Why the most recent admission attempt was refused, for attribution.
    pub last_refusal: Mutex<Option<PermitRefusal>>,
    /// The pool reservation a yielding predecessor made for this successor.
    /// Only this rendition's admission can claim it.
    pub handoff_claim: Mutex<Option<HandoffClaim>>,
    /// The admission point a race test can pause at; production installs
    /// [`NoopEncodingHooks`] (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
    pub hooks: Box<dyn EncodingHooks>,
}

/// Recent exact-recipe observations on this worker; this never grants a permit.
#[derive(Debug, Default)]
pub(crate) struct CandidateProductionProofs {
    rows: Mutex<HashMap<[u8; 32], ActiveProductionEvidence>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveProductionEvidence {
    pub milli_realtime: u32,
    pub active_ms: u32,
    pub completed_segments: u32,
    pub observed_at: Instant,
}

impl CandidateProductionProofs {
    #[cfg(test)]
    pub(crate) fn record_for_test(&self, recipe: [u8; 32], proof: ActiveProductionEvidence) {
        self.record(recipe, proof);
    }

    pub(crate) fn get(&self, recipe: [u8; 32]) -> Option<ActiveProductionEvidence> {
        let now = Instant::now();
        let mut rows = self.rows.lock().expect("candidate production proofs");
        rows.retain(|_, proof| {
            now.saturating_duration_since(proof.observed_at) <= Duration::from_secs(15)
        });
        rows.get(&recipe).copied()
    }

    fn remove(&self, recipe: Option<[u8; 32]>) {
        if let Some(recipe) = recipe {
            self.rows
                .lock()
                .expect("candidate production proofs")
                .remove(&recipe);
        }
    }

    fn record(&self, recipe: [u8; 32], proof: ActiveProductionEvidence) {
        let mut rows = self.rows.lock().expect("candidate production proofs");
        rows.retain(|_, value| {
            proof
                .observed_at
                .saturating_duration_since(value.observed_at)
                <= Duration::from_secs(15)
        });
        if rows.len() >= 256 && !rows.contains_key(&recipe) {
            if let Some(oldest) = rows
                .iter()
                .min_by_key(|(_, value)| value.observed_at)
                .map(|(key, _)| *key)
            {
                rows.remove(&oldest);
            }
        }
        rows.insert(recipe, proof);
    }
}

/// Points from one continuous unpaced producer interval. The first completed
/// video fragment establishes a baseline, excluding startup. Holds and retries
/// discard the interval rather than treating idle time as spare capacity.
#[derive(Debug, Default)]
pub(crate) struct ActiveProductionWindow {
    generation: u64,
    active: bool,
    /// One uninterrupted parent-held entitlement across bounded DV helpers.
    /// Weak ownership cannot keep cached capacity alive.
    #[cfg(any(target_os = "linux", test))]
    processed_reservation: std::sync::Weak<EncodeReservation>,
    points: VecDeque<(Instant, u32, i64)>,
    evidence: Option<ActiveProductionEvidence>,
}

impl ActiveProductionWindow {
    #[cfg(any(target_os = "linux", test))]
    fn begin_processed_window(
        &mut self,
        generation: u64,
        entry: u32,
        permit: &EncodePermit,
    ) -> bool {
        let reservation = Arc::downgrade(&permit._reservation);
        let contiguous = self.active
            && self.processed_reservation.ptr_eq(&reservation)
            && self.points.back().is_some_and(|(at, previous, _)| {
                previous.checked_add(1) == Some(entry) && at.elapsed() <= Duration::from_secs(15)
            });
        if contiguous {
            // Keep the generation fence current without erasing measured
            // elapsed time between consecutive, physically reaped helpers.
            self.generation = generation;
        } else {
            *self = Self {
                generation,
                active: true,
                processed_reservation: reservation,
                ..Self::default()
            };
        }
        contiguous
    }

    fn observe(
        &mut self,
        now: Instant,
        generation: u64,
        entry: u32,
        end_ms: i64,
    ) -> Option<ActiveProductionEvidence> {
        if !self.active || self.generation != generation {
            return None;
        }
        if self.points.back().is_some_and(|(_, previous, end)| {
            previous.checked_add(1) != Some(entry) || *end >= end_ms
        }) {
            self.points.clear();
            self.evidence = None;
        }
        while self
            .points
            .front()
            .is_some_and(|(at, _, _)| now.saturating_duration_since(*at) > Duration::from_secs(15))
        {
            self.points.pop_front();
        }
        self.points.push_back((now, entry, end_ms));
        while self.points.len() > 64 {
            self.points.pop_front();
        }
        let (first_at, first_entry, first_end) = *self.points.front()?;
        let elapsed = now.saturating_duration_since(first_at).as_millis();
        let completed_segments = entry.saturating_sub(first_entry);
        if completed_segments < 2 || elapsed < 2000 {
            return None;
        }
        let media_ms = end_ms.checked_sub(first_end)?;
        let speed = u128::try_from(media_ms)
            .ok()?
            .checked_mul(1000)?
            .checked_div(elapsed)?;
        let evidence = ActiveProductionEvidence {
            milli_realtime: u32::try_from(speed).ok()?,
            active_ms: u32::try_from(elapsed).ok()?,
            completed_segments,
            observed_at: now,
        };
        self.evidence = Some(evidence);
        Some(evidence)
    }
}

/// The points of an encoding's admission that a test can pause at
/// (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
///
/// Every [`Encoding`] holds one of these in every build, so its layout and the
/// await points of [`Encoding::try_permit`] are the same in the test and
/// release binaries. The hooks are chosen where the encoding is built:
/// production (`TranscodeManager::prepare_vod_encoding`) installs
/// [`NoopEncodingHooks`], and the test fixtures, which build their own
/// encodings, install `EncodingAdmissionPause`. A paused hook's timing is still
/// a test artefact: what this makes identical is the struct and the set of
/// await points, not scheduling.
///
/// `Any` is a supertrait only so a test can reach the pause it installed on an
/// encoding it handed to a rendition.
pub(crate) trait EncodingHooks: std::any::Any + Send + Sync {
    /// A producer start asked for an encoder permit, before admission reads
    /// the node's pool policy.
    fn before_admission(&self) -> HookFuture<'_>;
}

/// What production installs: the admission point is already ready.
pub(crate) struct NoopEncodingHooks;

impl EncodingHooks for NoopEncodingHooks {
    fn before_admission(&self) -> HookFuture<'_> {
        Box::pin(HookReady)
    }
}

/// What the test fixtures install: nothing pauses until a test arms the next
/// admission with [`Encoding::pause_next_admission`]; the armed pause is taken
/// by that one admission.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct EncodingAdmissionPause {
    armed: Mutex<Option<Arc<AsyncPause>>>,
}

#[cfg(test)]
impl EncodingHooks for EncodingAdmissionPause {
    fn before_admission(&self) -> HookFuture<'_> {
        let pause = self.armed.lock().expect("admission test pause").take();
        Box::pin(async move {
            if let Some(pause) = pause {
                pause.hold().await;
            }
        })
    }
}

impl std::fmt::Debug for Encoding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Encoding")
            .field("dv_processing", &self.dv_processing)
            .field("decoder", &self.plan.decode().backend())
            .field("encoder", &self.plan.encoder())
            .field("resources", &self.resources())
            .field("options", &self.options)
            .field("grid", &self.grid)
            .finish_non_exhaustive()
    }
}

/// One refused encoder admission, with the pool state that refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PermitRefusal {
    pub priority: Priority,
    pub hardware_limit: usize,
    pub software_budget: usize,
    /// The frozen plan needs more threads than the whole software budget; no
    /// release anywhere can admit it.
    pub over_budget: bool,
    pub pool: PoolSnapshot,
}

impl std::fmt::Display for PermitRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?} priority; hardware {}/{} in use; software {}/{} threads in use; \
             {} live waiter(s); background {}; {} reservation(s){}",
            self.priority,
            self.pool.hardware_used,
            self.hardware_limit,
            self.pool.software_used,
            self.software_budget,
            self.pool.live_waiting,
            if self.pool.background_active {
                "holds a permit"
            } else {
                "idle"
            },
            self.pool.reservations,
            if self.over_budget {
                "; the plan needs more threads than the whole software budget"
            } else {
                ""
            }
        )
    }
}

/// Ordinary producers retain this until their exact process is reaped.
/// A bounded family may additionally retain the same reservation for its
/// attachment lifetime; a retiring worker's clone keeps capacity owned even
/// when its parent has already ended.
#[derive(Debug, Clone)]
pub(crate) struct EncodePermit {
    _reservation: Arc<EncodeReservation>,
}

#[derive(Debug)]
struct EncodeReservation {
    _hardware: Option<HwSlot>,
    _software: Option<SwPermit>,
    worker_claimed: std::sync::atomic::AtomicBool,
}

/// Weak link from one immutable rendition to its exact retained capacity.
/// A cached rendition owns no admission by itself. Parents may adopt the
/// running worker's credit, and later workers borrow that same entitlement.
#[derive(Debug, Default)]
pub(crate) struct RetainedEncodeAdmission {
    reservation: Mutex<std::sync::Weak<EncodeReservation>>,
}
/// Stabilize the weak binding during a family's one atomic pool decision.
/// This synchronous guard is held only after all asynchronous policy reads.
pub(crate) struct RetainedEncodeBinding<'a> {
    reservation: std::sync::MutexGuard<'a, std::sync::Weak<EncodeReservation>>,
}

impl RetainedEncodeAdmission {
    pub(crate) fn lock_binding(&self) -> RetainedEncodeBinding<'_> {
        RetainedEncodeBinding {
            reservation: self
                .reservation
                .lock()
                .expect("retained rendition admission"),
        }
    }

    pub(crate) fn current(&self) -> Option<EncodePermit> {
        self.lock_binding().current()
    }

    pub(crate) fn bind(&self, admitted: EncodePermit) -> EncodePermit {
        self.lock_binding().bind(admitted)
    }
}

impl RetainedEncodeBinding<'_> {
    pub(crate) fn current(&self) -> Option<EncodePermit> {
        self.reservation.upgrade().map(|reservation| EncodePermit {
            _reservation: reservation,
        })
    }

    pub(crate) fn bind(&mut self, admitted: EncodePermit) -> EncodePermit {
        if let Some(existing) = self.reservation.upgrade() {
            // An attachment arriving while ordinary admission was in flight
            // already owns this rendition's exact credit. Release the newly
            // admitted duplicate and preserve the existing worker claim.
            EncodePermit {
                _reservation: existing,
            }
        } else {
            *self.reservation = Arc::downgrade(&admitted._reservation);
            admitted
        }
    }
}

/// Exclusive process ownership, retained by the producer slot until reap.
/// Retaining a family reservation never grants concurrent use of its capacity.
#[derive(Debug)]
pub(crate) struct EncodeWorkerPermit {
    reservation: Arc<EncodeReservation>,
}

impl EncodePermit {
    pub(crate) fn same_reservation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self._reservation, &other._reservation)
    }
    pub(crate) fn retained_resources(&self) -> crate::admission::RetainedTranscodePermit<'_> {
        crate::admission::RetainedTranscodePermit {
            hardware: self._reservation._hardware.as_ref(),
            software: self._reservation._software.as_ref(),
        }
    }

    pub(crate) fn family_id(&self) -> Option<u64> {
        self.retained_resources().family_id()
    }

    pub(crate) fn try_claim_worker(self) -> Option<EncodeWorkerPermit> {
        self._reservation
            .worker_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()?;
        Some(EncodeWorkerPermit {
            reservation: self._reservation,
        })
    }
}

impl EncodeWorkerPermit {
    #[cfg(target_os = "linux")]
    pub(crate) fn reservation(&self) -> EncodePermit {
        EncodePermit {
            _reservation: Arc::clone(&self.reservation),
        }
    }
    /// Borrow the reservation owned by this exclusive generation worker.
    pub(crate) fn retained_resources(&self) -> crate::admission::RetainedTranscodePermit<'_> {
        crate::admission::RetainedTranscodePermit {
            hardware: self.reservation._hardware.as_ref(),
            software: self.reservation._software.as_ref(),
        }
    }
}

impl Drop for EncodeWorkerPermit {
    fn drop(&mut self) {
        self.reservation
            .worker_claimed
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl From<crate::admission::TranscodePermit> for EncodePermit {
    fn from(bundle: crate::admission::TranscodePermit) -> Self {
        let (hardware, software) = bundle.into_parts();
        Self {
            _reservation: Arc::new(EncodeReservation {
                _hardware: hardware,
                _software: software,
                worker_claimed: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }
}

/// Conservative Source copy pipeline estimate: bounded input codec, audio
/// encoder, filter and mux/fragment work. This is the existing governor's
/// reservation, not a process CPU quota or a one-thread profiling claim.
pub(crate) const SOURCE_COPY_CPU_THREADS: usize = 4;

#[allow(dead_code)] // The owned Source actor retains and consumes the actual permit.
pub(crate) enum SourceCopyPermitRead {
    Admitted(EncodePermit),
    Capacity,
    Unavailable,
}
impl EncodePermit {
    /// One Source copy attempt, as a live start: a single admission against
    /// the node's current pool policy. Later generations of an attached
    /// rendition use this; a first start waits with [`Self::admit_source_copy`].
    #[allow(dead_code)] // The owned Source actor acquires before first activation.
    pub(crate) async fn try_source_copy(
        admissions: &Admissions,
        store: &dyn plurx_core::store::Store,
    ) -> SourceCopyPermitRead {
        let mut waiting = admissions.queue_for_slot();
        let Some((hardware_limit, software_budget)) = source_copy_policy(store).await else {
            return SourceCopyPermitRead::Unavailable;
        };
        if software_budget < SOURCE_COPY_CPU_THREADS {
            return SourceCopyPermitRead::Capacity;
        }
        let result = Self::try_source_copy_under(admissions, hardware_limit, software_budget);
        if matches!(result, SourceCopyPermitRead::Capacity) {
            waiting.mark_capacity_blocked();
        }
        result
    }

    /// Wait for a Source copy permit until `deadline`, as a live start.
    ///
    /// The waiter is registered for the whole wait, the way `admit_live`
    /// holds its queue guard: background work is told to yield for as long as
    /// this start is queuing, and cannot take the capacity back between two
    /// attempts. Pool policy is node state read once per wait, under the same
    /// bound every encoder admission uses. A budget below the copy's own
    /// estimate cannot change during that wait and is refused at once;
    /// anything else is retried on the manager's admission cadence, clamped
    /// to the deadline.
    pub(crate) async fn admit_source_copy(
        admissions: &Admissions,
        store: &dyn plurx_core::store::Store,
        deadline: Instant,
    ) -> SourceCopyPermitRead {
        let mut waiting = admissions.queue_for_slot();
        let Some((hardware_limit, software_budget)) = source_copy_policy(store).await else {
            return SourceCopyPermitRead::Unavailable;
        };
        if software_budget < SOURCE_COPY_CPU_THREADS {
            return SourceCopyPermitRead::Capacity;
        }
        loop {
            if let admitted @ SourceCopyPermitRead::Admitted(_) =
                Self::try_source_copy_under(admissions, hardware_limit, software_budget)
            {
                return admitted;
            }
            waiting.mark_capacity_blocked();
            let now = Instant::now();
            if now >= deadline {
                return SourceCopyPermitRead::Capacity;
            }
            tokio::time::sleep(crate::transcode::ADMISSION_POLL.min(deadline - now)).await;
        }
    }

    fn try_source_copy_under(
        admissions: &Admissions,
        hardware_limit: usize,
        software_budget: usize,
    ) -> SourceCopyPermitRead {
        let resources = TranscodeResourceEstimate {
            hardware_slot: false,
            cpu_threads: SOURCE_COPY_CPU_THREADS,
            decoder_threads: Some(1),
        };
        if software_budget < SOURCE_COPY_CPU_THREADS {
            return SourceCopyPermitRead::Capacity;
        }
        let Some(bundle) = admissions.try_admit_bundle(
            hardware_limit,
            software_budget,
            &resources,
            Priority::Live,
        ) else {
            return SourceCopyPermitRead::Capacity;
        };
        SourceCopyPermitRead::Admitted(bundle.into())
    }
}

/// The node's pool policy for a Source copy: hardware cap and software
/// budget, defaulted exactly as encoder admission defaults them. A read that
/// fails or outlives its bound closes admission.
async fn source_copy_policy(store: &dyn plurx_core::store::Store) -> Option<(usize, usize)> {
    let Ok(Ok((hardware, software))) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.get_setting_pair(
            plurx_core::store::keys::MAX_HW_SESSIONS,
            plurx_core::store::keys::SW_POOL_THREADS,
        ),
    )
    .await
    else {
        return None;
    };
    let hardware_limit = hardware
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(crate::admission::DEFAULT_MAX_HW_SESSIONS);
    let software_budget = software
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(crate::admission::software_budget);
    Some((hardware_limit, software_budget))
}

impl Encoding {
    #[cfg(test)]
    pub(crate) async fn clone_with_admissions_for_test(
        &self,
        admissions: Admissions,
    ) -> Arc<Encoding> {
        Arc::new(Encoding {
            source_object_version: self.source_object_version.clone(),
            source_clock: self.source_clock,
            plan: self.plan.clone(),
            dv_processing: self.dv_processing.clone(),
            #[cfg(target_os = "linux")]
            dv_runtime: self.dv_runtime.clone(),
            shared_audio: self.shared_audio.clone(),
            resources: self.resources,
            options: self.options.clone(),
            grid: self.grid,
            reorder_frames: self.reorder_frames,
            subtitle: self.subtitle.clone(),
            subtitle_digest: self.subtitle_digest.clone(),
            ffmpeg_build: self.ffmpeg_build.clone(),
            executable: crate::ffmpeg::EncodedExecutable::capture()
                .await
                .expect("test encoder executable remains available"),
            engine: self.engine.clone(),
            admissions,
            store: Arc::clone(&self.store),
            nonpreemptive_trial: self.nonpreemptive_trial,
            candidate_recipe: self.candidate_recipe,
            production_proofs: Arc::clone(&self.production_proofs),
            active_production: Mutex::new(ActiveProductionWindow::default()),

            speculative: std::sync::atomic::AtomicBool::new(
                self.speculative.load(std::sync::atomic::Ordering::Acquire),
            ),
            queued: Mutex::new(None),
            policy_retry: std::sync::atomic::AtomicBool::new(false),
            handoff_wait: std::sync::atomic::AtomicBool::new(false),
            last_refusal: Mutex::new(None),
            handoff_claim: Mutex::new(None),
            hooks: Box::new(EncodingAdmissionPause::default()),
        })
    }

    /// Arm a pause at this encoding's next admission, before it reads the pool
    /// policy. Only test-built encodings carry the pausing hooks.
    #[cfg(test)]
    pub(crate) fn pause_next_admission(&self) -> Arc<AsyncPause> {
        let hooks: &dyn std::any::Any = &*self.hooks;
        let pauses = hooks
            .downcast_ref::<EncodingAdmissionPause>()
            .expect("test encodings install EncodingAdmissionPause");
        let pause = AsyncPause::new("encoding admission");
        *pauses.armed.lock().expect("admission test pause") = Some(Arc::clone(&pause));
        pause
    }

    pub(crate) fn mark_speculative(&self) {
        self.speculative
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn reset_active_production(&self, generation: u64, active: bool) {
        let mut window = self.active_production.lock().expect("active production");
        *window = ActiveProductionWindow {
            generation,
            active,
            ..ActiveProductionWindow::default()
        };
        if self.shared_audio.is_none() {
            self.production_proofs.remove(self.candidate_recipe);
        }
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn begin_processed_window(
        &self,
        generation: u64,
        entry: u32,
        permit: &EncodePermit,
    ) {
        let mut window = self.active_production.lock().expect("active production");
        window.begin_processed_window(generation, entry, permit);
        if window.evidence.is_none() {
            self.production_proofs.remove(self.candidate_recipe);
        }
    }

    pub(crate) fn note_active_segment(&self, generation: u64, entry: u32, end_ms: i64) {
        if self.shared_audio.is_some() {
            return;
        }
        let mut window = self.active_production.lock().expect("active production");
        let proof = window.observe(Instant::now(), generation, entry, end_ms);

        if let (Some(recipe), Some(proof)) = (self.candidate_recipe, proof) {
            self.production_proofs.record(recipe, proof);
        } else if window.evidence.is_none() {
            self.production_proofs.remove(self.candidate_recipe);
        }
    }

    pub(crate) fn active_production_evidence(&self) -> Option<ActiveProductionEvidence> {
        if self.shared_audio.is_some() {
            return None;
        }
        self.active_production
            .lock()
            .expect("active production")
            .evidence
            .filter(|proof| proof.observed_at.elapsed() <= Duration::from_secs(15))
    }

    pub(crate) fn promote(&self) {
        self.speculative
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_speculative(&self) -> bool {
        self.speculative.load(std::sync::atomic::Ordering::Acquire)
    }

    fn priority(&self) -> Priority {
        if self.speculative.load(std::sync::atomic::Ordering::Acquire) {
            Priority::Speculative
        } else {
            Priority::Live
        }
    }

    pub async fn try_permit(&self) -> Option<EncodePermit> {
        self.hooks.before_admission().await;
        self.try_permit_after(self.store.get_setting_pair(
            plurx_core::store::keys::MAX_HW_SESSIONS,
            plurx_core::store::keys::SW_POOL_THREADS,
        ))
        .await
    }

    /// Revalidate demand after the asynchronous policy read. The returned
    /// guard remains held while registering the queue and attempting admission.
    pub(crate) async fn try_permit_guarded<G>(
        &self,
        demand: impl std::future::Future<Output = Option<(Priority, G)>>,
    ) -> Option<EncodePermit> {
        self.hooks.before_admission().await;
        self.try_permit_with_priority(
            self.store.get_setting_pair(
                plurx_core::store::keys::MAX_HW_SESSIONS,
                plurx_core::store::keys::SW_POOL_THREADS,
            ),
            None,
            async {
                demand
                    .await
                    .map(|(priority, guard)| (Some(priority), guard))
            },
        )
        .await
    }

    /// The policy-read future is injected only to test the same production
    /// deadline without making an unresolved backend strand a test runner.
    pub(crate) async fn try_permit_after(
        &self,
        policy: impl std::future::Future<
            Output = Result<(Option<String>, Option<String>), plurx_core::error::StoreError>,
        >,
    ) -> Option<EncodePermit> {
        self.try_permit_with_priority(policy, None, async { Some((None, ())) })
            .await
    }

    /// The finite output-preparation owner uses real background resources;
    /// it never registers foreground demand or borrows a handoff claim.
    pub(crate) async fn try_background_permit(&self) -> Option<EncodePermit> {
        self.try_permit_with_priority(
            self.store.get_setting_pair(
                plurx_core::store::keys::MAX_HW_SESSIONS,
                plurx_core::store::keys::SW_POOL_THREADS,
            ),
            Some(Priority::Background),
            async { Some((None, ())) },
        )
        .await
    }

    async fn try_permit_with_priority<G>(
        &self,
        policy: impl std::future::Future<
            Output = Result<(Option<String>, Option<String>), plurx_core::error::StoreError>,
        >,
        preparation_priority: Option<Priority>,
        demand: impl std::future::Future<Output = Option<(Option<Priority>, G)>>,
    ) -> Option<EncodePermit> {
        // Pool policy is current node state, not immutable media identity.
        // A failed policy read closes admission; an existing child's permit
        // remains owned until reap and is never confiscated underneath it.
        let Ok(Ok((hardware, software))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), policy).await
        else {
            self.cancel_wait();
            self.last_refusal
                .lock()
                .expect("VOD encoder refusal")
                .take();
            self.policy_retry
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return None;
        };
        let Some((demand_priority, _demand_guard)) = demand.await else {
            self.cancel_wait();
            self.last_refusal
                .lock()
                .expect("VOD encoder refusal")
                .take();
            return None;
        };
        self.policy_retry
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let hardware_limit = hardware
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(crate::admission::DEFAULT_MAX_HW_SESSIONS);
        let software_budget = software
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(crate::admission::software_budget);
        let mut queued = self.queued.lock().expect("VOD encoder admission");
        // Preserve ordinary promotion's post-policy-read observation.
        let priority = demand_priority
            .or(preparation_priority)
            .unwrap_or_else(|| self.priority());
        if priority == Priority::Live {
            queued.get_or_insert_with(|| self.admissions.queue_for_slot());
        } else {
            // A previous consumer may have left a Live wait on this cached
            // encoding. Its replacement's preparation must not inherit it.
            queued.take();
        }
        let refuse = |over_budget: bool| {
            *self.last_refusal.lock().expect("VOD encoder refusal") = Some(PermitRefusal {
                priority,
                hardware_limit,
                software_budget,
                over_budget,
                pool: self.admissions.snapshot(),
            });
            None
        };
        // A shared-audio producer admits its own bounded AAC estimate, never
        // the video recipe it was resolved beside.
        let claim = (priority != Priority::Background)
            .then(|| {
                self.handoff_claim
                    .lock()
                    .expect("VOD handoff claim")
                    .clone()
            })
            .flatten();
        let bundle = match try_admit_frozen_bundle(
            &self.admissions,
            hardware_limit,
            software_budget,
            &self.resources(),
            self.admission_software_threads(),
            priority,
            claim.as_ref().map(|claim| claim.token),
        ) {
            Ok(bundle) => bundle,
            Err(over_budget) => {
                if !over_budget {
                    if let Some(waiting) = queued.as_mut() {
                        waiting.mark_capacity_blocked();
                    }
                }
                return refuse(over_budget);
            }
        };
        let permit = EncodePermit::from(bundle);
        queued.take();
        if let Some(claim) = claim {
            claim
                .admitted
                .store(true, std::sync::atomic::Ordering::Release);
            let mut pending = self.handoff_claim.lock().expect("VOD handoff claim");
            if pending
                .as_ref()
                .is_some_and(|current| current.token == claim.token)
            {
                pending.take();
            }
        }
        self.handoff_wait
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.last_refusal
            .lock()
            .expect("VOD encoder refusal")
            .take();
        Some(permit)
    }

    /// The video recipe's software-encoder cap does not bound the AAC
    /// producer: shared audio's fixed estimate is its whole floor.
    fn admission_software_threads(&self) -> Option<u32> {
        self.shared_audio
            .is_none()
            .then_some(self.options.software_threads)
            .flatten()
    }

    /// `Some(floor)` when no permit can ever be granted under
    /// `software_budget`: the frozen plan alone needs more CPU threads than
    /// the whole budget. The same rule admission applies, asked before a
    /// rendition is named, so such a plan is refused at create instead of
    /// parking demand on the materialize deadline.
    pub(crate) fn unadmittable_software_floor(&self, software_budget: usize) -> Option<usize> {
        let floor = frozen_admission_floor(&self.resources(), self.admission_software_threads());
        (floor > software_budget).then_some(floor)
    }

    pub fn cancel_wait(&self) {
        self.queued.lock().expect("VOD encoder admission").take();
        self.policy_retry
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.handoff_wait
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(claim) = self.handoff_claim.lock().expect("VOD handoff claim").take() {
            self.admissions.release_reservation(claim.token);
        }
    }

    /// Install the reservation a yielding predecessor made for this rendition,
    /// releasing any earlier one it replaces.
    pub(crate) fn accept_handoff_claim(
        &self,
        token: u64,
        admitted: Arc<std::sync::atomic::AtomicBool>,
    ) {
        if let Some(earlier) = self
            .handoff_claim
            .lock()
            .expect("VOD handoff claim")
            .replace(HandoffClaim { token, admitted })
        {
            self.admissions.release_reservation(earlier.token);
        }
    }

    pub fn is_waiting(&self) -> bool {
        self.policy_retry.load(std::sync::atomic::Ordering::Relaxed)
            || self.handoff_wait.load(std::sync::atomic::Ordering::Relaxed)
            || self.has_live_wait()
    }

    pub(crate) fn last_refusal(&self) -> Option<PermitRefusal> {
        *self.last_refusal.lock().expect("VOD encoder refusal")
    }

    /// Whether returning a permit of `released`'s size would let this exact
    /// speculative plan start, under the limits its last refusal read.
    pub(crate) fn fits_after_release(&self, released: &TranscodeResourceEstimate) -> bool {
        let Some(refusal) = self.last_refusal() else {
            return false;
        };
        !refusal.over_budget
            && self.admissions.speculative_fits_after_release(
                refusal.hardware_limit,
                refusal.software_budget,
                &self.resources(),
                released,
            )
    }

    /// Whether this rendition is queued as a live start that returning a
    /// permit of `released`'s size would admit, under the limits its last
    /// refusal read. False for a plan no release can admit.
    pub(crate) fn live_start_fits_after_release(
        &self,
        released: &TranscodeResourceEstimate,
    ) -> bool {
        if !self.has_live_wait() {
            return false;
        }
        let Some(refusal) = self.last_refusal() else {
            return false;
        };
        !refusal.over_budget
            && self.admissions.live_fits_after_release(
                refusal.hardware_limit,
                refusal.software_budget,
                &self.resources(),
                released,
            )
    }

    /// Whether this exact rendition has registered a foreground pool waiter.
    /// Kept separate from policy retry so only a real capacity wait wakes
    /// other renditions to consider yielding their permit.
    pub fn has_live_wait(&self) -> bool {
        self.queued.lock().expect("VOD encoder admission").is_some()
    }

    pub(crate) fn resources(&self) -> TranscodeResourceEstimate {
        if self.shared_audio.is_some() {
            TranscodeResourceEstimate {
                hardware_slot: false,
                cpu_threads: plurx_core::transcode::VOD_SHARED_AUDIO_CPU_THREADS,
                decoder_threads: Some(1),
            }
        } else {
            self.resources
        }
    }

    pub(crate) fn media_plan(&self, duration_ms: i64) -> plurx_core::segplan::SegmentPlan {
        if let Some(audio) = &self.shared_audio {
            audio.plan_on_grid(duration_ms, self.grid)
        } else {
            // Typed audio delivery owns the budget; a copied or downmixed
            // track is never planned at the transitional scalar rate.
            let audio_rate = if self.plan.options().input_has_audio {
                self.options.audio_budget_kbps()
            } else {
                0
            };
            self.grid.plan(
                duration_ms,
                u64::from(self.options.video_bitrate_kbps.saturating_add(audio_rate)) * 1000,
            )
        }
    }

    /// Container-inclusive delivery ceiling, enforced before publication and
    /// again on cached child delivery. Average rate stays unknown for JIT media.
    pub(crate) fn continuous_peak_bps(
        &self,
        plan: &plurx_core::segplan::SegmentPlan,
    ) -> Option<u64> {
        if self.shared_audio.is_none()
            && (self.plan.options().input_has_audio
                || self.options.video_sample_envelope
                    != plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50)
        {
            return None;
        }
        let shortest = plan
            .entries
            .iter()
            .map(|entry| entry.duration_ticks)
            .min()?;
        if shortest == 0 || plan.timescale == 0 {
            return None;
        }
        let (rate, burst) = if self.shared_audio.is_some() {
            (
                u128::from(self.options.audio_bitrate_kbps) * 2_000,
                64 * 1024 * 8,
            )
        } else {
            let nominal = u128::from(self.options.video_bitrate_kbps) * 1_000;
            (nominal * 3, nominal * 2 + 256 * 1024 * 8)
        };
        u64::try_from(rate + (burst * u128::from(plan.timescale)).div_ceil(u128::from(shortest)))
            .ok()
    }

    /// The rate this continuous rendition needs from a link: the encoder's
    /// VBV cap for video, the configured rate for the shared soundtrack. It is
    /// the same figure the candidate catalog advertises for the encode route,
    /// split by role. [`Self::continuous_peak_bps`] stays the publication
    /// ceiling; nothing enforces this value, and it is always below it.
    pub(crate) fn continuous_sustained_bps(&self) -> Option<u64> {
        if self.shared_audio.is_some() {
            return Some(u64::from(self.options.audio_bitrate_kbps) * 1000);
        }
        if self.plan.options().input_has_audio
            || self.options.video_sample_envelope
                != plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
        {
            return None;
        }
        Some(plurx_core::transcode::vbv_maxrate_bps(
            self.options.video_bitrate_kbps,
        ))
    }

    pub(crate) fn continuous_object_fits(
        &self,
        plan: &plurx_core::segplan::SegmentPlan,
        index: u32,
        bytes: u64,
    ) -> Option<bool> {
        let peak = self.continuous_peak_bps(plan)?;
        let entry = plan.entry(index)?;
        Some(
            u128::from(bytes) * 8 * u128::from(plan.timescale)
                <= u128::from(peak) * u128::from(entry.duration_ticks),
        )
    }

    pub fn args(&self, file: &MediaFile, start_seconds: f64, duration_seconds: f64) -> Vec<String> {
        let mut options = self.options.clone();
        options.start_seconds = start_seconds;
        let execution = TranscodeExecution::from_options(file, &options, Pacing::unpaced(), ".")
            .expect("frozen VOD execution remains valid");
        #[cfg(target_os = "linux")]
        if let Some(runtime) = &self.dv_runtime {
            let output = self
                .plan
                .completed_reconstructed_output(runtime.encoder)
                .expect("verified reconstructed output encoder")
                .with_vaapi_hdr_policy(runtime.vaapi_hdr_policy);
            return plurx_core::transcode::vod_completed_reconstructed_pipe_args(
                file,
                &output,
                &execution,
                self.grid,
                duration_seconds,
                "/proc/self/fd/3",
                "/proc/self/fd/4",
                self.source_clock,
            )
            .expect("verified reconstructed VOD recipe remains valid");
        }
        if let Some(audio) = &self.shared_audio {
            // This input is the source duration, not an already rounded plan
            // end. Re-rounding the latter could add another whole video frame.
            let duration_ms = (duration_seconds * 1_000.0).round() as i64;
            let end_seconds = self.grid.shared_audio_end_ticks(duration_ms) as f64
                / f64::from(plurx_core::transcode::VOD_AUDIO_RATE);
            audio
                .args_with_source_clock(&execution, end_seconds, self.source_clock)
                .expect("frozen soundtrack execution remains valid")
        } else {
            vod_pipe_args_with_source_clock(
                file,
                &self.plan,
                &execution,
                self.grid,
                duration_seconds,
                self.reorder_frames,
                self.source_clock,
            )
        }
    }

    /// The encoder that produces this encoding's bytes when it is an admitted,
    /// permitted processing graph: the graph's window encoder, never the
    /// software source-decode plan that only built its input contract. `None`
    /// for every ordinary encoding, whose plan encoder is the delivered one.
    /// The one place that distinction is made; every report derives from it.
    pub(crate) fn processed_window_encoder(&self) -> Option<plurx_core::transcode::Encoder> {
        #[cfg(target_os = "linux")]
        if let Some(runtime) = &self.dv_runtime {
            if matches!(&self.dv_processing,
                plurx_core::transcode::dv_processing::DvSelection::Selected(plan)
                    if plurx_core::transcode::dv_processing::DvProductionRegistry.permits(plan))
            {
                return Some(runtime.encoder);
            }
        }
        None
    }

    /// Human label of the delivered encoder, for `StartInfo::encoder`.
    pub(crate) fn delivered_encoder_label(&self) -> &'static str {
        match self.processed_window_encoder() {
            Some(plurx_core::transcode::Encoder::Software) => "software (x265)",
            Some(encoder) => encoder.label(),
            None => self.plan.encoder().label(),
        }
    }

    /// FFmpeg name of the delivered video encoder, for the stream format a
    /// session publishes: the processing graph's completed-window encoder
    /// (`hevc_vaapi`, `hevc_nvenc`, `libx265`) when one is admitted, otherwise
    /// the frozen plan's output encoder.
    pub(crate) fn delivered_output_encoder(&self) -> String {
        self.processed_window_encoder()
            .and_then(|encoder| {
                self.plan
                    .completed_reconstructed_output(encoder)
                    .map(|output| output.encoder_name().to_owned())
            })
            .unwrap_or_else(|| self.plan.output_contract().output_encoder().to_owned())
    }

    pub(crate) fn preserves_processed_dv(&self) -> bool {
        matches!(&self.dv_processing,
            plurx_core::transcode::dv_processing::DvSelection::Selected(plan)
                if plurx_core::transcode::dv_processing::DvProductionRegistry.permits(plan)
                    && plan.destination() == plurx_core::transcode::dv_processing::DvDestination::Profile81)
    }
    /// Whether the admitted processing plan reconstructs the FEL; `None`
    /// when no plan is selected.
    pub(crate) fn delivered_output_grade(&self) -> plurx_core::transcode::OutputGrade {
        if matches!(&self.dv_processing, plurx_core::transcode::dv_processing::DvSelection::Selected(plan) if plan.destination() == plurx_core::transcode::dv_processing::DvDestination::Sdr)
        {
            plurx_core::transcode::OutputGrade::Sdr
        } else {
            self.options.pipeline.output_grade()
        }
    }
    pub(crate) fn processed_fel(&self) -> Option<bool> {
        match &self.dv_processing {
            plurx_core::transcode::dv_processing::DvSelection::Selected(plan)
                if plurx_core::transcode::dv_processing::DvProductionRegistry.permits(plan) =>
            {
                Some(plan.reconstructs_fel())
            }
            _ => None,
        }
    }
    pub(crate) fn processed_dv_supplemental(&self) -> Option<String> {
        if !self.preserves_processed_dv() {
            return None;
        }
        let plurx_core::transcode::dv_processing::DvSelection::Selected(plan) = &self.dv_processing
        else {
            return None;
        };
        plan.profile81_level()
            // Profile 8.1 is HDR10-compatible. Apple requires the compatibility
            // brand alongside VIDEO-RANGE=PQ to recognize the DV enhancement.
            .map(|level| format!("dvhe.08.{level:02}/db1p"))
    }

    pub(crate) fn processed_candidate_digest(
        &self,
        file: &MediaFile,
        planning: &crate::media_pool::PlanningBinding,
    ) -> Result<[u8; 32], String> {
        use plurx_core::transcode::dv_processing::{DvDestination, DvSelection};
        // HDR10-E and the reconstructed Profile 8.1 are both processed
        // candidates; the destination is part of their identity.
        let tag = match &self.dv_processing {
            DvSelection::Selected(plan) if plan.destination() == DvDestination::Sdr => {
                "plurx:processed-sdr-candidate:v1"
            }
            DvSelection::Selected(plan) if plan.destination() == DvDestination::Hdr10 => {
                "plurx:processed-hdr10-candidate:v1"
            }
            DvSelection::Selected(plan) if plan.destination() == DvDestination::Profile81 => {
                "plurx:processed-profile81-candidate:v1"
            }
            _ => return Err("candidate is not a completed processed recipe".into()),
        };
        if self.options.pipeline.output_grade() != plurx_core::transcode::OutputGrade::Hdr10
            || self.shared_audio.is_some()
        {
            return Err("candidate is not a completed processed recipe".into());
        }
        let duration = file
            .duration_ms
            .filter(|duration| *duration > 0)
            .ok_or("processed candidate requires the full source duration")?;
        let identity = self.identity(file, duration as f64 / 1000.0);
        let bytes = serde_json::to_vec(&(
            tag,
            planning,
            identity.size,
            identity.mtime_ms,
            identity.argv_fingerprint,
        ))
        .map_err(|error| error.to_string())?;
        Ok(Sha256::digest(bytes).into())
    }

    pub fn identity(&self, file: &MediaFile, duration_seconds: f64) -> SourceIdentity {
        let mut hash = Sha256::new();
        hash.update(if self.shared_audio.is_some() {
            b"immutable-vod-shared-aac-v1\0".as_slice()
        } else {
            b"immutable-vod-encoded-v1\0".as_slice()
        });
        hash.update((self.source_object_version.len() as u64).to_le_bytes());
        hash.update(self.source_object_version.as_bytes());
        hash.update(self.source_clock.identity());
        hash.update(self.ffmpeg_build.as_bytes());
        hash.update(self.executable.digest.as_bytes());
        hash.update(self.engine.digest.as_bytes());
        if let plurx_core::transcode::dv_processing::DvSelection::Selected(plan) =
            &self.dv_processing
        {
            // Only a byte-changing qualified graph enters the output key.
            // Disabled/unavailable preferences keep the existing cache identity.
            hash.update(b"plurx.vod.dv-processing.v1\0");
            hash.update(plan.semantic_digest().as_str().as_bytes());
        }
        hash.update(
            self.shared_audio
                .as_ref()
                .map_or_else(
                    || self.plan.plan_digest(),
                    |audio| audio.digest().to_owned(),
                )
                .as_bytes(),
        );
        if self.shared_audio.is_none() {
            hash.update(b"vod-reorder-choice-v1\0");
            hash.update([u8::from(self.reorder_frames)]);
        }
        for argument in self.args(file, 0.0, duration_seconds) {
            hash.update((argument.len() as u64).to_le_bytes());
            hash.update(argument.as_bytes());
        }
        // The same descriptor path may hold different selected subtitle
        // streams; argv alone cannot name their source-stream identity.
        if self.shared_audio.is_none() {
            if let Some(burn) = &self.options.subtitle_burn {
                hash.update(burn.subtitle_index.to_le_bytes());
                hash.update([u8::from(burn.bitmap)]);
            }
            if let Some(digest) = &self.subtitle_digest {
                hash.update(digest.as_bytes());
            }
        }
        SourceIdentity::new(
            file.size.max(0) as u64,
            file.mtime,
            hex::encode(hash.finalize()),
        )
    }
}

/// Hash the actual retained extraction output before handing its capability
/// to an encoder. The descriptor is rewound before publication to a recipe.
pub(crate) async fn digest_subtitle(file: &std::fs::File) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut input = tokio::fs::File::from_std(file.try_clone().map_err(|error| error.to_string())?);
    let read = async {
        input.rewind().await?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = input.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > 64 * 1024 * 1024 {
                return Err(std::io::Error::other("burn sidecar exceeds its size bound"));
            }
            hash.update(&buffer[..count]);
        }
        input.rewind().await?;
        Ok::<_, std::io::Error>(hex::encode(hash.finalize()))
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), read)
        .await
        .map_err(|_| "hashing the held burn sidecar exceeded five seconds".to_owned())?
        .map_err(|error| error.to_string())
}

/// Pick a declared output cadence from probe evidence. VFR is deliberately
/// normalized by the production fps filter; this does not infer that the
/// source has constant inter-frame intervals.
pub(crate) fn frame_grid(probe: Option<&str>) -> Option<VodFrameGrid> {
    let probe: serde_json::Value = serde_json::from_str(probe?).ok()?;
    probe.get("streams")?.as_array()?.iter().find_map(|stream| {
        if stream.get("codec_type")?.as_str()? != "video" {
            return None;
        }
        ["avg_frame_rate", "r_frame_rate"].iter().find_map(|key| {
            let (numerator, denominator) = stream.get(key)?.as_str()?.split_once('/')?;
            VodFrameGrid::new(numerator.parse().ok()?, denominator.parse().ok()?)
        })
    })
}

#[cfg(test)]
mod production_evidence_tests {
    use super::*;

    #[test]
    fn processed_helpers_measure_contiguous_windows_only_under_uninterrupted_exact_credit() {
        let admissions = Admissions::new();
        let reserve = || -> EncodePermit {
            admissions
                .try_admit_bundle(
                    2,
                    4,
                    &TranscodeResourceEstimate {
                        hardware_slot: true,
                        cpu_threads: 2,
                        decoder_threads: None,
                    },
                    Priority::Live,
                )
                .expect("bounded processing reservation")
                .into()
        };
        let first_credit = reserve();
        let other_credit = reserve();
        let now = Instant::now();
        let mut window = ActiveProductionWindow::default();
        window.begin_processed_window(1, 10, &first_credit);
        assert!(window
            .observe(now - Duration::from_secs(2), 1, 10, 44_000)
            .is_none());
        window.begin_processed_window(2, 11, &first_credit);
        assert!(
            window
                .observe(now - Duration::from_secs(1), 1, 11, 48_000)
                .is_none(),
            "old generation is fenced"
        );
        assert!(window
            .observe(now - Duration::from_secs(1), 2, 11, 48_000)
            .is_none());
        window.begin_processed_window(3, 12, &first_credit);
        let proof = window
            .observe(now, 3, 12, 52_000)
            .expect("two real contiguous intervals");
        assert_eq!(proof.milli_realtime, 4000);
        assert_eq!(proof.active_ms, 2000);
        assert_eq!(proof.completed_segments, 2);
        window.begin_processed_window(4, 13, &other_credit);
        assert!(
            window.evidence.is_none(),
            "different entitlement resets proof"
        );
        assert!(window.points.is_empty());
        window.observe(now, 4, 13, 56_000);
        window.begin_processed_window(5, 80, &other_credit);
        assert!(window.points.is_empty(), "seek resets proof");
        window.observe(now, 5, 80, 324_000);
        window.active = false;
        window.begin_processed_window(6, 81, &other_credit);
        assert!(window.points.is_empty(), "hold resets proof");
        window.observe(now - Duration::from_secs(16), 6, 81, 328_000);
        window.begin_processed_window(7, 82, &other_credit);
        assert!(window.points.is_empty(), "expired episode resets proof");
        drop(first_credit);
        drop(other_credit);
        assert_eq!(
            admissions.software_in_use(),
            0,
            "weak continuity cannot retain capacity"
        );
    }

    #[test]
    fn active_rate_excludes_startup_and_requires_two_complete_intervals() {
        let now = Instant::now();
        let mut window = ActiveProductionWindow {
            generation: 7,
            active: true,
            ..Default::default()
        };
        assert!(window.observe(now, 7, 10, 44_000).is_none());
        assert!(window
            .observe(now + Duration::from_secs(1), 7, 11, 48_000)
            .is_none());
        let proof = window
            .observe(now + Duration::from_secs(2), 7, 12, 52_000)
            .expect("two completed active intervals");
        assert_eq!(proof.milli_realtime, 4000);
        assert_eq!(proof.active_ms, 2000);
        assert_eq!(proof.completed_segments, 2);
    }

    #[test]
    fn production_proof_cannot_bridge_generation_hold_or_noncontiguous_seek() {
        let now = Instant::now();
        let mut window = ActiveProductionWindow {
            generation: 3,
            active: true,
            ..Default::default()
        };
        window.observe(now, 3, 1, 8_000);
        window.observe(now + Duration::from_secs(1), 3, 2, 12_000);
        assert!(window
            .observe(now + Duration::from_secs(2), 4, 3, 16_000)
            .is_none());
        assert!(window
            .observe(now + Duration::from_secs(3), 3, 50, 204_000)
            .is_none());
        assert!(window.evidence.is_none());
        assert_eq!(window.points.len(), 1);
        window.active = false;
        assert!(window
            .observe(now + Duration::from_secs(5), 3, 51, 208_000)
            .is_none());
        assert_eq!(window.points.len(), 1);
    }

    #[test]
    fn exact_recipe_production_proof_expires_without_refresh() {
        let proofs = CandidateProductionProofs::default();
        let recipe = [7; 32];
        proofs.record(
            recipe,
            ActiveProductionEvidence {
                milli_realtime: 1600,
                active_ms: 2500,
                completed_segments: 2,
                observed_at: Instant::now(),
            },
        );
        assert!(proofs.get(recipe).is_some());
        assert!(proofs.get([8; 32]).is_none());
        proofs.record(
            recipe,
            ActiveProductionEvidence {
                milli_realtime: 1600,
                active_ms: 2500,
                completed_segments: 2,
                observed_at: Instant::now() - Duration::from_secs(16),
            },
        );
        assert!(proofs.get(recipe).is_none());
    }
}

#[cfg(test)]
mod source_copy_admission_tests {
    use super::*;
    use plurx_core::store::SettingsStore;

    #[tokio::test]
    async fn processed_preparation_requests_background_yield_and_owns_actual_capacity() {
        let admissions = Admissions::new();
        let resources = TranscodeResourceEstimate {
            hardware_slot: true,
            cpu_threads: 5,
            decoder_threads: Some(1),
        };
        let background = admissions
            .try_admit_bundle(2, 5, &resources, Priority::Background)
            .expect("background occupies the pool");
        let pending = {
            let admissions = admissions.clone();
            tokio::spawn(async move {
                admit_frozen_preparation(
                    &admissions,
                    2,
                    5,
                    &resources,
                    Some(1),
                    Instant::now() + Duration::from_secs(2),
                )
                .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            while !admissions.live_capacity_is_waiting() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("foreground demand reaches background owner");
        assert!(
            !pending.is_finished(),
            "a running background owner is never discounted"
        );
        assert_eq!(admissions.software_in_use(), 5);
        drop(background);
        let owned = pending
            .await
            .expect("preparation task")
            .expect("reaped capacity admitted");
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        assert_eq!(admissions.software_in_use(), 5);
        assert!(admissions
            .try_admit_bundle(2, 5, &resources, Priority::Background)
            .is_none());
        drop(owned);
        assert_eq!(admissions.software_in_use(), 0);
    }

    #[tokio::test]
    async fn processed_preparation_deadline_and_cancellation_release_foreground_demand() {
        let admissions = Admissions::new();
        let resources = TranscodeResourceEstimate {
            hardware_slot: false,
            cpu_threads: 5,
            decoder_threads: Some(1),
        };
        assert!(
            admit_frozen_preparation(&admissions, 2, 5, &resources, Some(1), Instant::now())
                .await
                .is_err(),
            "an idle pool cannot admit an expired preparation"
        );
        assert_eq!(admissions.software_in_use(), 0);
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        let immediate = admit_frozen_preparation(
            &admissions,
            2,
            5,
            &resources,
            Some(1),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("idle capacity admits without pressure");
        assert!(!admissions.live_capacity_is_waiting());
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        drop(immediate);
        let held = admissions
            .try_admit_bundle(2, 5, &resources, Priority::Live)
            .expect("live owner");
        assert!(
            admit_frozen_preparation(&admissions, 2, 5, &resources, Some(1), Instant::now())
                .await
                .is_err()
        );
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        assert!(admit_frozen_preparation(
            &admissions,
            2,
            4,
            &resources,
            Some(5),
            Instant::now() + Duration::from_secs(60)
        )
        .await
        .is_err());
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        // Foreground work holds the capacity and nothing background can
        // yield: the preparation is refused at once, however long its
        // deadline, and registers no capacity demand.
        let started = Instant::now();
        assert!(admit_frozen_preparation(
            &admissions,
            2,
            5,
            &resources,
            Some(1),
            Instant::now() + Duration::from_secs(60),
        )
        .await
        .is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        assert_eq!(admissions.software_in_use(), 5);
        drop(held);
    }

    /// The one case the wait exists for: background work owns the capacity,
    /// so the preparation registers its foreground demand (which tells that
    /// work to yield) and waits for the release, inside its deadline.
    #[tokio::test]
    async fn processed_preparation_waits_only_for_background_work_that_can_yield() {
        let admissions = Admissions::new();
        let resources = TranscodeResourceEstimate {
            hardware_slot: false,
            cpu_threads: 5,
            decoder_threads: Some(1),
        };
        let background = admissions
            .try_admit_bundle(2, 5, &resources, Priority::Background)
            .expect("background owner");
        assert!(admissions.background_is_active());
        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            admit_frozen_preparation(
                &admissions,
                2,
                5,
                &resources,
                Some(1),
                Instant::now() + Duration::from_secs(60),
            ),
        )
        .await;
        assert!(cancelled.is_err(), "held background capacity is waited for");
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(!admissions.live_capacity_is_waiting());
        let releasing = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            drop(background);
        });
        let admitted = admit_frozen_preparation(
            &admissions,
            2,
            5,
            &resources,
            Some(1),
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("the yielded capacity admits the preparation");
        releasing.await.expect("release task");
        assert_eq!(admissions.software_in_use(), 5);
        drop(admitted);
        assert_eq!(admissions.software_in_use(), 0);
        assert!(!admissions.live_capacity_is_waiting());
    }

    /// The handoff gap `admit_live` closes. A copy start waiting for CPU must
    /// stay queued across its attempts, or background work is not told to
    /// yield and may take the capacity back in the gap between two of them.
    #[tokio::test]
    async fn a_source_copy_wait_stays_queued_until_capacity_frees() {
        let store = plurx_core::store::SqliteStore::open_in_memory().expect("policy store");
        store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "4")
            .await
            .expect("policy");
        let admissions = Admissions::new();
        let copy = TranscodeResourceEstimate {
            hardware_slot: false,
            cpu_threads: SOURCE_COPY_CPU_THREADS,
            decoder_threads: Some(1),
        };
        let held = admissions
            .try_admit_bundle(1, 4, &copy, Priority::Live)
            .expect("the whole software pool");
        let waiting = {
            let admissions = admissions.clone();
            tokio::spawn(async move {
                EncodePermit::admit_source_copy(
                    &admissions,
                    &store,
                    Instant::now() + Duration::from_secs(10),
                )
                .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !admissions.live_is_waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the copy start queues");
        // Several refused attempts later, it is still the one queued start.
        tokio::time::sleep(crate::transcode::ADMISSION_POLL * 3).await;
        assert_eq!(admissions.snapshot().live_waiting, 1);
        assert!(
            admissions.live_capacity_is_waiting(),
            "a genuinely blocked source copy still preempts preparation"
        );
        assert!(!waiting.is_finished());
        drop(held);
        let admitted = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the freed capacity admits the waiter")
            .expect("admission task");
        assert!(matches!(admitted, SourceCopyPermitRead::Admitted(_)));
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert!(
            !admissions.live_capacity_is_waiting(),
            "grant releases capacity pressure"
        );
        assert_eq!(admissions.software_in_use(), SOURCE_COPY_CPU_THREADS);
        drop(admitted);
        assert_eq!(admissions.software_in_use(), 0);
    }

    /// A budget below the copy's own estimate cannot change during one wait,
    /// which reads policy once, so it is refused without waiting out the
    /// deadline or leaving a queued start behind.
    #[tokio::test]
    async fn a_source_copy_wait_refuses_an_insufficient_budget_at_once() {
        let store = plurx_core::store::SqliteStore::open_in_memory().expect("policy store");
        store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "3")
            .await
            .expect("policy");
        let admissions = Admissions::new();
        let read = tokio::time::timeout(
            Duration::from_secs(2),
            EncodePermit::admit_source_copy(
                &admissions,
                &store,
                Instant::now() + Duration::from_secs(30),
            ),
        )
        .await
        .expect("refused without waiting for the deadline");
        assert!(matches!(read, SourceCopyPermitRead::Capacity));
        assert_eq!(admissions.snapshot().live_waiting, 0);
        assert_eq!(admissions.software_in_use(), 0);
    }
}
