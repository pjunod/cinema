//! Frozen encoded-VOD recipes and per-generation foreground capacity.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plurx_core::domain::MediaFile;
use plurx_core::segplan::SourceIdentity;
use plurx_core::transcode::{
    vod_pipe_args_with_reorder, Pacing, ResolvedTranscode, TranscodeExecution, TranscodeOptions,
    VodFrameGrid,
};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::seam_hooks::AsyncPause;
use crate::seam_hooks::{HookFuture, HookReady};

use crate::admission::{
    Admissions, HwSlot, LiveWait, PoolSnapshot, Priority, SwPermit, TranscodeResourceEstimate,
};

/// Resolved once before attachment. A restart cannot silently change encoder,
/// grade, cadence, rate control, tracks, or burn pixels under an immutable URI.
pub(crate) struct Encoding {
    pub source_object_version: String,
    pub plan: ResolvedTranscode,
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
    pub handoff_claim: Mutex<Option<u64>>,
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
    points: VecDeque<(Instant, u32, i64)>,
    evidence: Option<ActiveProductionEvidence>,
}

impl ActiveProductionWindow {
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
impl RetainedEncodeAdmission {
    pub(crate) fn current(&self) -> Option<EncodePermit> {
        self.reservation
            .lock()
            .expect("retained rendition admission")
            .upgrade()
            .map(|reservation| EncodePermit {
                _reservation: reservation,
            })
    }

    pub(crate) fn bind(&self, admitted: EncodePermit) -> EncodePermit {
        let mut binding = self
            .reservation
            .lock()
            .expect("retained rendition admission");
        if let Some(existing) = binding.upgrade() {
            // An attachment arriving while ordinary admission was in flight
            // already owns this rendition's exact credit. Release the newly
            // admitted duplicate and preserve the existing worker claim.
            EncodePermit {
                _reservation: existing,
            }
        } else {
            *binding = Arc::downgrade(&admitted._reservation);
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

impl Encoding {
    #[cfg(test)]
    pub(crate) async fn clone_with_admissions_for_test(
        &self,
        admissions: Admissions,
    ) -> Arc<Encoding> {
        Arc::new(Encoding {
            source_object_version: self.source_object_version.clone(),
            plan: self.plan.clone(),
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

    /// The policy-read future is injected only to test the same production
    /// deadline without making an unresolved backend strand a test runner.
    pub(crate) async fn try_permit_after(
        &self,
        policy: impl std::future::Future<
            Output = Result<(Option<String>, Option<String>), plurx_core::error::StoreError>,
        >,
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
        self.policy_retry
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let hardware_limit = hardware
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(crate::admission::DEFAULT_MAX_HW_SESSIONS);
        let software_budget = software
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(crate::admission::software_budget);
        let mut queued = self.queued.lock().expect("VOD encoder admission");
        let priority = self.priority();
        if priority == Priority::Live {
            queued.get_or_insert_with(|| self.admissions.wait_for_slot());
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
        // The shared pool deliberately admits one oversize job when otherwise
        // idle. A frozen VOD recipe cannot shrink its thread demand on retry,
        // so an operator lowering the budget below that exact plan is an
        // explicit refusal rather than an oversize exception.
        if self.resources().cpu_threads > software_budget {
            return refuse(true);
        }
        let claim = *self.handoff_claim.lock().expect("VOD handoff claim");
        let Some(bundle) = self.admissions.try_admit_bundle_claiming(
            hardware_limit,
            software_budget,
            &self.resources(),
            priority,
            claim,
        ) else {
            return refuse(false);
        };
        let permit = EncodePermit::from(bundle);
        queued.take();
        self.handoff_claim.lock().expect("VOD handoff claim").take();
        self.handoff_wait
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.last_refusal
            .lock()
            .expect("VOD encoder refusal")
            .take();
        Some(permit)
    }

    pub fn cancel_wait(&self) {
        self.queued.lock().expect("VOD encoder admission").take();
        self.policy_retry
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.handoff_wait
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(token) = self.handoff_claim.lock().expect("VOD handoff claim").take() {
            self.admissions.release_reservation(token);
        }
    }

    /// Install the reservation a yielding predecessor made for this rendition,
    /// releasing any earlier one it replaces.
    pub(crate) fn accept_handoff_claim(&self, token: u64) {
        if let Some(earlier) = self
            .handoff_claim
            .lock()
            .expect("VOD handoff claim")
            .replace(token)
        {
            self.admissions.release_reservation(earlier);
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
            let audio_rate = if self.plan.options().input_has_audio {
                self.options.audio_bitrate_kbps
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
        if let Some(audio) = &self.shared_audio {
            // This input is the source duration, not an already rounded plan
            // end. Re-rounding the latter could add another whole video frame.
            let duration_ms = (duration_seconds * 1_000.0).round() as i64;
            let end_seconds = self.grid.shared_audio_end_ticks(duration_ms) as f64
                / f64::from(plurx_core::transcode::VOD_AUDIO_RATE);
            audio
                .args(&execution, end_seconds)
                .expect("frozen soundtrack execution remains valid")
        } else {
            vod_pipe_args_with_reorder(
                file,
                &self.plan,
                &execution,
                self.grid,
                duration_seconds,
                self.reorder_frames,
            )
        }
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
        hash.update(self.ffmpeg_build.as_bytes());
        hash.update(self.executable.digest.as_bytes());
        hash.update(self.engine.digest.as_bytes());
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
