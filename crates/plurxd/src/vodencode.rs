//! Frozen encoded-VOD recipes and per-generation foreground capacity.

use std::sync::{Arc, Mutex};

use plurx_core::domain::MediaFile;
use plurx_core::segplan::SourceIdentity;
use plurx_core::transcode::{
    vod_pipe_args, Pacing, ResolvedTranscode, TranscodeExecution, TranscodeOptions, VodFrameGrid,
};
use sha2::{Digest, Sha256};

use crate::admission::{
    Admissions, HwSlot, LiveWait, PoolSnapshot, Priority, SwPermit, TranscodeResourceEstimate,
};

/// Resolved once before attachment. A restart cannot silently change encoder,
/// grade, cadence, rate control, tracks, or burn pixels under an immutable URI.
pub(crate) struct Encoding {
    pub source_object_version: String,
    pub plan: ResolvedTranscode,
    pub resources: TranscodeResourceEstimate,
    pub options: TranscodeOptions,
    pub grid: VodFrameGrid,
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
    #[cfg(test)]
    pub admission_pause: Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl std::fmt::Debug for Encoding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Encoding")
            .field("decoder", &self.plan.decode().backend())
            .field("encoder", &self.plan.encoder())
            .field("resources", &self.resources)
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

/// Kept by the pipe owner until the exact process has been reaped. The permit
/// is not kept by a dormant rendition, an HTTP waiter, or a cache hit.
#[derive(Debug)]
pub(crate) struct EncodePermit {
    _hardware: Option<HwSlot>,
    _software: Option<SwPermit>,
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
            resources: self.resources,
            options: self.options.clone(),
            grid: self.grid,
            subtitle: self.subtitle.clone(),
            subtitle_digest: self.subtitle_digest.clone(),
            ffmpeg_build: self.ffmpeg_build.clone(),
            executable: crate::ffmpeg::EncodedExecutable::capture()
                .await
                .expect("test encoder executable remains available"),
            engine: self.engine.clone(),
            admissions,
            store: Arc::clone(&self.store),
            speculative: std::sync::atomic::AtomicBool::new(
                self.speculative.load(std::sync::atomic::Ordering::Acquire),
            ),
            queued: Mutex::new(None),
            policy_retry: std::sync::atomic::AtomicBool::new(false),
            handoff_wait: std::sync::atomic::AtomicBool::new(false),
            last_refusal: Mutex::new(None),
            admission_pause: Mutex::new(None),
        })
    }

    pub(crate) fn mark_speculative(&self) {
        self.speculative
            .store(true, std::sync::atomic::Ordering::Release);
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
        #[cfg(test)]
        let pause = self
            .admission_pause
            .lock()
            .expect("admission test seam")
            .take();
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.wait().await;
            pause.wait().await;
        }
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
        if self.resources.cpu_threads > software_budget {
            return refuse(true);
        }
        let Some(bundle) = self.admissions.try_admit_bundle(
            hardware_limit,
            software_budget,
            &self.resources,
            priority,
        ) else {
            return refuse(false);
        };
        let (hardware, software) = bundle.into_parts();
        let permit = EncodePermit {
            _hardware: hardware,
            _software: software,
        };
        queued.take();
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
                &self.resources,
                released,
            )
    }

    /// Whether this exact rendition has registered a foreground pool waiter.
    /// Kept separate from policy retry so only a real capacity wait wakes
    /// other renditions to consider yielding their permit.
    pub fn has_live_wait(&self) -> bool {
        self.queued.lock().expect("VOD encoder admission").is_some()
    }

    pub fn args(&self, file: &MediaFile, start_seconds: f64, duration_seconds: f64) -> Vec<String> {
        let mut options = self.options.clone();
        options.start_seconds = start_seconds;
        let execution = TranscodeExecution::from_options(file, &options, Pacing::unpaced(), ".")
            .expect("frozen VOD execution remains valid");
        vod_pipe_args(file, &self.plan, &execution, self.grid, duration_seconds)
    }

    pub fn identity(&self, file: &MediaFile, duration_seconds: f64) -> SourceIdentity {
        let mut hash = Sha256::new();
        hash.update(b"immutable-vod-encoded-v1\0");
        hash.update((self.source_object_version.len() as u64).to_le_bytes());
        hash.update(self.source_object_version.as_bytes());
        hash.update(self.ffmpeg_build.as_bytes());
        hash.update(self.executable.digest.as_bytes());
        hash.update(self.engine.digest.as_bytes());
        hash.update(self.plan.plan_digest().as_bytes());
        for argument in self.args(file, 0.0, duration_seconds) {
            hash.update((argument.len() as u64).to_le_bytes());
            hash.update(argument.as_bytes());
        }
        // The same descriptor path may hold different selected subtitle
        // streams; argv alone cannot name their source-stream identity.
        if let Some(burn) = &self.options.subtitle_burn {
            hash.update(burn.subtitle_index.to_le_bytes());
            hash.update([u8::from(burn.bitmap)]);
        }
        if let Some(digest) = &self.subtitle_digest {
            hash.update(digest.as_bytes());
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
