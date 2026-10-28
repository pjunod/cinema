//! Shared application state and the background job manager.

#[path = "cache_preparation.rs"]
mod cache_preparation;
#[path = "library_work.rs"]
mod library_work;
#[path = "subtitle_work.rs"]
mod subtitle_work;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use plurx_core::cluster::coordination::LeaseClaim;
use plurx_core::cluster::coordination::{ClusterJobAuthority, StoreCoordinator};
#[cfg(test)]
use plurx_core::domain::ArtworkAttempt;
use plurx_core::domain::{
    BookMetadataPatch, BookMetadataSource, Item, ItemKind, Library, LibraryKind, MediaFile,
    MetadataPatch, NewPretranscodeJob, OfflinePackageStats, PlaybackEvent,
    PretranscodeRequirements,
};
use plurx_core::error::StoreError;
use plurx_core::metadata::book::BookEnrichReport;
use plurx_core::metadata::genres::GenreBackfillReport;
use plurx_core::metadata::local::LocalArtReport;
use plurx_core::metadata::{self, AniListClient, EnrichReport, TmdbClient};
use plurx_core::scan::{self, PlacedFile, ScanProgress, ScanReport, TargetError, TargetedScan};
use plurx_core::secrets::CredentialKey;
use plurx_core::store::{
    cluster_fragment_index_blob_sha256, cluster_fragment_index_generation_key,
    cluster_fragment_index_key, encode_cluster_fragment_index_blob, keys, AnalysisRequest,
    ArtworkRepairFence, CatalogueReader, ClusterFragmentIndexArtifact, DvConversionMode,
    DvConversionQueueBatch, DvConversionState, DvRecoveryGuardState, NewAnalysisRequest,
    NewClusterFragmentIndexJob, PrometheusStoreSnapshot, PublicationStore,
    QueueDvConversionOutcome, SeriesHintOutcome, Store, SubtitleSourceStamp,
    DV_CONVERSION_QUEUE_BATCH_MAX,
};
use plurx_core::transcode::EncoderCaps;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use crate::job_lease::{acquire_cluster_job, ActiveJobLease};
use crate::logbuf::{LogBuffer, LogBuffers};
use crate::offline::OfflineManager;
use crate::schedule::{due_jobs, DueJob, GlobalSchedule};
use crate::trakt::TraktManager;
use crate::transcode::{PretranscodeProduceOutcome, TranscodeManager};

const FRAGMENT_INDEX_VALIDATION_PAGE: u32 = 64;
const FRAGMENT_INDEX_VALIDATION_INTERVAL: Duration = Duration::from_secs(30);
static FRAGMENT_INDEX_VALIDATED: AtomicU64 = AtomicU64::new(0);
static FRAGMENT_INDEX_REFUSED: AtomicU64 = AtomicU64::new(0);
static FRAGMENT_INDEX_GONE: AtomicU64 = AtomicU64::new(0);

/// Playback may admit exact analysis before a media session has attached.
/// A process-wide broadcast reaches the local queue consumer without making
/// the VOD serve layer depend on a particular JobManager instance. Durable
/// rows remain authoritative if no consumer is listening during startup.
fn fragment_analysis_wake() -> &'static broadcast::Sender<()> {
    static WAKE: OnceLock<broadcast::Sender<()>> = OnceLock::new();
    WAKE.get_or_init(|| broadcast::channel(8).0)
}

pub(crate) fn fragment_index_validation_prometheus() -> String {
    format!(
        "# HELP plurx_index_validation_backfill_total Legacy fragment-index validation attempts by result.\n\
         # TYPE plurx_index_validation_backfill_total counter\n\
         plurx_index_validation_backfill_total{{result=\"validated\"}} {}\n\
         plurx_index_validation_backfill_total{{result=\"refused\"}} {}\n\
         plurx_index_validation_backfill_total{{result=\"gone\"}} {}\n",
        FRAGMENT_INDEX_VALIDATED.load(Ordering::Relaxed),
        FRAGMENT_INDEX_REFUSED.load(Ordering::Relaxed),
        FRAGMENT_INDEX_GONE.load(Ordering::Relaxed),
    )
}

pub(crate) fn node_hwaccel_key(node_id: &str) -> String {
    format!("node.{node_id}.transcode.hwaccel")
}

/// Environment facts collected once at startup, shown on the settings page.
/// Everything here is admin-facing diagnostics — paths, tool versions,
/// detected hardware — not runtime state.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SystemInfo {
    pub data_dir: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    /// First line of `ffmpeg -version`, if ffmpeg ran at all.
    pub ffmpeg_version: Option<String>,
    /// Digest of the canonical FFprobe executable bytes plus its complete
    /// bounded `-version` output. Decoder facts are scoped to this identity.
    pub ffprobe_build_digest: Option<String>,
    /// Runtime-only binding to the executable that produced the digest.
    #[serde(skip)]
    pub(crate) decode_probe_identity: Option<crate::decode_facts::DecodeProbeIdentity>,
    /// Backend preference applied at startup, including any node override.
    pub hwaccel_pref: String,
    /// Node-specific choice applied at startup with the matching tone-map probe.
    #[serde(skip)]
    pub hwaccel_override: Option<String>,
    pub encoders: EncoderCaps,
    #[serde(skip)]
    pub transcoder_optimization: Option<crate::transcoder_optimization::Report>,
    /// Portable video decoders reported by this exact ffmpeg at boot.
    pub decoders: Vec<String>,
    /// Which decoder that ffmpeg was measured to select for each of them.
    pub measured_decoders: plurx_core::transcode::decoder_inventory::MeasuredDecoders,
    /// Human label of the encoder the transcoder will actually pick.
    pub encoder_selected: String,
    /// What the tone-map probe found at boot: the graph this node uses, and
    /// what each rejected candidate failed on.
    ///
    /// Here rather than in a field of its own because it is the same kind of
    /// fact as the encoder list — something measured about this machine at
    /// startup and true for the process's life. Worth surfacing rather than
    /// leaving in the log, because falling back to the CPU chain is *silent*:
    /// everything plays, 4K just stays slow. "This box has no GPU tone-map"
    /// and "the driver refused the graph" are the difference between shrugging
    /// and installing a package.
    pub tone_map: crate::pipeprobe::PipelineReport,
    /// Input-pacing flags proved against this exact ffmpeg binary at boot.
    pub pacing: crate::ffmpeg::PacingCaps,
    /// Whether this ffmpeg can strip a Dolby Vision configuration
    /// (`dovi_rpu`, 7.1+) — probed at boot, not inferred from the version
    /// line. It decides a *verdict*, not just a filter argument: without it a
    /// DV source has to be re-encoded for any browser that cannot decode DV,
    /// instead of remuxed to its HDR10 base. Surfaced because the symptom
    /// (a 4K film quietly playing at the Auto rung in Chrome and perfectly in
    /// Safari) is otherwise unattributable from outside the machine.
    pub dovi_rpu: bool,
    /// Whether this exact ffmpeg can run the software-decode → tonemapx
    /// Dolby Vision reshape → software-encode graph.
    pub dovi_reshape: bool,
    /// Whether this exact ffmpeg can run the HDR10 rung: the same software
    /// decode into `tonemapx`'s HDR *passthrough* mode at 10 bit, into
    /// libx265 Main10. Separate from `dovi_reshape` because it is a separate
    /// probe of a separate graph — the SDR one proves nothing about a PQ
    /// output, where an 8-bit format aborts the process outright.
    pub dovi_passthrough: bool,
    /// Whether this node proved the P010 upload → QSV Main10 half of the
    /// Dolby Vision HDR10 route. Kept separate so a working software rung
    /// never implies a working GPU driver.
    pub dovi_passthrough_qsv: bool,
    /// Whether this exact ffmpeg can re-encode an ordinary HDR source without
    /// tone-mapping it — a 10-bit scale straight into HEVC Main10 PQ. The
    /// route for HDR10, HDR10+ and a stripped Dolby Vision base layer, which
    /// is nearly every HDR title; separate from `dovi_passthrough` because it
    /// touches no RPU and needs no Dolby-aware filter.
    pub hdr10_passthrough: bool,
    /// Whether this node proved the QSV half of that route — P010 upload into
    /// `hevc_qsv` Main10. Separate from `dovi_passthrough_qsv`, which is
    /// gated behind a Dolby Vision filter this route does not use.
    pub hdr10_passthrough_qsv: bool,
    /// Independent plain-HDR10 P010/Main10 VAAPI graph proof (1080p).
    pub hdr10_passthrough_vaapi: bool,
    /// Whether this build converts Dolby Vision Profile 7 to Profile 8.1 on
    /// the way through a copy (PLAYBACK-CAPS-V2-PLAN §4.8).
    ///
    /// Not a probe: the conversion is plurx's own code, so the answer is
    /// "this binary has it" — which is always true — narrowed by the
    /// `playback.dv_convert` setting an operator can turn off. It sits with
    /// the probes because a client asking why a title played as HDR10 rather
    /// than Dolby Vision needs all four answers in one place.
    pub dolby_vision_convert: bool,
    /// Pinned on-disk conversion tools measured at boot. Queue admission uses
    /// this exact record, so a missing binary is named before any media work
    /// starts instead of after a 60–80 GB extraction.
    pub dv_disk: crate::dv_disk::DvDiskCapabilities,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SubtitleBackfillStatus {
    pub(crate) lease_holder: Option<String>,
    pub(crate) enqueued_process: u64,
    pub(crate) remaining_files: i64,
    pub(crate) remaining_bytes: i64,
}

/// The daemon's managed directories across the configured storage roots.
///
/// Grouped rather than passed loose because the distinction between them
/// matters and is easy to get backwards positionally: `transcode` is scratch
/// and is wiped at every boot, while `cache` holds finished pre-transcodes
/// and must survive restarts — swapping the two would erase the cache on
/// startup and leave stale segments behind forever.
#[derive(Clone, Debug, Default)]
pub struct Dirs {
    pub artwork: PathBuf,
    pub transcode: PathBuf,
    pub cache: PathBuf,
    /// Extracted-subtitle cache (review §3.3). A sibling of the transcode
    /// cache under `cache/`, for the same reason that one is: it survives
    /// restarts, and the transcode scratch — which is cleared at boot and
    /// swept for orphans — must not contain it.
    pub subs: PathBuf,
    /// Disposable ffmpeg bookkeeping. It may be regenerated, but it must be
    /// resolved and fenced separately from live-session scratch so a symlink
    /// or bind alias cannot put scratch cleanup over another managed root.
    pub runtime_cache: PathBuf,
    /// Persistent VOD rendition bytes, including admitted copy-cache entries.
    /// Unlike `runtime_cache`, these survive restarts and cache relocation.
    pub renditions: PathBuf,
}

const STORE_METRICS_FRESHNESS_SECS: u64 = 120;

struct StoreMetricsAtomics {
    sequence: AtomicU64,
    published: AtomicBool,
    sampled_elapsed: AtomicU64,
    errors: AtomicU64,
    libraries: AtomicI64,
    users: AtomicI64,
    queued: AtomicI64,
    preparing: AtomicI64,
    ready: AtomicI64,
    failed: AtomicI64,
    queued_bytes: AtomicI64,
    preparing_bytes: AtomicI64,
    ready_bytes: AtomicI64,
    failed_bytes: AtomicI64,
    active_leases: AtomicI64,
    pinned_bytes: AtomicI64,
    outbox_pending: AtomicI64,
    outbox_ok: AtomicI64,
    outbox_failed: AtomicI64,
    background_job_counts: [AtomicI64; plurx_core::store::background_jobs::JOB_METRIC_SLOTS],
    background_job_oldest_age: [AtomicI64; plurx_core::store::background_jobs::JOB_METRIC_SLOTS],
    background_source_io: AtomicI64,
    background_legacy_pending: AtomicI64,
    analysis_queue_depth: [AtomicI64; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
    analysis_queue_oldest_age_seconds: [AtomicI64; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
    analysis_lifecycle_counts: [AtomicI64; plurx_core::store::ANALYSIS_LIFECYCLE_METRIC_SLOTS],
    analysis_marker_counts: [AtomicI64; plurx_core::store::ANALYSIS_MARKER_METRIC_SLOTS],
    analysis_ready_24h: AtomicI64,
    analysis_attempt_limit_24h: AtomicI64,
    analysis_claimed_24h: AtomicI64,
    analysis_claimable: AtomicI64,
    analysis_running_past_lease: AtomicI64,
    analysis_last_ready_at_ms: AtomicI64,
    /// The cumulative counters as this process first saw them. The store's own
    /// are cumulative since the schema landed, and a queue that built 12,000
    /// artifacts last month and nothing since looks identical to a healthy one
    /// through those. The delta is what says whether it works now.
    analysis_baseline_claims: AtomicI64,
    analysis_baseline_lease_losses: AtomicI64,
    analysis_baseline_seen: AtomicBool,
    /// Consecutive samples with a row `running` past its lease. One is a sweep
    /// that has not run yet; two is a queue that is not sweeping.
    analysis_running_past_lease_samples: AtomicU32,
}

impl Default for StoreMetricsAtomics {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            published: AtomicBool::new(false),
            sampled_elapsed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            libraries: AtomicI64::new(0),
            users: AtomicI64::new(0),
            queued: AtomicI64::new(0),
            preparing: AtomicI64::new(0),
            ready: AtomicI64::new(0),
            failed: AtomicI64::new(0),
            queued_bytes: AtomicI64::new(0),
            preparing_bytes: AtomicI64::new(0),
            ready_bytes: AtomicI64::new(0),
            failed_bytes: AtomicI64::new(0),
            active_leases: AtomicI64::new(0),
            pinned_bytes: AtomicI64::new(0),
            outbox_pending: AtomicI64::new(0),
            outbox_ok: AtomicI64::new(0),
            outbox_failed: AtomicI64::new(0),
            background_job_counts: std::array::from_fn(|_| AtomicI64::new(0)),
            background_job_oldest_age: std::array::from_fn(|_| AtomicI64::new(0)),
            background_source_io: AtomicI64::new(0),
            background_legacy_pending: AtomicI64::new(0),
            analysis_queue_depth: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_queue_oldest_age_seconds: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_lifecycle_counts: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_marker_counts: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_ready_24h: AtomicI64::new(0),
            analysis_attempt_limit_24h: AtomicI64::new(0),
            analysis_claimed_24h: AtomicI64::new(0),
            analysis_claimable: AtomicI64::new(0),
            analysis_running_past_lease: AtomicI64::new(0),
            analysis_last_ready_at_ms: AtomicI64::new(0),
            analysis_baseline_claims: AtomicI64::new(0),
            analysis_baseline_lease_losses: AtomicI64::new(0),
            analysis_baseline_seen: AtomicBool::new(false),
            analysis_running_past_lease_samples: AtomicU32::new(0),
        }
    }
}

/// Sum every lifecycle counter for one event, across its typed reasons.
fn lifecycle_total(analysis: &plurx_core::store::AnalysisStoreMetrics, event: &str) -> i64 {
    plurx_core::store::ANALYSIS_LIFECYCLE_METRICS
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| *name == event)
        .map(|(slot, _)| analysis.lifecycle_counts[slot])
        .sum()
}

/// What the queue has produced, and what this process has watched it do.
///
/// The two `since_start` figures are deltas against the first sample this
/// process took, so they reset on restart — which the field names say, because
/// an operator reading "0 claims" needs to know whether that means the queue
/// is quiet or the daemon is new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisQueueHealthReport {
    pub health: plurx_core::store::AnalysisQueueHealth,
    pub claims_since_start: i64,
    pub lease_losses_since_start: i64,
    pub verdict: plurx_core::store::AnalysisQueueVerdict,
}

/// Rate limit for the "queue is not producing" warning.
///
/// An hourly limit is only a limit while it is armed. Re-arming the moment one
/// sample comes back clean turns a verdict that flaps — a queue degraded by a
/// sweep that keeps almost catching up — into a warning on every bad sample,
/// which is how an hourly warning becomes a per-minute one. Recovery has to
/// hold for a few samples before the next outage is allowed to speak again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct QueueWarnState {
    last_warned_at_ms: i64,
    good_samples: u32,
}

impl QueueWarnState {
    /// Roughly two minutes of quiet at the sampler's 30-45s cadence.
    const GOOD_SAMPLES_BEFORE_REARM: u32 = 4;
    const AN_HOUR_MS: i64 = 60 * 60 * 1_000;

    fn record_good_sample(&mut self) {
        self.good_samples = self.good_samples.saturating_add(1);
        if self.good_samples >= Self::GOOD_SAMPLES_BEFORE_REARM {
            self.last_warned_at_ms = 0;
        }
    }

    fn should_warn(&mut self, now: i64) -> bool {
        self.good_samples = 0;
        if self.last_warned_at_ms != 0
            && now.saturating_sub(self.last_warned_at_ms) < Self::AN_HOUR_MS
        {
            return false;
        }
        self.last_warned_at_ms = now;
        true
    }
}

/// Lock-free view consumed by the Prometheus handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreMetricsView {
    pub sample: Option<PrometheusStoreSnapshot>,
    pub age_seconds: Option<u64>,
    pub valid: bool,
    pub errors: u64,
    /// Absent until the first sample lands: a verdict with nothing behind it
    /// would read as `idle`, which is a claim, not an absence.
    pub queue_health: Option<AnalysisQueueHealthReport>,
}

/// Last complete Store-backed sample used by the Prometheus handler.
///
/// A single-writer sequence protects atomic publication without putting a
/// mutex on a runtime thread. Scrapes see the complete preceding or complete
/// next sample, never a mixture of both.
#[derive(Clone)]
pub struct StoreMetricsCache {
    inner: Arc<StoreMetricsAtomics>,
    started_at: Instant,
}

impl Default for StoreMetricsCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(StoreMetricsAtomics::default()),
            started_at: Instant::now(),
        }
    }
}

impl StoreMetricsCache {
    #[must_use]
    pub fn snapshot(&self) -> StoreMetricsView {
        self.snapshot_at(self.started_at.elapsed().as_secs())
    }

    fn snapshot_at(&self, elapsed: u64) -> StoreMetricsView {
        loop {
            let before = self.inner.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let published = self.inner.published.load(Ordering::Relaxed);
            let sampled_elapsed = self.inner.sampled_elapsed.load(Ordering::Relaxed);
            let errors = self.inner.errors.load(Ordering::Relaxed);
            let sample = PrometheusStoreSnapshot {
                libraries: self.inner.libraries.load(Ordering::Relaxed),
                users: self.inner.users.load(Ordering::Relaxed),
                offline: OfflinePackageStats {
                    queued: self.inner.queued.load(Ordering::Relaxed),
                    preparing: self.inner.preparing.load(Ordering::Relaxed),
                    ready: self.inner.ready.load(Ordering::Relaxed),
                    failed: self.inner.failed.load(Ordering::Relaxed),
                    queued_bytes: self.inner.queued_bytes.load(Ordering::Relaxed),
                    preparing_bytes: self.inner.preparing_bytes.load(Ordering::Relaxed),
                    ready_bytes: self.inner.ready_bytes.load(Ordering::Relaxed),
                    failed_bytes: self.inner.failed_bytes.load(Ordering::Relaxed),
                    active_leases: self.inner.active_leases.load(Ordering::Relaxed),
                    pinned_bytes: self.inner.pinned_bytes.load(Ordering::Relaxed),
                },
                watched_outbox: (
                    self.inner.outbox_pending.load(Ordering::Relaxed),
                    self.inner.outbox_ok.load(Ordering::Relaxed),
                    self.inner.outbox_failed.load(Ordering::Relaxed),
                ),
                background_jobs: plurx_core::store::background_jobs::BackgroundJobMetrics {
                    counts: std::array::from_fn(|slot| {
                        self.inner.background_job_counts[slot].load(Ordering::Relaxed)
                    }),
                    oldest_age_seconds: std::array::from_fn(|slot| {
                        self.inner.background_job_oldest_age[slot].load(Ordering::Relaxed)
                    }),
                    source_io_reservations: self.inner.background_source_io.load(Ordering::Relaxed),
                    legacy_pending: self.inner.background_legacy_pending.load(Ordering::Relaxed),
                },
                analysis: plurx_core::store::AnalysisStoreMetrics {
                    queue_depth: std::array::from_fn(|slot| {
                        self.inner.analysis_queue_depth[slot].load(Ordering::Relaxed)
                    }),
                    queue_oldest_age_seconds: std::array::from_fn(|slot| {
                        self.inner.analysis_queue_oldest_age_seconds[slot].load(Ordering::Relaxed)
                    }),
                    lifecycle_counts: std::array::from_fn(|slot| {
                        self.inner.analysis_lifecycle_counts[slot].load(Ordering::Relaxed)
                    }),
                    marker_counts: std::array::from_fn(|slot| {
                        self.inner.analysis_marker_counts[slot].load(Ordering::Relaxed)
                    }),
                    health: plurx_core::store::AnalysisQueueHealth {
                        ready_24h: self.inner.analysis_ready_24h.load(Ordering::Relaxed),
                        attempt_limit_24h: self
                            .inner
                            .analysis_attempt_limit_24h
                            .load(Ordering::Relaxed),
                        claimed_24h: self.inner.analysis_claimed_24h.load(Ordering::Relaxed),
                        claimable: self.inner.analysis_claimable.load(Ordering::Relaxed),
                        running_past_lease: self
                            .inner
                            .analysis_running_past_lease
                            .load(Ordering::Relaxed),
                        last_ready_at_ms: self
                            .inner
                            .analysis_last_ready_at_ms
                            .load(Ordering::Relaxed),
                    },
                },
            };
            // Every input the verdict rules on is read before the sequence is
            // re-checked. A baseline or a streak sampled after the check could
            // belong to the next publication and disagree with the counters it
            // is subtracted from.
            let baseline_claims = self.inner.analysis_baseline_claims.load(Ordering::Relaxed);
            let baseline_losses = self
                .inner
                .analysis_baseline_lease_losses
                .load(Ordering::Relaxed);
            let running_past_lease_samples = self
                .inner
                .analysis_running_past_lease_samples
                .load(Ordering::Relaxed);
            let after = self.inner.sequence.load(Ordering::Acquire);
            if before == after {
                let age_seconds = published.then(|| elapsed.saturating_sub(sampled_elapsed));
                let claims_since_start = lifecycle_total(&sample.analysis, "claim")
                    .saturating_sub(baseline_claims)
                    .max(0);
                let lease_losses_since_start = lifecycle_total(&sample.analysis, "lease_loss")
                    .saturating_sub(baseline_losses)
                    .max(0);
                let queue_health = published.then(|| AnalysisQueueHealthReport {
                    health: sample.analysis.health,
                    claims_since_start,
                    lease_losses_since_start,
                    verdict: plurx_core::store::analysis_queue_verdict(
                        sample.analysis.health,
                        claims_since_start,
                        lease_losses_since_start,
                        running_past_lease_samples,
                    ),
                });
                return StoreMetricsView {
                    sample: published.then_some(sample),
                    age_seconds,
                    valid: age_seconds.is_some_and(|age| age <= STORE_METRICS_FRESHNESS_SECS),
                    errors,
                    queue_health,
                };
            }
        }
    }

    fn publish(&self, sample: PrometheusStoreSnapshot) {
        self.publish_at(sample, self.started_at.elapsed().as_secs());
    }

    fn publish_at(&self, sample: PrometheusStoreSnapshot, elapsed: u64) {
        let sequence = loop {
            let current = self.inner.sequence.load(Ordering::Acquire);
            if current & 1 == 0
                && self
                    .inner
                    .sequence
                    .compare_exchange(
                        current,
                        current.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                break current;
            }
            std::hint::spin_loop();
        };
        self.inner
            .libraries
            .store(sample.libraries, Ordering::Relaxed);
        self.inner.users.store(sample.users, Ordering::Relaxed);
        self.inner
            .queued
            .store(sample.offline.queued, Ordering::Relaxed);
        self.inner
            .preparing
            .store(sample.offline.preparing, Ordering::Relaxed);
        self.inner
            .ready
            .store(sample.offline.ready, Ordering::Relaxed);
        self.inner
            .failed
            .store(sample.offline.failed, Ordering::Relaxed);
        self.inner
            .queued_bytes
            .store(sample.offline.queued_bytes, Ordering::Relaxed);
        self.inner
            .preparing_bytes
            .store(sample.offline.preparing_bytes, Ordering::Relaxed);
        self.inner
            .ready_bytes
            .store(sample.offline.ready_bytes, Ordering::Relaxed);
        self.inner
            .failed_bytes
            .store(sample.offline.failed_bytes, Ordering::Relaxed);
        self.inner
            .active_leases
            .store(sample.offline.active_leases, Ordering::Relaxed);
        self.inner
            .pinned_bytes
            .store(sample.offline.pinned_bytes, Ordering::Relaxed);
        self.inner
            .outbox_pending
            .store(sample.watched_outbox.0, Ordering::Relaxed);
        self.inner
            .outbox_ok
            .store(sample.watched_outbox.1, Ordering::Relaxed);
        self.inner
            .outbox_failed
            .store(sample.watched_outbox.2, Ordering::Relaxed);
        for (target, value) in self
            .inner
            .analysis_queue_depth
            .iter()
            .zip(sample.analysis.queue_depth)
        {
            target.store(value, Ordering::Relaxed);
        }
        for (target, value) in self
            .inner
            .analysis_queue_oldest_age_seconds
            .iter()
            .zip(sample.analysis.queue_oldest_age_seconds)
        {
            target.store(value, Ordering::Relaxed);
        }
        for (target, value) in self
            .inner
            .analysis_lifecycle_counts
            .iter()
            .zip(sample.analysis.lifecycle_counts)
        {
            target.store(value, Ordering::Relaxed);
        }
        for (target, value) in self
            .inner
            .analysis_marker_counts
            .iter()
            .zip(sample.analysis.marker_counts)
        {
            target.store(value, Ordering::Relaxed);
        }
        for (target, value) in self
            .inner
            .background_job_counts
            .iter()
            .zip(sample.background_jobs.counts)
        {
            target.store(value, Ordering::Relaxed);
        }
        for (target, value) in self
            .inner
            .background_job_oldest_age
            .iter()
            .zip(sample.background_jobs.oldest_age_seconds)
        {
            target.store(value, Ordering::Relaxed);
        }
        self.inner.background_source_io.store(
            sample.background_jobs.source_io_reservations,
            Ordering::Relaxed,
        );
        self.inner
            .background_legacy_pending
            .store(sample.background_jobs.legacy_pending, Ordering::Relaxed);
        let health = sample.analysis.health;
        self.inner
            .analysis_ready_24h
            .store(health.ready_24h, Ordering::Relaxed);
        self.inner
            .analysis_attempt_limit_24h
            .store(health.attempt_limit_24h, Ordering::Relaxed);
        self.inner
            .analysis_claimed_24h
            .store(health.claimed_24h, Ordering::Relaxed);
        self.inner
            .analysis_claimable
            .store(health.claimable, Ordering::Relaxed);
        self.inner
            .analysis_running_past_lease
            .store(health.running_past_lease, Ordering::Relaxed);
        self.inner
            .analysis_last_ready_at_ms
            .store(health.last_ready_at_ms, Ordering::Relaxed);
        let claims = lifecycle_total(&sample.analysis, "claim");
        let losses = lifecycle_total(&sample.analysis, "lease_loss");
        // Re-anchor when a counter has gone backwards as well as on the first
        // sample. These are monotonic in the store, so a decrease means the
        // term under us changed — a restored backup, a rebuilt node, a
        // leadership change onto a lagging log. Keeping the old baseline would
        // pin the delta at zero from then on, and a permanent zero reads as
        // `idle`: the one verdict that never asks anyone to look.
        let baseline_ran_backwards = claims
            < self.inner.analysis_baseline_claims.load(Ordering::Relaxed)
            || losses
                < self
                    .inner
                    .analysis_baseline_lease_losses
                    .load(Ordering::Relaxed);
        if !self
            .inner
            .analysis_baseline_seen
            .swap(true, Ordering::Relaxed)
            || baseline_ran_backwards
        {
            self.inner
                .analysis_baseline_claims
                .store(claims, Ordering::Relaxed);
            self.inner
                .analysis_baseline_lease_losses
                .store(losses, Ordering::Relaxed);
        }
        if health.running_past_lease > 0 {
            self.inner
                .analysis_running_past_lease_samples
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .analysis_running_past_lease_samples
                .store(0, Ordering::Relaxed);
        }
        self.inner.sampled_elapsed.store(elapsed, Ordering::Relaxed);
        self.inner.published.store(true, Ordering::Relaxed);
        self.inner
            .sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
    }

    fn record_error(&self) {
        self.inner.errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Everything a request handler needs. Cheap to clone (all shared via `Arc`).
#[derive(Clone)]
pub struct AppState {
    /// Opened once under SelectedStore's actual lock, before scratch/probes.
    /// None means unsupported/unconfigured custody, NEVER zero resource usage.
    pub(crate) resource_runtime:
        Option<Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>>,
    /// Addressed-node HTTPS intent and the actually loaded public identity.
    pub(crate) https: Option<Arc<crate::https::Control>>,
    pub store: Arc<dyn Store>,
    pub(crate) sharing: Arc<crate::sharing::SharingManager>,
    pub(crate) link_receipts: Arc<crate::http::hls::link_receipts::LinkReceipts>,
    /// Fixed-lifetime, digest-only admin proofs for cluster recovery reads.
    /// Ordinary authentication populates it; cache-only routes never reach
    /// Store on a miss.
    pub(crate) cache_only_admin_proofs: crate::http::CacheOnlyAdminProofCache,
    /// Process-local, bounded failed-login history. It never enters the Store,
    /// so a hostile request cannot turn authentication pressure into Raft
    /// writes or a replicated account lockout.
    pub(crate) login_throttle: crate::http::LoginThrottle,
    /// Argon2 admission (workers and queue). Per state, like the throttle:
    /// one per process in production, one per router in tests (see
    /// `http::PasswordCapacity`).
    pub(crate) password_capacity: crate::http::PasswordCapacity,
    /// C-07's façade census. Per state rather than process-global so each
    /// router counts only its own requests (see `http::PlexCensus`).
    pub(crate) plex_census: Arc<crate::http::PlexCensus>,
    /// What became of this process's Jellyfin standard-port listener.
    /// Advisory; Settings -> Developer reports it.
    pub(crate) jellyfin_standard_port: Arc<crate::http::JellyfinStandardPortStatus>,
    /// Node-local network boundary for security-sensitive forwarding headers.
    pub(crate) trusted_proxies: Arc<Vec<ipnet::IpNet>>,
    /// Named Authority/BoundedReplica boundary for eligible catalogue reads.
    pub catalogue: CatalogueReader,
    /// Read-only projection of the selected backend's watch-state convergence.
    pub replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    /// Monotonic, Store-free authority for mutable media and readiness.
    pub(crate) serving: crate::serving_fence::ServingFence,
    /// Join/add/remove lifecycle and privacy-safe per-node health.
    pub membership: plurx_core::cluster::membership::MembershipManager,
    /// Shared complete observation owner. Pending startup hands this exact
    /// handle to normal HTTP; a second loop cannot claim its receiver.
    pub(crate) clock_observer: crate::clock_offset::ClockObserver,
    /// Bounded authenticated client for node-local activity snapshots.
    #[allow(dead_code)] // aggregation child #326 is the first consumer
    pub peer_activity: crate::http::internal_activity::PeerActivityClient,
    /// Five-second cache for authenticated, bounded cluster-status fan-out.
    /// This is process-local diagnostics state and never writes to Store.
    pub(crate) peer_status_cache: crate::http::cluster_operations::PeerStatusCache,
    /// Five-second, background-refreshed committed-membership projection for
    /// operations requests. Request handlers cannot reach Store through it.
    pub(crate) membership_status_cache: crate::http::cluster_operations::MembershipStatusCache,
    /// Fresh, authenticated media-capability snapshots and diagnostics-only
    /// placement offers. P4 observes candidates; it never starts a session.
    pub(crate) media_pool: Arc<crate::media_pool::MediaPool>,
    /// Authenticated remote start/abort and streaming HLS relay transport.
    pub(crate) media_sessions: Arc<crate::media_sessions::MediaSessionCoordinator>,
    /// What tells an idle reader of replicated settings that a save may have
    /// landed on another node. Each reader takes its own clone.
    pub(crate) settings_changes: crate::settings_changes::SettingsChangeSource,
    /// Always-compiled HDHomeRun configuration, readiness, and lineup owner.
    pub(crate) live_tv: Arc<crate::live_tv::LiveTvManager>,
    /// One HTTP client for every Live TV ingress-to-owner exchange, plus a
    /// five-second memory of the owner's address. Signing, expected-node
    /// binding and response verification stay per request inside the
    /// transport; nothing about a credential is held here.
    pub(crate) live_tv_peers: Arc<crate::http::live_tv::LiveTvPeers>,
    pub(crate) subtitle_models: Arc<crate::subtitle_models::Runtime>,
    pub server_name: String,
    /// Items whose review-site scores this node is fetching right now.
    pub(crate) rating_fetches: crate::http::annotations::RatingFetches,
    /// Where review-site score providers are reached (overridden in tests).
    pub(crate) provider_endpoints: crate::http::annotations::ProviderEndpoints,
    /// This node's short hostname, resolved once from membership at
    /// construction (the name the roster shows for it); `None` when the
    /// machine has no usable name.
    pub node_hostname: Option<String>,
    /// Stable identity of the node that owns local transcode/offline bytes.
    pub node_id: String,
    /// Whether LAN discovery must distinguish this node from cluster peers.
    pub cluster_advertisement: bool,
    /// Reported by the Developer readiness route; not consulted by any
    /// recovery path, which reads them from `Config` when it builds the node.
    pub snapshot_recovery_budgets: crate::state::SnapshotRecoveryBudgets,
    pub artwork_dir: PathBuf,
    /// Shared peer-artwork HTTP client, per-filename singleflight, and global
    /// response-buffer bound for request and background reconciliation paths.
    pub(crate) artwork_fetch: Arc<crate::http::images::ArtworkCoordinator>,
    /// Finished content-addressed transcodes. Offline routes never join a
    /// request-controlled path directly to this root.
    pub cache_dir: PathBuf,
    /// Explicit ffmpeg/cache-bookkeeping root. Kept separately because cache
    /// relocation may make it non-sibling to the finished-transcode root.
    pub(crate) runtime_cache_dir: PathBuf,
    /// Optional shared-cache mount, admitted only while its all-voter canary
    /// proof remains current.
    pub(crate) shared_cache: Arc<crate::shared_cache::SharedCacheCoordinator>,
    pub(crate) playback_cache: Arc<crate::playback_cache::PlaybackCacheBudget>,
    /// Where extracted subtitles are kept, keyed by file identity and source
    /// fingerprint — see `http::stream::subtitles_vtt`.
    pub subs_dir: PathBuf,
    pub jobs: Arc<JobManager>,
    pub(crate) backup: Arc<crate::backup::BackupManager>,
    pub transcode: Arc<TranscodeManager>,
    pub offline: Arc<OfflineManager>,
    /// Short-lived, revision-bound EPUB resource capabilities. Publication
    /// markup receives one of these, never the user's reusable API token.
    pub publications: Arc<crate::http::publication::PublicationSessions>,
    pub remote: Arc<crate::http::remote::Hub>,
    pub invitations: Arc<crate::http::remote::invitations::Hub>,
    pub trakt: Arc<TraktManager>,
    pub system: Arc<SystemInfo>,
    pub logs: Arc<LogBuffer>,
    /// Replication/membership detail kept out of the general diagnostics ring.
    pub cluster_logs: Arc<LogBuffer>,
    /// The coming-soon rail's cached answer from Curator (plan §11.2).
    pub coming_soon: Arc<crate::http::ComingSoonCache>,
    /// Pushes watch state to Curator when enabled (plan §11.1).
    pub watched: Arc<crate::watched::WatchedNotifier>,
    /// Leading/trailing progress write coalescer. Every player may report as
    /// often as it likes; durable watch commits stay bounded for Raft.
    pub progress: Arc<crate::progress::ProgressCoalescer>,
    /// What the storage under each library actually reads at
    /// (`crate::storeprobe`). Behind a lock and not in `SystemInfo` because,
    /// unlike the encoder list, it is not a fact about the machine that is
    /// true for the process's life: a mount can be re-exported, a link can be
    /// re-cabled, and the number is re-measurable on demand.
    ///
    /// Filled by a background task shortly after boot rather than during it —
    /// the numbers are worth having, but not at the price of a server that
    /// will not answer until a sleeping array has spun up.
    pub storage: Arc<tokio::sync::RwLock<crate::storeprobe::StorageReport>>,
    /// The operator-run tone-map census (`pipeprobe::census`): at most one
    /// run, its live status and the node's stored result. Process-local; it
    /// starts only from Settings → System and never on its own.
    pub(crate) tone_map_census: Arc<crate::pipeprobe::ToneMapCensus>,
    /// Keeps the click path off the NAS, and announces a start once playback
    /// is real rather than once a decision has been made.
    pub availability: Arc<crate::playstart::AvailabilityCache>,
    /// Advisory, bounded file observations for item detail only. Playback
    /// never consults this cache when deciding whether a source opens.
    pub detail_availability: Arc<crate::availability::AvailabilityCache>,
    /// The library index coverage measurement: the one running and the last
    /// result (`/api/v1/analysis/coverage`). Process-local.
    pub(crate) index_coverage: Arc<crate::http::index_coverage::CoverageRuns>,
    pub starts: Arc<crate::playstart::StartNotifier>,
    /// Finite-playback start attempts and how each ended (C-08 M5 row 4).
    pub start_attempts: Arc<crate::playstart::StartAttempts>,
    /// Live telemetry for progressive `/stream.mp4` remuxes, which are not
    /// transcode sessions and so have nowhere else to report from.
    pub streams: Arc<crate::progressive::Streams>,
    /// Deliveries that keep no record of themselves — direct play. The other
    /// three routes are listed from the machinery that already owns their
    /// lifetimes, so this holds only what would otherwise be invisible; see
    /// [`crate::delivery`].
    pub direct_plays: Arc<crate::delivery::DirectPlays>,
    /// Last progress beat per viewer and item, for
    /// `plurx_watched_seconds_total` (C-08 M5). One per process, like the
    /// direct-play registry beside it.
    pub watch_ledger: Arc<crate::telemetry::WatchLedger>,
    /// Store-backed gauges sampled away from the Prometheus request path.
    pub store_metrics: StoreMetricsCache,
    /// Graceful drain. The cluster leave endpoint cancels this only after its
    /// own voter removal has committed; `main` cancels it when a process
    /// signal starts the drain, so owners see one token for either cause.
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Test-only rendezvous inside the real cache-admin revocation fence.
    /// Production has no hook or alternate path.
    #[cfg(test)]
    pub(crate) cache_revocation_test_barrier: Option<Arc<tokio::sync::Barrier>>,
    /// The HLS route group's test points (TRANSCODE-DECOMPOSITION-PLAN §3.9,
    /// M8, Decision D-M8-J), in every build and shared by every clone.
    /// Production never fills the slot, so every point reads
    /// [`crate::http::hls::NoopHlsRouteHooks`].
    pub(crate) hls_route_hooks:
        Arc<crate::seam_hooks::HookSlot<dyn crate::http::hls::HlsRouteHooks>>,
    pub started_at: Instant,
}

/// What one page of the first-frame luminance backfill did with its rows.
#[derive(Debug, Default)]
struct FrameBackfillTally {
    observed: usize,
    absent: usize,
    refused: usize,
    fenced: usize,
    changed: usize,
    gone: usize,
    ineligible: usize,
}

impl FrameBackfillTally {
    /// Reads ffprobe completed, whatever the frame carried.
    fn read(&self) -> usize {
        self.observed + self.absent + self.fenced
    }
}

/// The Activity purpose of the unsignalled-transfer re-probe's frame reads.
const UNSIGNALLED_TRANSFER_PURPOSE: &str =
    "catalogue HDR re-probe of titles whose stream left the transfer unsignalled";

/// Frame reads one tick of that walk may start: each opens and decodes media.
const UNSIGNALLED_TRANSFER_READS_PER_TICK: usize = 16;

/// Where a first-frame walk's cursor ends after a page whose every attempted
/// read was refused.
///
/// One refusal is a statement about one file. Several with no read beside
/// them could be a statement about the reader, so ffprobe is asked its own
/// self-check. If it passes, the refusals are verdicts on those files — a
/// corrupt title, a truncated download — and the cursor moves past them, so
/// a library whose last rows are unreadable still completes its walk instead
/// of re-reading them every tick. If it fails, the problem is this node's,
/// and the cursor holds at the row before the first refusal, keeping every
/// advance the page earned on its own facts before it.
async fn settle_refused_page<G, GFut>(
    walk: &'static str,
    tally: &FrameBackfillTally,
    walked: i64,
    before_first_refusal: i64,
    self_check: G,
) -> i64
where
    G: FnOnce() -> GFut,
    GFut: std::future::Future<Output = bool>,
{
    if tally.refused <= 1 || tally.read() > 0 {
        return walked;
    }
    if self_check().await {
        tracing::warn!(
            walk,
            refused = tally.refused,
            "every frame read in this page was refused and ffprobe passes its self-check; \
             those files keep their stored facts and the walk moves past them"
        );
        walked
    } else {
        tracing::warn!(
            walk,
            refused = tally.refused,
            "every frame read in this page was refused and ffprobe fails its own self-check; \
             holding the cursor until it passes"
        );
        before_first_refusal
    }
}

fn stored_luminance(probe_json: &str) -> plurx_core::domain::ProbeResult {
    serde_json::from_str::<serde_json::Value>(probe_json)
        .ok()
        .map(|document| plurx_core::scan::probe::parse_probe_json(&document))
        .unwrap_or_default()
}

impl AppState {
    /// Whether PGS subtitle tracks are served through the `pgs-v1` overlay.
    ///
    /// Read at the request boundary rather than latched at boot, so an
    /// operator who turns it on in Settings sees the next request honour it.
    ///
    /// The store error is propagated rather than answered `false`, and that is
    /// deliberate. Off is not the safe default here: it is the expensive one.
    /// A selected PGS track with the overlay off becomes a burn-in, which
    /// re-encodes the video and drops it to SDR, so one failed read during a
    /// leader election would silently turn a direct-play HDR remux into a full
    /// SDR transcode. The handlers that ask this already propagate store
    /// errors a few lines later, so swallowing it here buys no availability —
    /// it only substitutes a wrong answer for an honest failure.
    pub(crate) async fn pgs_overlay_enabled(&self) -> Result<bool, plurx_core::error::StoreError> {
        Ok(plurx_core::store::stored_switch(
            self.store.get_setting(keys::PGS_OVERLAY).await?.as_deref(),
            false,
        ))
    }

    /// Resolve the operator's bounded forward subtitle span at the request
    /// boundary. A malformed hand-edited row falls back to the 200-second
    /// default; the settings API itself only persists values inside 30–900.
    pub(crate) async fn subtitle_window_seconds(&self) -> i64 {
        let stored = self
            .store
            .get_setting(keys::SUBTITLE_WINDOW_SECS)
            .await
            .ok()
            .flatten();
        plurx_core::store::bounded_subtitle_window_seconds(stored.as_deref())
    }

    /// The subtitle-source store as a consumer on this node sees it. Nothing
    /// is read here: `subtitles.stored_sources` is read by a lookup, and only
    /// once one is going to consult the store, so a manifest poll for a warm
    /// overlay generation pays nothing for it.
    pub(crate) fn subtitle_source_access(&self) -> crate::subtitle_source::StoreAccess {
        crate::subtitle_source::StoreAccess::from_setting(
            Arc::clone(&self.store),
            &self.runtime_cache_dir,
        )
        .on_node(Some(&self.node_id))
        .with_membership(self.membership.clone())
        .with_jobs(Arc::clone(&self.jobs))
    }

    pub(crate) async fn subtitle_backfill_status(
        &self,
    ) -> Result<SubtitleBackfillStatus, StoreError> {
        let diagnostics = self.store.subtitle_backfill_diagnostics(clock_ms()).await?;
        Ok(SubtitleBackfillStatus {
            lease_holder: diagnostics.lease_holder,
            enqueued_process: self.jobs.subtitle_backfill_enqueued_process(),
            remaining_files: diagnostics.remaining_files,
            remaining_bytes: diagnostics.remaining_bytes,
        })
    }

    /// `node_id` is this server's stable id — the `node_id` a cache location
    /// is recorded against, so a cluster can tell whose copy is whose.
    ///
    /// The state holds the HLS route test hooks with priming turned off, so
    /// its admitted preparation candidates stage without priming (Decision
    /// D-M8-K) until a test calls
    /// [`crate::http::hls::prime_prepared_successors`];
    /// [`AppState::new_unhooked`] leaves the production no-op in place.
    #[cfg(test)]
    pub fn new(
        server_name: String,
        store: Arc<dyn Store>,
        dirs: Dirs,
        node_id: String,
        encoder_caps: EncoderCaps,
        system: SystemInfo,
        logs: Arc<LogBuffer>,
    ) -> Self {
        let state = Self::new_unhooked(
            server_name,
            store,
            dirs,
            node_id,
            encoder_caps,
            system,
            logs,
        );
        crate::http::hls::prime_prepared_successors(&state, false);
        state
    }

    /// [`AppState::new`] without the HLS route test hooks: every route point
    /// reads the no-op production installs.
    #[cfg(test)]
    pub fn new_unhooked(
        server_name: String,
        store: Arc<dyn Store>,
        dirs: Dirs,
        node_id: String,
        encoder_caps: EncoderCaps,
        system: SystemInfo,
        logs: Arc<LogBuffer>,
    ) -> Self {
        let catalogue = CatalogueReader::authority(Arc::clone(&store));
        Self::new_configured(
            AppConfig {
                sharing_network: Default::default(),
                server_name,
                node_id,
                cluster_advertisement: false,
                trusted_proxies: Vec::new(),
                scan_prune_percent: plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
                // Process-lifetime key. Production resolves one from disk in
                // `open_store`; this constructor is for callers that have no
                // data directory to resolve it from, so nothing sealed under
                // it is expected to outlive the process.
                credential_key: Arc::new(CredentialKey::generate()),
                replication: plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
                membership: plurx_core::cluster::membership::MembershipManager::unavailable(),
                cluster_id: String::new(),
                shared_cache_dir: PathBuf::new(),
                shared_cache_id: String::new(),
                catalogue,
                snapshot_recovery_budgets: SnapshotRecoveryBudgets::default(),
                data_dir: PathBuf::new(),
                credential_key_path: PathBuf::new(),
                backup_client: None,
            },
            store,
            dirs,
            encoder_caps,
            system,
            LogBuffers {
                general: logs,
                cluster: Arc::new(LogBuffer::default()),
            },
            None,
        )
    }

    pub fn new_configured(
        config: AppConfig,
        store: Arc<dyn Store>,
        dirs: Dirs,
        encoder_caps: EncoderCaps,
        system: SystemInfo,
        logs: LogBuffers,
        resource_runtime: Option<Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>>,
    ) -> Self {
        let AppConfig {
            sharing_network,
            server_name,
            node_id,
            cluster_advertisement,
            trusted_proxies,
            scan_prune_percent,
            credential_key,
            replication,
            membership,
            cluster_id,
            shared_cache_dir,
            shared_cache_id,
            catalogue,
            snapshot_recovery_budgets,
            data_dir,
            credential_key_path,
            backup_client,
        } = config;
        let serving = crate::serving_fence::ServingFence::new(replication.metrics_handle());
        let Dirs {
            artwork: artwork_dir,
            transcode: transcode_dir,
            cache: cache_dir,
            subs: subs_dir,
            runtime_cache,
            renditions,
        } = dirs;
        // Every regenerable playback namespace shares physical capacity and
        // outstanding write grants. Durable state and artwork are excluded.
        let playback_cache = crate::playback_cache::PlaybackCacheBudget::new(
            Arc::clone(&store),
            vec![
                cache_dir.clone(),
                subs_dir.clone(),
                runtime_cache.clone(),
                renditions.clone(),
                transcode_dir.clone(),
            ],
        );
        // The finite-media orphan sweep reserves this child. Only the Live TV
        // registry can distinguish active tuner scratch from restart debris.
        let live_tv_scratch = transcode_dir.join(crate::transcode::LIVE_TV_WORK_DIR_NAME);
        let system = Arc::new(system);
        let jobs = Arc::new(
            JobManager::new_with_scan_prune_percent(
                Arc::clone(&store),
                artwork_dir.clone(),
                scan_prune_percent,
                node_id.clone(),
                Arc::new(membership.clone()),
            )
            .with_dv_disk_capabilities(system.dv_disk.clone())
            .with_membership(membership.clone()),
        );
        let sharing = Arc::new(crate::sharing::SharingManager::new(
            Arc::clone(&credential_key),
            data_dir.join("sharing-tls"),
            sharing_network,
        ));
        // A replicated daemon applies the Raft log in this process; that watch
        // is how an idle settings reader (the Live TV observer, the takeover
        // gate, the clock-enforcement follower) learns of a save elsewhere.
        let settings_changes = match backup_client
            .as_ref()
            .and_then(|client| client.local_db_raft_metrics().ok())
        {
            Some(applied) => crate::settings_changes::SettingsChangeSource::Applied(applied),
            None if membership.is_replicated() => {
                crate::settings_changes::SettingsChangeSource::Polled
            }
            None => crate::settings_changes::SettingsChangeSource::LocalOnly,
        };
        let subtitle_models = Arc::new(crate::subtitle_models::Runtime::new(&data_dir));
        let backup = crate::backup::BackupManager::new(
            Arc::clone(&store),
            Arc::clone(&jobs),
            backup_client,
            data_dir,
            credential_key_path,
            node_id.clone(),
            credential_key.id().to_owned(),
            Duration::from_secs(snapshot_recovery_budgets.transfer_secs)
                .saturating_add(Duration::from_secs(45)),
        );
        let coming_soon = crate::http::ComingSoonCache::new();
        let watched = crate::watched::WatchedNotifier::new(Arc::clone(&store));
        let progress = crate::progress::ProgressCoalescer::new(Arc::clone(&store));
        let shared_cache = crate::shared_cache::SharedCacheCoordinator::new(
            shared_cache_dir,
            shared_cache_id,
            &cluster_id,
            node_id.clone(),
            membership.clone(),
            Arc::clone(&store),
        );
        let transcode = Arc::new(
            TranscodeManager::new(
                Arc::clone(&store),
                transcode_dir,
                encoder_caps,
                system.tone_map.selected(),
            )
            .with_encoder_override(crate::transcoder_optimization::effective_override(&system))
            .with_decoders(system.decoders.clone())
            // Cloned from the policy this process installed, which happens in
            // `probe_system` before this state is built. Handing it over makes
            // the dependency visible in one place instead of being a global
            // this manager reaches for at an unstated moment.
            .with_diagnostic_policy(std::sync::Arc::new(
                crate::decoder_health::diagnostic_policy().clone(),
            ))
            .with_measured_decoders(system.measured_decoders.clone())
            .with_decode_probe(system.decode_probe_identity.clone())
            .with_dv_strippable(system.dovi_rpu)
            .with_dv_convertible(system.dolby_vision_convert)
            .with_dovi_reshape(system.dovi_reshape)
            .with_dovi_passthrough(system.dovi_passthrough)
            .with_dovi_passthrough_qsv(system.dovi_passthrough_qsv)
            .with_hdr10_passthrough(system.hdr10_passthrough)
            .with_hdr10_passthrough_qsv(system.hdr10_passthrough_qsv)
            .with_hdr10_passthrough_vaapi(system.hdr10_passthrough_vaapi)
            .with_metal_tone_map_parity(
                system
                    .tone_map
                    .parity(plurx_core::transcode::Pipeline::VtToneMapMetal),
            )
            .with_cache_layout(
                cache_dir.clone(),
                runtime_cache.clone(),
                subs_dir.clone(),
                renditions,
                system.ffmpeg_version.clone().unwrap_or_default(),
                node_id.clone(),
                Some(membership.clone()),
                resource_runtime.clone(),
            )
            .with_subtitle_jobs(Arc::clone(&jobs))
            .with_serving_authority(serving.authority())
            .with_shared_cache(Arc::clone(&shared_cache)),
        );
        transcode.register_playback_cache(&playback_cache);
        crate::subtitle_translation::runtime::register_playback_cache(
            &playback_cache,
            &runtime_cache,
        );
        crate::fragment_index_cluster::register_playback_cache(
            &playback_cache,
            &runtime_cache,
            Arc::clone(&store),
            node_id.clone(),
        );
        crate::http::chapter_thumbs::register_playback_cache(&playback_cache, &runtime_cache);
        crate::subtitle_source::register_playback_cache(
            &playback_cache,
            &runtime_cache,
            &subs_dir,
            Arc::clone(&store),
            &node_id,
        );
        // PLURX_TRAKT_BASE overrides the API base for tests/mocks.
        let trakt_base = std::env::var("PLURX_TRAKT_BASE")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| plurx_core::trakt::DEFAULT_BASE.to_owned());
        let trakt = Arc::new(TraktManager::new(
            Arc::clone(&store),
            credential_key,
            trakt_base,
        ));
        let offline = OfflineManager::new(
            Arc::clone(&store),
            Arc::clone(&transcode),
            node_id.clone(),
            serving.clone(),
        );
        let media_pool = crate::media_pool::MediaPool::new(membership.clone());
        let media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
            membership.clone(),
            Arc::clone(&store),
        );
        let cache_only_admin_proofs =
            crate::http::CacheOnlyAdminProofCache::new(membership.is_replicated());
        let live_tv_peers = crate::http::live_tv::LiveTvPeers::new(membership.clone());
        let live_tv = crate::live_tv::LiveTvManager::new(
            Arc::clone(&store),
            Arc::clone(&system),
            Arc::clone(&transcode),
            serving.authority(),
            node_id.clone(),
            live_tv_scratch,
            runtime_cache.join("live-tv"),
        );
        live_tv.set_settings_changes(settings_changes.clone());
        AppState {
            resource_runtime,
            https: None,
            store,
            sharing,
            link_receipts: Default::default(),
            cache_only_admin_proofs,
            login_throttle: Default::default(),
            password_capacity: Default::default(),
            plex_census: Default::default(),
            jellyfin_standard_port: Default::default(),
            trusted_proxies: Arc::new(trusted_proxies),
            catalogue,
            replication,
            peer_activity: crate::http::internal_activity::PeerActivityClient::new(
                membership.clone(),
            ),
            peer_status_cache: Default::default(),
            membership_status_cache: Default::default(),
            serving,
            clock_observer: crate::clock_offset::ClockObserver::new(membership.clone()),
            node_hostname: membership.local_hostname(),
            membership,
            media_pool,
            media_sessions,
            settings_changes,
            live_tv,
            live_tv_peers,
            subtitle_models,
            server_name,
            rating_fetches: Default::default(),
            provider_endpoints: Default::default(),
            node_id,
            cluster_advertisement,
            snapshot_recovery_budgets,
            artwork_dir,
            artwork_fetch: crate::http::images::ArtworkCoordinator::new(),
            cache_dir,
            runtime_cache_dir: runtime_cache,
            shared_cache,
            playback_cache,
            subs_dir,
            jobs,
            backup,
            transcode,
            offline,
            publications: crate::http::publication::PublicationSessions::new(),
            remote: Arc::new(crate::http::remote::Hub::default()),
            invitations: Arc::new(crate::http::remote::invitations::Hub::default()),
            trakt,
            system,
            logs: logs.general,
            cluster_logs: logs.cluster,
            coming_soon,
            watched,
            progress,
            storage: Arc::new(tokio::sync::RwLock::new(Default::default())),
            tone_map_census: Default::default(),
            availability: Arc::new(crate::playstart::AvailabilityCache::new()),
            detail_availability: crate::availability::AvailabilityCache::new(),
            index_coverage: Arc::default(),
            starts: Arc::new(crate::playstart::StartNotifier::new()),
            start_attempts: Arc::new(crate::playstart::StartAttempts::new()),
            streams: crate::progressive::Streams::new(),
            direct_plays: crate::delivery::DirectPlays::new(),
            watch_ledger: Arc::new(crate::telemetry::WatchLedger::default()),
            store_metrics: StoreMetricsCache::default(),
            shutdown: tokio_util::sync::CancellationToken::new(),
            #[cfg(test)]
            cache_revocation_test_barrier: None,
            hls_route_hooks: crate::http::hls::unfilled_hls_route_hooks(),
            started_at: Instant::now(),
        }
    }

    /// Refresh all Store-backed Prometheus gauges as one complete sample.
    /// A failure preserves the previous sample and increments its bounded
    /// source counter.
    pub async fn refresh_store_metrics(&self) -> Result<(), StoreError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64;
        match self
            .store
            .prometheus_store_snapshot(&self.node_id, now)
            .await
        {
            Ok(snapshot) => {
                self.store_metrics.publish(snapshot);
                Ok(())
            }
            Err(error) => {
                self.store_metrics.record_error();
                Err(error)
            }
        }
    }

    /// Say out loud, once an hour per node, that the queue is not working.
    ///
    /// The numbers existed before this: `plurx_analysis_queue_depth` and the
    /// lifecycle counters were the evidence that eventually named a
    /// seventy-two hour outage. What did not exist was anything that turned
    /// them into a sentence, so a queue failing every job it claimed looked
    /// like a queue with a lot of history.
    fn warn_if_the_queue_is_not_producing(&self, warn_state: &mut QueueWarnState) {
        let Some(report) = self.store_metrics.snapshot().queue_health else {
            return;
        };
        if !matches!(
            report.verdict,
            plurx_core::store::AnalysisQueueVerdict::Dead
                | plurx_core::store::AnalysisQueueVerdict::Degraded
        ) {
            warn_state.record_good_sample();
            return;
        }
        let now = clock_ms();
        if !warn_state.should_warn(now) {
            return;
        }
        tracing::warn!(
            verdict = report.verdict.as_str(),
            claims_since_start = report.claims_since_start,
            lease_losses_since_start = report.lease_losses_since_start,
            ready_24h = report.health.ready_24h,
            attempt_limit_24h = report.health.attempt_limit_24h,
            claimed_24h = report.health.claimed_24h,
            claimable = report.health.claimable,
            running_past_lease = report.health.running_past_lease,
            "fragment-index queue is {}: {} claimed and {} ready in 24h, {} waiting",
            report.verdict.as_str(),
            report.health.claimed_24h,
            report.health.ready_24h,
            report.health.claimable
        );
    }

    /// Keep the scrape snapshot fresh without coupling availability to a
    /// Store or leader round trip. The first sample starts immediately.
    pub async fn store_metrics_loop(self) {
        let stagger = self.node_id.bytes().fold(0_u64, |hash, byte| {
            hash.wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(byte))
        }) % 15;
        let base_interval = Duration::from_secs(30 + stagger);
        let mut consecutive_errors = 0_u32;
        let mut warn_state = QueueWarnState::default();
        loop {
            if let Err(error) = self.refresh_store_metrics().await {
                consecutive_errors = consecutive_errors.saturating_add(1);
                tracing::debug!(%error, "refreshing Store-backed metrics snapshot failed");
            } else {
                consecutive_errors = 0;
                self.warn_if_the_queue_is_not_producing(&mut warn_state);
            }
            let backoff = 1_u32 << consecutive_errors.min(3);
            let interval = base_interval.saturating_mul(backoff);
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }
}

/// What this node has budgeted for one Raft snapshot recovery.
///
/// These live in `[cluster]` config and are consumed only when the Hiqlite
/// node is built, so nothing in the admin surface could report them. The
/// Developer readiness route needs to say what they actually are — an
/// operator asked whether the budgets fit their deployment cannot answer that
/// from a document listing the defaults.
///
/// `cluster.trusted_network` was carried here in a first draft and removed:
/// nothing in the daemon reads it, so printing it beside a row about keeping
/// the transport off the public Internet would have implied a boundary the
/// process is not holding.
#[derive(Clone, Debug, Default)]
pub struct SnapshotRecoveryBudgets {
    pub chunk_secs: u64,
    pub transfer_secs: u64,
    pub install_secs: u64,
}

pub struct AppConfig {
    pub sharing_network: plurx_core::config::SharingNetworkConfig,
    pub server_name: String,
    pub node_id: String,
    pub cluster_advertisement: bool,
    pub trusted_proxies: Vec<ipnet::IpNet>,
    pub scan_prune_percent: u8,
    /// Node-local key for durable credentials plurx replays rather than
    /// verifies. Resolved by `open_store` so a boot that cannot open the
    /// existing Trakt rows fails before any subsystem starts.
    pub credential_key: Arc<CredentialKey>,
    /// Actual backend selected before HTTP starts; tests default to SQLite.
    pub replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    pub membership: plurx_core::cluster::membership::MembershipManager,
    pub cluster_id: String,
    pub shared_cache_dir: PathBuf,
    pub shared_cache_id: String,
    pub catalogue: CatalogueReader,
    pub snapshot_recovery_budgets: SnapshotRecoveryBudgets,
    pub data_dir: PathBuf,
    pub credential_key_path: PathBuf,
    pub backup_client: Option<hiqlite::Client>,
}

/// Status of the most recent (or in-flight) scan for one library.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanStatus {
    pub running: bool,
    /// Durable waiting state "queued", or active "scanning" / "enriching".
    pub phase: Option<String>,
    /// Live counters while running (sampled from the scan's atomics).
    pub progress: Option<ProgressSnapshot>,
    pub last_scan: Option<ScanReport>,
    pub last_enrich: Option<EnrichReport>,
    /// Home libraries only: the local-artwork pass (frame grabs, adopted
    /// sidecar images, inherited folder posters) that stands in for a provider.
    pub last_local_art: Option<LocalArtReport>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

/// What one enrichment pass produced. Exactly the two status fields a scan
/// records, so the single enrichment path can hand them back to a caller that
/// keeps a `ScanStatus` (the full scan) and to one that does not (a targeted
/// scan, the retry sweep, a per-item refresh) without either learning which
/// kind of provider ran.
#[derive(Clone, Debug, Default, Serialize)]
pub struct EnrichOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrich: Option<EnrichReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_art: Option<LocalArtReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub books: Option<BookEnrichReport>,
}

impl EnrichOutcome {
    /// Put a provider outage into the scan's own problems.
    ///
    /// The enrichment report lives only in this node's memory, and a finished
    /// library's status is read back from the durable work row, which keeps
    /// the `ScanReport` alone — so a pass with TMDB unreachable used to finish
    /// as a clean scan with zero problems (LAB2-EVIDENCE-2026-10-11 §5). The
    /// one line per unreachable provider host is the part an operator must
    /// see; per-title detail stays in the enrichment report and the log.
    fn report_provider_outage(&self, scan: &mut ScanReport) {
        for line in self.enrich.iter().flat_map(|r| &r.provider_unavailable) {
            scan.errors += 1;
            scan.add_problem(line.clone());
        }
    }
}

/// Point-in-time view of a running scan's counters.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ProgressSnapshot {
    pub found: usize,
    pub processed: usize,
    pub changed: usize,
}

impl ProgressSnapshot {
    fn sample(p: &ScanProgress) -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        ProgressSnapshot {
            found: p.found.load(Relaxed),
            processed: p.processed.load(Relaxed),
            changed: p.changed.load(Relaxed),
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Runs library scans (and metadata enrichment) off the request path, one at a
/// time per library. In Phase 4 this becomes a leader-scheduled cluster
/// singleton (ARCHITECTURE §2.2); the API surface here stays the same.
/// What asked for a scan. It is a label on a counter, but the distinction is
/// the point of having the counter: "plurx scanned 400 times today" says
/// nothing, while "398 of them were scheduled and 2 were targeted" says the
/// fast path is not being used and something upstream is not calling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanTrigger {
    /// A person pressed a button, or created or edited a library.
    Manual,
    /// The reconcile interval came round (P5).
    Scheduled,
    /// Scan-at-startup, covering what landed while the server was off.
    Startup,
    /// Another application said "index exactly this path".
    Targeted,
}

impl ScanTrigger {
    pub fn label(self) -> &'static str {
        match self {
            ScanTrigger::Manual => "manual",
            ScanTrigger::Scheduled => "scheduled",
            ScanTrigger::Startup => "startup",
            ScanTrigger::Targeted => "targeted",
        }
    }
}

/// Counters for the integration, exposed on `/metrics`.
///
/// Deliberately counters and not gauges: the question these answer is "is
/// the other application actually talking to us, and has it stopped?", and
/// only a monotonic count can distinguish "never" from "not since the last
/// restart" once it is graphed.
#[derive(Default, Debug)]
pub struct IntegrationMetrics {
    manual: AtomicU64,
    scheduled: AtomicU64,
    startup: AtomicU64,
    targeted: AtomicU64,
    notify_received: AtomicU64,
}

impl IntegrationMetrics {
    fn count_scan(&self, trigger: ScanTrigger) {
        self.count_for(trigger).fetch_add(1, Ordering::Relaxed);
    }

    /// Every inbound scan request that reached the handler, counted before
    /// the path is resolved — a request rejected for a path-mapping mistake
    /// still proves the other application reached plurx, which is the first
    /// thing anyone debugging this needs to know.
    pub fn count_notification(&self) {
        self.notify_received.fetch_add(1, Ordering::Relaxed);
    }

    /// `(trigger label, count)` pairs, plus notifications received.
    ///
    /// Every trigger is listed even at zero. A counter that only appears
    /// once it fires cannot express "this has never happened", which is the
    /// single most useful thing `plurx_scan_total{trigger="targeted"}` has
    /// to say.
    pub fn snapshot(&self) -> (Vec<(&'static str, u64)>, u64) {
        let counts = [
            ScanTrigger::Manual,
            ScanTrigger::Scheduled,
            ScanTrigger::Startup,
            ScanTrigger::Targeted,
        ]
        .into_iter()
        .map(|t| (t.label(), self.count_for(t).load(Ordering::Relaxed)))
        .collect();
        (counts, self.notify_received.load(Ordering::Relaxed))
    }

    fn count_for(&self, trigger: ScanTrigger) -> &AtomicU64 {
        match trigger {
            ScanTrigger::Manual => &self.manual,
            ScanTrigger::Scheduled => &self.scheduled,
            ScanTrigger::Startup => &self.startup,
            ScanTrigger::Targeted => &self.targeted,
        }
    }
}

/// How long a fresh title's Dolby Vision scan waits for the node-local index
/// pass, which reads fresh files ahead of its backlog and scans the title in
/// the same read. Past it, the title job reads the file itself.
pub(crate) const TITLE_SCAN_INDEX_WAIT: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);

pub struct JobManager {
    store: Arc<dyn Store>,
    /// Bounded, node-local repair previews. A plan ID is a lookup handle, not
    /// authority; every status/apply request authenticates its original admin.
    pub(crate) identity_repairs: crate::http::scan_identity::IdentityRepairCache,
    coordinator: StoreCoordinator,
    /// Live "may this node run leader-singleton work?" authority. Held rather
    /// than sampled once, because committed membership moves under a running
    /// daemon: a learner may be promoted, and a voter may be removed.
    job_authority: Arc<dyn ClusterJobAuthority>,
    /// Exact committed membership and internal addressing for optional peer
    /// hydration. Tests and recovery-only construction deliberately omit it.
    membership: Option<plurx_core::cluster::membership::MembershipManager>,
    artwork_dir: PathBuf,
    scan_prune_percent: u8,
    /// Test-only provider override so the targeted-scan seam can be exercised
    /// through the real job manager without reaching the public TMDB API.
    #[cfg(test)]
    tmdb_base: Option<(String, String)>,
    statuses: Mutex<HashMap<i64, ScanStatus>>,
    /// Live counters for in-flight scans, sampled by `all_statuses`.
    live: Mutex<HashMap<i64, Arc<ScanProgress>>>,
    /// Wake durable library consumers promptly after local admission.
    library_wake: tokio::sync::Notify,
    library_readiness: crate::background_jobs::LibraryReadiness,
    /// Shared with TranscodeManager when attached; standalone managers have
    /// no other encoder pool. Foreground subtitles reserve one CPU thread.
    subtitle_admissions: std::sync::Mutex<crate::admission::Admissions>,
    metrics: Arc<IntegrationMetrics>,
    /// A pre-transcode pass is running. Not a mutex, because the answer wanted
    /// is "is one going" rather than "wait for it": a second pass would fight
    /// the first for the same slots, and queuing one behind an encode that
    /// takes hours is worse than skipping it.
    producing: std::sync::atomic::AtomicBool,
    /// A fragment-indexing pass is running on this node. Same shape and same
    /// reason as `producing`: the question is "is one going", not "wait".
    indexing: std::sync::atomic::AtomicBool,
    /// Queue execution is independent of discovery cadence. This guard keeps
    /// minute scheduler ticks from stacking drain loops on the same node.
    cluster_index_working: std::sync::atomic::AtomicBool,
    cluster_analysis_working: std::sync::atomic::AtomicBool,
    background_upkeep_running: std::sync::atomic::AtomicBool,
    /// Permanent-media conversion is a bounded queue, but one slow disc may
    /// outlive many scheduler ticks. Keep exactly one local drain loop.
    dv_disk_working: std::sync::atomic::AtomicBool,
    /// Boot-time availability of the two pinned destructive-media tools.
    dv_disk_capabilities: crate::dv_disk::DvDiskCapabilities,
    /// Throttle bounded, replicated analysis-history pruning so an idle queue
    /// does not produce a Raft write on every scheduler tick.
    last_analysis_prune_ms: AtomicI64,
    /// High-frequency worker progress is deliberately node-local. Replicating
    /// every fragment would turn one long media read into sustained Raft load;
    /// the bounded peer Activity snapshot carries these rows to an ingress.
    analysis_progress: std::sync::Mutex<HashMap<(String, String), AnalysisProgress>>,
    analysis_progress_epoch: AtomicU64,
    analysis_metrics: Arc<AnalysisRuntimeMetrics>,
    /// Every progress row as it stood when its pass ended, so a test can see
    /// what a real pass wrote onto the row it no longer has.
    #[cfg(test)]
    finished_analysis_rows: std::sync::Mutex<Vec<AnalysisProgress>>,
    /// Which title the pass is on, for the activity feed.
    ///
    /// The flag above answers "may another pass start"; this answers "what is
    /// using the GPU, and why", which is the question an admin looking at a
    /// busy ffmpeg actually has. Without it the producer holds an encoder for
    /// up to six hours with no entry in the activity feed, no session in the
    /// UI and nothing to stop — `ps auxwww` on the box was the only way to
    /// find out, which is not an acceptable answer for someone's own server.
    now_producing: Mutex<Option<ProducingNow>>,
    /// Set to ask the running pass to stop after the title it is on.
    stop_producing: std::sync::atomic::AtomicBool,
    /// Jobs this node proved it cannot currently read. Claim filtering keeps
    /// the refusal local: a peer with the same absolute path mounted may take
    /// the row immediately, while this process avoids reclaiming and burning
    /// the shared retry budget every scheduler tick.
    pretranscode_refusals: Mutex<HashMap<String, i64>>,
    /// Content jobs this node cannot currently read. These exclusions remain
    /// node-local so another voter can claim the row immediately.
    fragment_index_refusals: Mutex<HashMap<String, i64>>,
    /// Logical fragment keys whose identity failed a shared read (§5.6):
    /// until the mark expires that identity is built in a pass of its own,
    /// and so is a lead that carries one. Keyed by the logical key, so a new
    /// engine or a replaced source starts unmarked. Node-local: whether to
    /// share a read is this node's execution choice, not a fact about the job.
    shared_pass_marks: Mutex<HashMap<String, i64>>,
    /// Per-node maintenance cadence for node-local cache bytes. Candidate
    /// generation is cluster-singleton and cannot maintain every worker disk.
    last_pretranscode_cache_sweep_ms: AtomicI64,
    /// Stable local cursor for the content-addressed index cache. Without a
    /// cursor each bounded pass would revisit the same legitimate head page.
    fragment_index_sweep_cursor: Mutex<Option<String>>,
    /// The subtitle-source store's own sweep cursor: `f<id>` of the last
    /// directory examined, or empty once a walk has wrapped.
    subtitle_source_sweep_cursor: Mutex<Option<String>>,
    /// The subtitle sidecar sweep's cursor: the last file id examined.
    subtitle_sidecar_sweep_cursor: Mutex<Option<i64>>,
    /// Files the last scans added or changed, oldest first, which the
    /// node-local fragment-index pass examines before its round-robin
    /// backlog. Without it a changed title waited for the cursor to wrap the
    /// whole library — about 56 hours behind 900 files (lab N4, 2026-10-11).
    fresh_index_files: std::sync::Mutex<std::collections::VecDeque<i64>>,
    /// The node-local index pass, not the cluster queue, ran last. Only then
    /// does `fresh_index_files` drain, so only then may a Dolby Vision title
    /// scan wait for the pass that would read its title anyway.
    local_index_pass: std::sync::atomic::AtomicBool,
    /// When each fresh file was noted: a title scan waits for the pass only
    /// for [`TITLE_SCAN_INDEX_WAIT`] after that.
    fresh_noted_at: std::sync::Mutex<std::collections::HashMap<i64, std::time::Instant>>,
    /// Process-local discovery count shown in the Developer backfill item.
    subtitle_backfill_enqueued_process: AtomicU64,
    /// A genre-backfill pass is running. Same reasoning as `producing`: the
    /// question is "may another one start", not "wait for this one" — two
    /// passes would read the same cursor, fetch the same titles and double
    /// the request count for nothing.
    backfilling_genres: std::sync::atomic::AtomicBool,
    /// One artwork-retry batch at a time. The scheduler spawns this job so it
    /// cannot block scans or disk cleanup; this flag is the corresponding
    /// single-flight guarantee when a slow provider outlives its interval.
    retrying_artwork: std::sync::atomic::AtomicBool,
    /// EPUB parsing uses `spawn_blocking`, which cannot be cancelled by the
    /// artwork lease. Keep timed-out reads single-flight until the blocking
    /// worker itself exits so a slow mount cannot accumulate detached reads.
    book_cover_workers: metadata::book::CoverMaterializationWorkers,
    /// What the last genre-backfill pass did. Server-wide rather than per
    /// library because the backfill is: it walks item ids, not libraries.
    /// Surfaced on the settings page, which is where an operator armed it.
    last_genre_backfill: Mutex<Option<GenreBackfillReport>>,
}

/// The title a producer pass is working on, and why it chose it.
#[derive(Clone, Debug, Serialize)]
pub struct ProducingNow {
    pub title: String,
    /// One of [`crate::produce::REASON_IN_PROGRESS`] and friends — the rail
    /// this candidate came off. "Why is it encoding *that*" is half the
    /// question, and the answer is never obvious from the filename.
    pub reason: String,
    /// 1-based position within this pass, and how many it means to attempt.
    pub index: usize,
    pub total: usize,
}

const MAX_ANALYSIS_PROGRESS: usize = 64;

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AnalysisProgress {
    pub job_id: String,
    /// Explicit correlation; legacy job_id may be an artifact key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_fence: Option<i64>,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    pub component: String,
    pub stage: String,
    pub bytes_read: u64,
    pub total_bytes: u64,
    pub media_ms_examined: i64,
    pub total_media_ms: i64,
    pub fragments_indexed: usize,
    /// PGS tracks the pass is also keeping for the subtitle-source store;
    /// zero when it is not riding along. Defaulted for a peer that predates
    /// the field.
    #[serde(default)]
    pub pgs_tracks: usize,
    /// Text tracks (including styled text) this subtitle-source pass maps.
    #[serde(default)]
    pub text_tracks: usize,
    /// Bytes the ride-along has written so far.
    #[serde(default)]
    pub pgs_bytes_written: u64,
    /// How many copy identities this row's pass builds from one read of the
    /// source; zero for a pass that builds one. A shared read occupies one
    /// background slot for several rows, and this is what says so.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub shared_read_identities: usize,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub elapsed_ms: i64,
    pub throughput_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<i64>,
    #[serde(skip)]
    registry_epoch: u64,
    /// The first byte count this attempt reported, and when. A resumed
    /// whole-file attestation reports its resume offset first. An earlier
    /// attempt read those bytes, so this attempt's rate is measured from here.
    #[serde(skip)]
    rate_origin: Option<(u64, i64)>,
}

/// This attempt's read rate and the time left at that rate. The rate counts
/// only bytes read since `origin`. Before the first report, `origin` is
/// `(0, started_at_ms)`.
fn analysis_rate(
    bytes_read: u64,
    total_bytes: u64,
    origin: (u64, i64),
    now_ms: i64,
) -> (u64, Option<i64>) {
    let (origin_bytes, origin_ms) = origin;
    let rate_ms = u64::try_from(now_ms.saturating_sub(origin_ms)).unwrap_or(0);
    let throughput_bps = if rate_ms > 0 {
        bytes_read
            .saturating_sub(origin_bytes)
            .saturating_mul(1_000)
            .saturating_div(rate_ms)
    } else {
        0
    };
    let eta_ms = (bytes_read > 0 && total_bytes > bytes_read && throughput_bps > 0).then(|| {
        total_bytes
            .saturating_sub(bytes_read)
            .saturating_mul(1_000)
            .saturating_div(throughput_bps)
            .min(i64::MAX as u64) as i64
    });
    (throughput_bps, eta_ms)
}

/// An outcome that is the pass's, not the identity's: the child failed, the
/// pass ran out of budget, or something transient. A shared pass that ends
/// this way says nothing about what each identity would do alone.
fn failed_as_a_pass(outcome: &crate::fragindex::IndexOutcome) -> bool {
    use plurx_core::content_analysis::IndexFailureCode;
    matches!(outcome, crate::fragindex::IndexOutcome::Failed(failure)
    if failure.transient_allowlisted
        || matches!(
            failure.code,
            IndexFailureCode::IndexProcessFailed | IndexFailureCode::IndexBudgetExceeded
        ))
}

/// [`failed_as_a_pass`], less the transient: those retry on their own turn.
fn failed_as_a_shared_pass(outcome: &crate::fragindex::IndexOutcome) -> bool {
    failed_as_a_pass(outcome)
        && !matches!(outcome, crate::fragindex::IndexOutcome::Failed(failure) if failure.transient_allowlisted)
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

#[cfg(test)]
impl AnalysisProgress {
    pub(crate) fn test_row(job_id: &str, title: &str) -> Self {
        Self {
            job_id: job_id.to_owned(),
            durable_job_id: None,
            durable_fence: None,
            file_id: 1,
            item_id: 1,
            title: title.to_owned(),
            component: "fragment_index".to_owned(),
            stage: "fragment_index".to_owned(),
            bytes_read: 1,
            total_bytes: 2,
            media_ms_examined: 1,
            total_media_ms: 2,
            fragments_indexed: 1,
            pgs_tracks: 0,
            text_tracks: 0,
            pgs_bytes_written: 0,
            shared_read_identities: 0,
            started_at_ms: 1,
            updated_at_ms: 1,
            elapsed_ms: 1,
            throughput_bps: 1,
            eta_ms: Some(1),
            registry_epoch: 0,
            rate_origin: None,
        }
    }
}

#[cfg(test)]
mod analysis_rate_tests;

const ANALYSIS_STAGES: [&str; 7] = [
    "probing",
    "fragment_index",
    "subtitle_source",
    "fingerprints",
    "marker_correlation",
    "persisting",
    "verifying",
];
const ANALYSIS_BYTES_BOUNDS: [u64; 8] = [
    1 << 20,
    8 << 20,
    32 << 20,
    128 << 20,
    512 << 20,
    1 << 30,
    4 << 30,
    16 << 30,
];
const ANALYSIS_SECONDS_BOUNDS: [u64; 8] = [1, 5, 15, 30, 60, 300, 900, 1_800];
const ANALYSIS_THROUGHPUT_BOUNDS: [u64; 8] = [
    128 << 10,
    512 << 10,
    1 << 20,
    4 << 20,
    16 << 20,
    64 << 20,
    256 << 20,
    1 << 30,
];

struct AnalysisHistogram {
    buckets: [AtomicU64; 9],
    count: AtomicU64,
    sum: AtomicU64,
}

impl Default for AnalysisHistogram {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
        }
    }
}

impl AnalysisHistogram {
    fn record(&self, value: u64, bounds: &[u64; 8]) {
        let slot = bounds
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(bounds.len());
        self.buckets[slot].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
    }

    fn render(&self, out: &mut String, name: &str, help: &str, bounds: &[u64; 8]) {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
        let mut cumulative = 0;
        for (slot, bound) in bounds.iter().enumerate() {
            cumulative += self.buckets[slot].load(Ordering::Relaxed);
            out.push_str(&format!("{name}_bucket{{le=\"{bound}\"}} {cumulative}\n"));
        }
        cumulative += self.buckets[bounds.len()].load(Ordering::Relaxed);
        out.push_str(&format!(
            "{name}_bucket{{le=\"+Inf\"}} {cumulative}\n{name}_sum {}\n{name}_count {}\n",
            self.sum.load(Ordering::Relaxed),
            self.count.load(Ordering::Relaxed),
        ));
    }
}

/// What became of one lease heartbeat tick, or one outcome write.
///
/// These are per-process counters, so they reset on restart — but a queue that
/// loses every lease it takes shows it within one scheduler pass, which is
/// what the store's cumulative lifecycle counters could not do from a scrape.
#[derive(Clone, Copy)]
pub(crate) enum LeaseEvent {
    /// The store moved the lease we asked it to move.
    Renewed,
    /// The renewal errored. The lease is still ours until its deadline.
    RenewFailed,
    /// The renewal matched no row: another node owns it now.
    Lost,
    /// No renewal succeeded before the lease we held ran out.
    Expired,
    /// A terminal write — fail, yield or complete — matched no row or errored,
    /// so the row keeps whatever state the sweep eventually gives it.
    OutcomeWriteLost,
}

const LEASE_EVENTS: [&str; 5] = [
    "renewed",
    "renew_failed",
    "lost",
    "expired",
    "outcome_write_lost",
];

#[derive(Default)]
pub(crate) struct AnalysisRuntimeMetrics {
    current_by_stage: [AtomicU64; ANALYSIS_STAGES.len()],
    bytes: AnalysisHistogram,
    media_seconds: AnalysisHistogram,
    wall_seconds: AnalysisHistogram,
    throughput: AnalysisHistogram,
    publications: [[AtomicU64; 2]; 2],
    correlations: [AtomicU64; 3],
    leases: [AtomicU64; LEASE_EVENTS.len()],
}

impl AnalysisRuntimeMetrics {
    fn stage_index(stage: &str) -> usize {
        let normalized = match stage {
            "source_probe" => "probing",
            "hashing" => "fingerprints",
            "staged" | "publishing" => "verifying",
            other => other,
        };
        ANALYSIS_STAGES
            .iter()
            .position(|candidate| *candidate == normalized)
            .unwrap_or(ANALYSIS_STAGES.len() - 1)
    }

    fn start(&self, stage: &str) {
        self.current_by_stage[Self::stage_index(stage)].fetch_add(1, Ordering::Relaxed);
    }

    fn transition(&self, from: &str, to: &str) {
        let from = Self::stage_index(from);
        let to = Self::stage_index(to);
        if from != to {
            self.current_by_stage[from].fetch_sub(1, Ordering::Relaxed);
            self.current_by_stage[to].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn finish(&self, progress: &AnalysisProgress) {
        self.current_by_stage[Self::stage_index(&progress.stage)].fetch_sub(1, Ordering::Relaxed);
        let wall_seconds = u64::try_from(clock_ms().saturating_sub(progress.started_at_ms))
            .unwrap_or_default()
            / 1_000;
        let media_seconds =
            u64::try_from(progress.media_ms_examined.max(0)).unwrap_or_default() / 1_000;
        let throughput = progress
            .bytes_read
            .checked_div(wall_seconds)
            .unwrap_or(progress.bytes_read);
        self.bytes
            .record(progress.bytes_read, &ANALYSIS_BYTES_BOUNDS);
        self.media_seconds
            .record(media_seconds, &ANALYSIS_SECONDS_BOUNDS);
        self.wall_seconds
            .record(wall_seconds, &ANALYSIS_SECONDS_BOUNDS);
        self.throughput
            .record(throughput, &ANALYSIS_THROUGHPUT_BOUNDS);
    }

    fn discard(&self, progress: &AnalysisProgress) {
        self.current_by_stage[Self::stage_index(&progress.stage)].fetch_sub(1, Ordering::Relaxed);
    }

    fn publication(&self, component: &str, replacement: bool) {
        let component = usize::from(component == "skip_markers");
        self.publications[component][usize::from(replacement)].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn lease_event(&self, event: LeaseEvent) {
        let slot = match event {
            LeaseEvent::Renewed => 0,
            LeaseEvent::RenewFailed => 1,
            LeaseEvent::Lost => 2,
            LeaseEvent::Expired => 3,
            LeaseEvent::OutcomeWriteLost => 4,
        };
        self.leases[slot].fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn lease_count(&self, event: LeaseEvent) -> u64 {
        let slot = match event {
            LeaseEvent::Renewed => 0,
            LeaseEvent::RenewFailed => 1,
            LeaseEvent::Lost => 2,
            LeaseEvent::Expired => 3,
            LeaseEvent::OutcomeWriteLost => 4,
        };
        self.leases[slot].load(Ordering::Relaxed)
    }

    pub(crate) fn prometheus(&self, owner_node: &str) -> String {
        let owner_node = owner_node
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let mut out = String::new();
        self.bytes.render(
            &mut out,
            "plurx_analysis_indexing_bytes",
            "Bytes read by completed analysis work.",
            &ANALYSIS_BYTES_BOUNDS,
        );
        self.media_seconds.render(
            &mut out,
            "plurx_analysis_media_seconds",
            "Media time examined by completed analysis work.",
            &ANALYSIS_SECONDS_BOUNDS,
        );
        self.wall_seconds.render(
            &mut out,
            "plurx_analysis_wall_seconds",
            "Wall time consumed by completed analysis work.",
            &ANALYSIS_SECONDS_BOUNDS,
        );
        self.throughput.render(
            &mut out,
            "plurx_analysis_throughput_bytes_per_second",
            "Read throughput of completed analysis work.",
            &ANALYSIS_THROUGHPUT_BOUNDS,
        );
        out.push_str(
            "# HELP plurx_analysis_current_jobs Current node-local analysis work by stage and owner.\n\
             # TYPE plurx_analysis_current_jobs gauge\n",
        );
        for (stage, value) in ANALYSIS_STAGES.iter().zip(&self.current_by_stage) {
            out.push_str(&format!(
                "plurx_analysis_current_jobs{{stage=\"{stage}\",owner_node=\"{owner_node}\"}} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }
        out.push_str(
            "# HELP plurx_analysis_artifact_publications_total Validated analysis artifact publications and replacements.\n\
             # TYPE plurx_analysis_artifact_publications_total counter\n",
        );
        for (component_index, component) in ["fragment_index", "skip_markers"].iter().enumerate() {
            let version = if component_index == 0 {
                plurx_core::segplan::SEGPLAN_VERSION.to_string()
            } else {
                crate::http::stream::CHAPTER_ANNOTATION_VERSION.to_owned()
            };
            for (operation_index, operation) in ["publication", "replacement"].iter().enumerate() {
                out.push_str(&format!(
                    "plurx_analysis_artifact_publications_total{{component=\"{component}\",operation=\"{operation}\",version=\"{version}\"}} {}\n",
                    self.publications[component_index][operation_index].load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str(
            "# HELP plurx_analysis_correlation_total Series marker-correlation outcomes.\n\
             # TYPE plurx_analysis_correlation_total counter\n",
        );
        for (outcome, value) in ["candidate", "accepted", "rejected_ambiguous"]
            .iter()
            .zip(&self.correlations)
        {
            out.push_str(&format!(
                "plurx_analysis_correlation_total{{outcome=\"{outcome}\"}} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }
        out.push_str(
            "# HELP plurx_analysis_lease_total Analysis queue lease heartbeat and outcome-write events on this process since it started.\n\
             # TYPE plurx_analysis_lease_total counter\n",
        );
        for (event, value) in LEASE_EVENTS.iter().zip(&self.leases) {
            out.push_str(&format!(
                "plurx_analysis_lease_total{{event=\"{event}\"}} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }
        out
    }
}

pub(crate) struct AnalysisProgressGuard {
    jobs: Arc<JobManager>,
    job_id: String,
    target_node_id: String,
    registry_epoch: u64,
}

impl AnalysisProgressGuard {
    pub(crate) fn stage(&self, stage: &str) {
        let mut rows = self
            .jobs
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(row) = rows.get_mut(&(self.job_id.clone(), self.target_node_id.clone())) {
            if row.registry_epoch == self.registry_epoch {
                self.jobs.analysis_metrics.transition(&row.stage, stage);
                row.stage = stage.into();
                row.updated_at_ms = clock_ms();
                row.elapsed_ms = row.updated_at_ms.saturating_sub(row.started_at_ms).max(0);
            }
        }
    }

    fn attach_attempt(&self, token: &plurx_core::store::background_jobs::JobToken) {
        let mut rows = self
            .jobs
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(row) = rows.get_mut(&(self.job_id.clone(), self.target_node_id.clone())) {
            if row.registry_epoch == self.registry_epoch {
                row.durable_job_id = Some(token.job_id.clone());
                row.durable_fence = Some(token.fence);
            }
        }
    }
}

impl Drop for AnalysisProgressGuard {
    fn drop(&mut self) {
        self.jobs
            .remove_analysis_progress(&self.job_id, &self.target_node_id, self.registry_epoch);
    }
}

/// Clears [`JobManager::backfilling_genres`] however the pass ends, panic
/// included. A flag left set by an early return is a job that never runs
/// again until the process restarts, and this one has no interval to make
/// that visible.
struct GenreBackfillGuard(Arc<JobManager>);

impl Drop for GenreBackfillGuard {
    fn drop(&mut self) {
        self.0.backfilling_genres.store(false, Ordering::Relaxed);
    }
}

/// Clears [`JobManager::retrying_artwork`] however a spawned retry pass ends.
struct ArtworkRetryGuard(Arc<JobManager>);

impl Drop for ArtworkRetryGuard {
    fn drop(&mut self) {
        self.0.retrying_artwork.store(false, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct ArtworkSweepResult {
    repaired: usize,
    #[cfg(test)]
    claimed_ids: Vec<i64>,
}

/// Ceiling on one indexing pass. Small so a backfill shares the node with
/// foreground playback instead of trying to drain the whole library at once.
const INDEX_MAX_PER_PASS: usize = 4;
/// Files one pass will even look at. An already-indexed library attempts
/// nothing, so without this the pass would query every file every minute for
/// the life of the server.
const INDEX_MAX_EXAMINED_PER_PASS: usize = 200;
/// Stop *starting* new files after this much wall time. A file already in
/// flight owns its independently bounded budget below.
const INDEX_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
/// Short files retain the original ceiling. Long remuxes need a duration-sized
/// allowance: indexing is a complete video-bitstream pass, and a measured
/// 14x Dolby Vision copy on media1 needs about nine minutes for a two-hour film.
const INDEX_FILE_BUDGET_FLOOR_SECS: u64 = 90;
const INDEX_FILE_BUDGET_CEILING_SECS: u64 = 30 * 60;
const INDEX_EXPECTED_MIN_SPEED: u64 = 8;
const INDEX_FILE_HEADROOM_SECS: u64 = 30;
/// The slowest whole-file read rate a first attempt plans for. A duration-only
/// budget assumes the read keeps up with eight times real time, which a
/// 50-90 Mb/s remux on a network mount does not: lab5 measured 33-40 MB/s and
/// killed every 70-80 GB remux it tried at 40-64%. 24 MiB/s leaves a margin
/// under the slowest disk rate measured there, so a healthy first read is not
/// cut off at the very end.
const INDEX_EXPECTED_MIN_READ_BYTES_PER_SEC: u64 = 24 * 1024 * 1024;
/// The ceiling a budget derived from the file's size or from a measured read
/// may reach. The 30-minute ceiling is a guess about time; these budgets are
/// derived from what the file and this node actually are, and both local and
/// queued passes yield to viewers, so a long one costs playback nothing.
const INDEX_MEASURED_BUDGET_CEILING_SECS: u64 = 6 * 60 * 60;

/// The progress-row key of a local (non-queue) index pass: the file and the
/// pipeline's argv fingerprint, so a file's identities get a row each, and a
/// prefix no queue cache key (a hex digest) can take.
fn local_index_job_id(file_id: i64, identity: &plurx_core::segplan::SourceIdentity) -> String {
    let fingerprint = identity
        .argv_fingerprint
        .get(..12)
        .unwrap_or(&identity.argv_fingerprint);
    format!("local-index:{file_id}:{fingerprint}")
}

/// The wall-clock budget for one whole-file index read.
///
/// Three bounds, the largest wins: the film's length read at eight times real
/// time (capped at 30 minutes, as before); the file's size read at the slowest
/// rate a network mount is expected to sustain; and, after an attempt that ran
/// out of budget, what that attempt measured. The last two are capped
/// separately and higher, because they are derived from this file and this
/// node rather than guessed.
fn index_file_budget(
    duration_ms: Option<i64>,
    size_bytes: i64,
    previous: Option<&plurx_core::segplan::FragmentIndexOutcome>,
) -> Duration {
    let film_secs = duration_ms
        .filter(|milliseconds| *milliseconds > 0)
        .map_or(0, |milliseconds| (milliseconds as u64).div_ceil(1_000));
    let estimated = film_secs
        .div_ceil(INDEX_EXPECTED_MIN_SPEED)
        .saturating_add(INDEX_FILE_HEADROOM_SECS)
        .clamp(INDEX_FILE_BUDGET_FLOOR_SECS, INDEX_FILE_BUDGET_CEILING_SECS);
    let by_size = u64::try_from(size_bytes.max(0))
        .unwrap_or(0)
        .div_ceil(INDEX_EXPECTED_MIN_READ_BYTES_PER_SEC)
        .saturating_add(INDEX_FILE_HEADROOM_SECS)
        .min(INDEX_MEASURED_BUDGET_CEILING_SECS);
    let measured = measured_index_budget_secs(duration_ms, previous).unwrap_or(0);
    Duration::from_secs(estimated.max(by_size).max(measured))
}

/// What an attempt that ran out of budget measured: the time it would have
/// needed for the whole film at the rate it actually read, plus a quarter.
///
/// `None` unless the previous attempt is a typed `index_budget_exceeded` with
/// the elapsed time and the coverage it reached, which is what every budget
/// failure records.
fn measured_index_budget_secs(
    duration_ms: Option<i64>,
    previous: Option<&plurx_core::segplan::FragmentIndexOutcome>,
) -> Option<u64> {
    let previous = previous?;
    if previous.typed_code
        != Some(plurx_core::content_analysis::IndexFailureCode::IndexBudgetExceeded)
    {
        return None;
    }
    let diagnostic = previous.diagnostic.as_ref()?;
    let elapsed = u128::from(diagnostic.elapsed_ms.filter(|value| *value > 0)?);
    let covered = u128::try_from(diagnostic.covered_ms.filter(|value| *value > 0)?).ok()?;
    let expected = diagnostic
        .expected_ms
        .or(duration_ms)
        .filter(|value| *value > 0)
        .and_then(|value| u128::try_from(value).ok())?;
    let needed_ms = elapsed
        .saturating_mul(expected.max(covered))
        .checked_div(covered)?
        .saturating_mul(5)
        / 4;
    let secs = u64::try_from(needed_ms.div_ceil(1_000)).unwrap_or(u64::MAX);
    Some(
        secs.saturating_add(INDEX_FILE_HEADROOM_SECS)
            .min(INDEX_MEASURED_BUDGET_CEILING_SECS),
    )
}

/// A local whole-file read owes viewers the same preemption as a queued
/// cluster pass. Dropping the losing build kills its child and removes its
/// partial subtitle stage; callers must return without recording a refusal
/// or advancing the cursor so the interrupted file remains due.
///
/// The read runs under `admission`, its turn on the heavy-background lane,
/// so the pool is never idle while it runs: what preempts it is foreground
/// demand beyond that admission's own claim, not the claim itself.
///
/// Known limit: only work that holds an admission is visible here. A
/// copy-mode or VOD viewer reading the source without one does not preempt
/// the read, and a preempted read starts over from the beginning next time.
pub(crate) async fn local_index_until_foreground<T>(
    transcode: &TranscodeManager,
    admission: &Arc<crate::transcode::FragmentAdmission>,
    build: impl std::future::Future<Output = T>,
) -> Option<T> {
    let keeper: Arc<dyn plurx_core::process::bounded::OriginalWorkKeeper> = admission.clone();
    let build = plurx_core::process::bounded::with_original_work_keeper(keeper, build);
    tokio::select! {
        biased;
        () = async {
            while !transcode.fragment_must_yield(admission) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } => None,
        result = build => Some(result),
    }
}

/// Stable, wrapping order for one bounded pass. The cursor is the last file a
/// prior pass examined, successful or not; starting after it prevents a few
/// permanently slow/unsupported rows from starving every later title.
/// The local pass's examination order: files a refused start demanded (newest
/// first), then files the scans added or changed (oldest first), then the
/// round-robin backlog from the cursor. Only files still in a library are
/// returned, each once; the flag marks the backlog entries, the only ones
/// that advance the cursor.
fn prioritised_index_paths(
    paths: Vec<(i64, PathBuf)>,
    cursor: Option<i64>,
    demanded: &[i64],
    fresh: &[i64],
) -> Vec<(i64, PathBuf, bool)> {
    let wanted: HashSet<i64> = demanded.iter().chain(fresh).copied().collect();
    let mut found = HashMap::new();
    let mut backlog = Vec::new();
    for (file_id, path) in ordered_index_paths(paths, cursor) {
        if wanted.contains(&file_id) {
            found.insert(file_id, path);
        } else {
            backlog.push((file_id, path, true));
        }
    }
    let mut ordered = Vec::with_capacity(found.len() + backlog.len());
    for file_id in demanded.iter().chain(fresh) {
        if let Some(path) = found.remove(file_id) {
            ordered.push((*file_id, path, false));
        }
    }
    ordered.extend(backlog);
    ordered
}

fn ordered_index_paths(mut paths: Vec<(i64, PathBuf)>, cursor: Option<i64>) -> Vec<(i64, PathBuf)> {
    paths.sort_unstable_by_key(|(file_id, _)| *file_id);
    let split = cursor.map_or(0, |cursor| {
        paths.partition_point(|(file_id, _)| *file_id <= cursor)
    });
    paths.rotate_left(split);
    paths
}

fn ordered_cluster_index_paths(
    paths: Vec<(i64, PathBuf)>,
    cursor: Option<i64>,
    node_id: &str,
) -> Vec<(i64, PathBuf)> {
    let mut paths = ordered_index_paths(paths, cursor);
    if cursor.is_none() && !paths.is_empty() {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in node_id.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let offset = usize::try_from(hash % paths.len() as u64).unwrap_or(0);
        paths.rotate_left(offset);
    }
    paths
}

fn setting_enabled(value: Option<String>) -> bool {
    plurx_core::store::stored_switch(value.as_deref(), false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnalysisRetryPolicy {
    lease_ms: i64,
    backoff_base_ms: i64,
    backoff_max_ms: i64,
    max_attempts: u32,
}

#[derive(Clone)]
struct ClusterFragmentIndexWorker {
    engine_sha256: String,
    cache_root: PathBuf,
    have_dovi: bool,
    /// Whether this node converts Dolby Vision Profile 7 to 8.1, which decides
    /// whether a convertible file has a third identity to index at all. Read
    /// once with `have_dovi` beside it rather than per job: both are node
    /// facts, and reading them at different moments is how a worker comes to
    /// enumerate one identity set and index another.
    convert_dolby_vision: bool,
    retry_policy: AnalysisRetryPolicy,
}

impl ClusterFragmentIndexWorker {
    fn pipeline(&self) -> crate::background_jobs::FragmentPipeline<'_> {
        crate::background_jobs::FragmentPipeline {
            engine_sha256: &self.engine_sha256,
            have_dovi: self.have_dovi,
            convert_dolby_vision: self.convert_dolby_vision,
        }
    }
}

impl AnalysisRetryPolicy {
    fn from_settings(settings: &BTreeMap<String, String>) -> Self {
        let lease_ms = plurx_core::store::bounded_analysis_lease_secs(
            settings.get(keys::ANALYSIS_LEASE_SECS).map(String::as_str),
        )
        .saturating_mul(1_000);
        let backoff_base_ms = plurx_core::store::bounded_analysis_backoff_base_secs(
            settings
                .get(keys::ANALYSIS_BACKOFF_BASE_SECS)
                .map(String::as_str),
        )
        .saturating_mul(1_000);
        let backoff_max_ms = plurx_core::store::bounded_analysis_backoff_max_secs(
            settings
                .get(keys::ANALYSIS_BACKOFF_MAX_SECS)
                .map(String::as_str),
        )
        .saturating_mul(1_000)
        .max(backoff_base_ms);
        let max_attempts = u32::try_from(plurx_core::store::bounded_analysis_max_attempts(
            settings
                .get(keys::ANALYSIS_MAX_ATTEMPTS)
                .map(String::as_str),
        ))
        .unwrap_or(u32::MAX);
        Self {
            lease_ms,
            backoff_base_ms,
            backoff_max_ms,
            max_attempts,
        }
    }

    fn renew_every(self) -> Duration {
        Duration::from_millis(u64::try_from((self.lease_ms / 3).max(1_000)).unwrap_or(1_000))
    }

    /// Stable jitter keeps peers from synchronizing while preserving an
    /// operator-visible next-attempt timestamp across process restarts.
    fn backoff_ms(self, identity: &str, attempt: i64) -> i64 {
        plurx_core::store::analysis_backoff_ms(
            identity,
            attempt,
            self.backoff_base_ms,
            self.backoff_max_ms,
        )
    }
}

/// The attestation regime a non-forced request belongs to.
///
/// `enqueue_analysis_request` refuses a request whose `requested_generation`
/// already exists in **any** state, terminal included, so a row that failed
/// under a broken queue blocks every later request for the same file forever.
/// Moving this token moves every non-forced generation once, which lets
/// discovery re-request a library whose rows are stranded — without deleting
/// or editing a single row, so the tombstones stay as history.
///
/// Change it only when the attestation itself changes meaning, and change it
/// alongside `ATTESTATION_REGIME` and `SAMPLE_DOMAIN` in
/// `fragment_index_cluster`: this token reopens the request, and that one
/// invalidates the memo the reopened request would otherwise short-circuit to.
const ANALYSIS_ATTESTATION_GENERATION: &str = "source-attestation/sampled-v1";

/// Why a cluster-job attestation did not produce a usable source.
///
/// The two are different faults and the queue row shows the difference: a
/// refusal is the source disagreeing with what the job asked for, a timeout is
/// a filesystem that stopped answering a 64 MiB read.
#[derive(Debug)]
enum AttestationFailure {
    Refused(String),
    TimedOut,
}

/// The durable `last_error_code` for one attestation outcome.
///
/// `Ok` reaches here only when the digest did not match the job's, which is
/// the source having changed under a queued identity — a refusal, not a
/// deadline.
fn attestation_failure_code(
    outcome: &Result<crate::fragment_index_cluster::AttestedSource, AttestationFailure>,
) -> &'static str {
    match outcome {
        Err(AttestationFailure::TimedOut) => "source_attestation_timeout",
        Ok(_) | Err(AttestationFailure::Refused(_)) => "source_attestation_failed",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnalysisResolutionError {
    /// Ownership already moved. A stale resolver deliberately writes nothing.
    ClaimLost,
    /// Foreground preemption does not consume an attempt; actual I/O and
    /// control-plane failures do.
    Retry {
        code: &'static str,
        charge_attempt: bool,
    },
    Terminal(&'static str),
}

/// Remember contention throughout a source read, even if playback ends before
/// its deadline. A busy-viewer timeout must not exhaust the durable retry budget.
#[derive(Default)]
struct AnalysisAttestationBudget(std::sync::atomic::AtomicBool);

impl AnalysisAttestationBudget {
    fn observe_busy(&self, busy: bool) {
        if busy {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn deadline_failure(&self, busy_now: bool) -> AnalysisResolutionError {
        self.observe_busy(busy_now);
        AnalysisResolutionError::Retry {
            code: "source_attestation_timeout",
            charge_attempt: !self.0.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FragmentSourceReadFailure {
    Stale,
    Transient,
}

fn classify_fragment_source_read(
    result: Result<Option<MediaFile>, StoreError>,
    source_size: i64,
    source_mtime: i64,
) -> Result<MediaFile, FragmentSourceReadFailure> {
    match result {
        Ok(Some(file)) if file.size == source_size && file.mtime == source_mtime => Ok(file),
        Ok(_) => Err(FragmentSourceReadFailure::Stale),
        Err(_) => Err(FragmentSourceReadFailure::Transient),
    }
}

/// Clears [`JobManager::indexing`] however the pass ends, including the ways a
/// `?` or a panic would leave it set for the life of the process.
struct IndexingGuard(Arc<JobManager>);

impl Drop for IndexingGuard {
    fn drop(&mut self) {
        self.0.indexing.store(false, Ordering::Relaxed);
    }
}

struct BackgroundUpkeepGuard(Arc<JobManager>);
impl Drop for BackgroundUpkeepGuard {
    fn drop(&mut self) {
        self.0
            .background_upkeep_running
            .store(false, Ordering::Release);
    }
}

struct ClusterIndexWorkingGuard(Arc<JobManager>);

impl Drop for ClusterIndexWorkingGuard {
    fn drop(&mut self) {
        self.0.cluster_index_working.store(false, Ordering::Relaxed);
    }
}

struct ClusterAnalysisWorkingGuard(Arc<JobManager>);

impl Drop for ClusterAnalysisWorkingGuard {
    fn drop(&mut self) {
        self.0
            .cluster_analysis_working
            .store(false, Ordering::Release);
    }
}

struct DvDiskWorkingGuard(Arc<JobManager>);

impl Drop for DvDiskWorkingGuard {
    fn drop(&mut self) {
        self.0.dv_disk_working.store(false, Ordering::Relaxed);
    }
}

/// Clears [`JobManager::producing`] however the pass ends — including the ways
/// a `?` or a panic would leave it set forever, which would silently stop the
/// producer for the life of the process.
struct ProducingGuard(Arc<JobManager>);

impl Drop for ProducingGuard {
    fn drop(&mut self) {
        self.0.producing.store(false, Ordering::Relaxed);
        self.0.stop_producing.store(false, Ordering::Relaxed);
        // The label has to go with the flag. A pass that ends between titles
        // would otherwise leave the activity feed claiming an encode that is
        // not happening — worse than saying nothing, because it is wrong.
        if let Ok(mut now) = self.0.now_producing.try_lock() {
            *now = None;
        }
    }
}

/// Ceiling on what one pass will attempt, across every user and rail.
const PRODUCE_MAX_PER_PASS: usize = 12;
/// Hard fan-out bound after the shared demand snapshot has been ranked.
const PRODUCE_DISCOVERY_ITEMS: usize = 64;

/// How long one pass may spend. A bound rather than "until the list is done"
/// because the list is never done: it is regenerated every interval from what
/// people are actually watching, and a pass that ran for a day would be
/// producing yesterday's predictions.
const PRODUCE_WINDOW: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Maximum posterless rows one artwork-retry pass may claim. The durable
/// per-item attempt stamp drains a larger backlog over successive passes.
const ARTWORK_RETRY_BATCH: i64 = 200;

/// Artwork slots maintained by the automatic retry path. Seasons and
/// episodes have one card image; movies and shows also own a hero backdrop.
fn needs_artwork_retry(item: &Item) -> bool {
    item.poster_path.is_none()
        || (matches!(
            item.kind,
            plurx_core::domain::ItemKind::Movie | plurx_core::domain::ItemKind::Show
        ) && item.backdrop_path.is_none())
}

pub use plurx_core::store::background_jobs_library::{
    LibraryBookHints as BookHints, LibraryIdHints as IdHints,
};

fn curator_pairing_can_advance(
    current_edition: Option<&str>,
    current_provider_cover: bool,
    requested_edition: &str,
    replacement_cover_ready: bool,
) -> bool {
    !current_provider_cover || current_edition == Some(requested_edition) || replacement_cover_ready
}

/// One "scan exactly this" ask.
#[derive(Clone, Debug)]
pub struct ScanRequest {
    pub id: String,
    pub library_id: i64,
    pub path: PathBuf,
    /// Applied by the JOB, not by the endpoint — a request served from the
    /// pending queue must apply its ids too, and an endpoint that applied
    /// them itself would silently drop them for every request that arrived
    /// while a scan was running. Which is most of them.
    pub ids: Option<IdHints>,
    pub book: Option<BookHints>,
    pub correlation_id: Option<String>,
    pub source: Option<String>,
}

/// What happened to a request, for the operator and for the caller polling
/// its status.
#[derive(Clone, Debug, Serialize)]
pub struct ScanRequestRecord {
    pub request_id: String,
    pub at: i64,
    pub library_id: i64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// queued | running | done | failed
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ScanReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<PlacedFile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DvDiskMode {
    #[default]
    Off,
    Manual,
    Auto,
}

impl DvDiskMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "manual" => Some(Self::Manual),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

fn parse_dv_disk_modes(raw: Option<&str>) -> BTreeMap<i64, DvDiskMode> {
    let Some(object) = raw
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
    else {
        return BTreeMap::new();
    };
    object
        .into_iter()
        .filter_map(|(id, mode)| {
            Some((id.parse::<i64>().ok()?, DvDiskMode::parse(mode.as_str()?)?))
        })
        .filter(|(_, mode)| *mode != DvDiskMode::Off)
        .collect()
}

fn parse_dv_disk_keep_original(raw: Option<&str>) -> bool {
    !raw.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn parse_dv_disk_parallel(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 8)
}

/// Bounded observation page; durable receipts outlive this page for seven days.
const MAX_REQUESTS: usize = plurx_core::store::background_jobs_library::MAX_LIBRARY_REQUESTS;
/// Independent requests admitted for one library before callers retry later.
const MAX_PENDING_PER_LIBRARY: usize =
    plurx_core::store::background_jobs_library::MAX_LIBRARY_REQUESTS;

const PRETRANSCODE_REFUSAL_TTL_MS: i64 = 10 * 60 * 1_000;
// The store admits at most 4,096 active queue rows. Retaining that entire
// bounded universe avoids rotating one unreadable high-priority row back into
// eligibility before a lower-priority row on this node's mount can be found.
const MAX_PRETRANSCODE_REFUSALS: usize = 4_096 + PRODUCE_MAX_PER_PASS;

fn lease_time_remaining(expires_at_unix_ms: i64) -> std::time::Duration {
    let now_unix_ms = clock_ms();
    std::time::Duration::from_millis(expires_at_unix_ms.saturating_sub(now_unix_ms).max(0) as u64)
}

/// Report a terminal fragment-index write that did not land.
///
/// `fail`, `yield` and `complete` all answer `Ok(false)` when the row moved
/// under the worker, and `Err` when the store refused the statement outright.
/// Eighteen call sites discarded both with `let _ = …`, which is most of why a
/// queue that failed every job it claimed for seventy-two hours produced no log
/// line saying so. The row is not repaired here — the sweep owns that — but the
/// loss is visible.
fn record_analysis_outcome(
    metrics: &AnalysisRuntimeMetrics,
    job: &plurx_core::store::ClusterFragmentIndexJob,
    action: &str,
    code: &str,
    result: Result<bool, StoreError>,
) -> bool {
    let code = if code.is_empty() { "-" } else { code };
    match result {
        Ok(true) => true,
        Ok(false) => {
            metrics.lease_event(LeaseEvent::OutcomeWriteLost);
            tracing::warn!(
                cache_key = job.cache_key,
                target_node_id = job.target_node_id,
                file_id = job.file_id,
                fence = job.fence,
                attempts = job.attempts,
                action,
                code,
                "fragment-index outcome matched no row; the claim was already gone"
            );
            false
        }
        Err(error) => {
            metrics.lease_event(LeaseEvent::OutcomeWriteLost);
            tracing::warn!(
                cache_key = job.cache_key,
                target_node_id = job.target_node_id,
                file_id = job.file_id,
                fence = job.fence,
                attempts = job.attempts,
                action,
                code,
                %error,
                "fragment-index outcome could not be written"
            );
            false
        }
    }
}

/// One queue row's lease: what it names in a log line, and its terms.
pub(crate) struct LeaseHeartbeat {
    /// `fragment-index` or `analysis-request`.
    pub(crate) queue: &'static str,
    /// `cache_key:target_node_id`, or the request id.
    pub(crate) row: String,
    pub(crate) fence: i64,
    pub(crate) attempts: i64,
    /// The expiry the claim granted. Everything after it is measured on the
    /// runtime clock from a successful renewal.
    pub(crate) known_expiry_ms: i64,
    pub(crate) lease_ms: i64,
    pub(crate) renew_every: Duration,
    pub(crate) metrics: Arc<AnalysisRuntimeMetrics>,
    pub(crate) stop: tokio_util::sync::CancellationToken,
    pub(crate) lost: tokio_util::sync::CancellationToken,
}

/// Renew one queue row's lease until the work stops, the row stops being ours,
/// or the lease we actually hold runs out.
///
/// Three properties, each of which was absent from the two heartbeats this
/// replaces, and the first of which cost seventy-two hours of index builds:
///
/// * **The first tick is consumed.** `tokio::time::interval` completes its
///   first tick immediately, so a renewal that always fails — as the
///   fragment-index one did, refused by the placeholder validator before any
///   I/O — declared the lease lost microseconds after the claim, before the
///   worker had read the catalog row.
/// * **A store error is not a lost lease.** `STORE_TIMEOUT` is three seconds
///   and the lease is sixty; abandoning a build the moment one renewal times
///   out throws away work this node still holds the right to finish. Errors
///   are retried on the next tick.
/// * **The deadline is an arm, not a check.** Tolerating errors is only safe
///   while the lease is genuinely alive, and sampling `clock_ms()` on ticks
///   can overrun the other nodes' sweep by a whole store timeout. Racing each
///   renewal against a timer that expires with the lease self-fences at the
///   moment the lease we know about runs out. That timer is measured on the
///   runtime clock rather than the wall clock, so an NTP step cannot extend a
///   lease the other nodes are already counting down.
///
/// Taking a closure rather than a `Store` is deliberate: `Store` is a blanket
/// implementation over two dozen traits, so a stub is not a test double this
/// repository can afford, and the behaviour worth testing is the loop.
impl LeaseHeartbeat {
    pub(crate) async fn run<Renew, Renewal>(self, mut renew: Renew)
    where
        Renew: FnMut(i64, i64) -> Renewal + Send,
        Renewal: std::future::Future<Output = Result<bool, StoreError>> + Send,
    {
        let Self {
            queue,
            row,
            fence,
            attempts,
            known_expiry_ms,
            lease_ms,
            renew_every,
            metrics,
            stop,
            lost,
        } = self;
        let mut ticker = tokio::time::interval(renew_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The immediate first tick belongs to the claim that just happened.
        ticker.tick().await;
        let mut expires_at = tokio::time::Instant::now() + lease_time_remaining(known_expiry_ms);
        loop {
            let deadline = tokio::time::sleep_until(expires_at);
            tokio::pin!(deadline);
            tokio::select! {
                () = stop.cancelled() => break,
                () = &mut deadline => {
                    metrics.lease_event(LeaseEvent::Expired);
                    lost.cancel();
                    tracing::warn!(
                        queue, row, fence, attempts,
                        "analysis lease expired without a successful renewal"
                    );
                    break;
                }
                _ = ticker.tick() => {}
            }
            let now = clock_ms();
            let issued_at = tokio::time::Instant::now();
            let requested_expiry_ms = now.saturating_add(lease_ms);
            let renewal = renew(now, requested_expiry_ms);
            tokio::pin!(renewal);
            let renewed = tokio::select! {
                // A renewal still in flight at retirement is dropped rather
                // than drained. What makes that safe is not the fence — `fail`
                // and `yield` leave it unchanged — but `AND state = 'running'`
                // in both renew statements: a late renewal cannot move a row
                // the outcome write has already settled.
                () = stop.cancelled() => break,
                () = &mut deadline => {
                    metrics.lease_event(LeaseEvent::Expired);
                    lost.cancel();
                    tracing::warn!(
                        queue, row, fence, attempts,
                        "analysis lease renewal exceeded its deadline and self-fenced"
                    );
                    break;
                }
                result = &mut renewal => result,
            };
            match renewed {
                Ok(true) => {
                    metrics.lease_event(LeaseEvent::Renewed);
                    // Anchored to the moment the renewal was *issued*, not the
                    // moment it was acknowledged. The row now reads
                    // `now + lease_ms` for the `now` sampled above; anchoring
                    // to the acknowledgement would put this fence a whole
                    // store round-trip past the expiry every other node is
                    // counting down — the overrun this deadline exists to
                    // prevent.
                    expires_at = issued_at + Duration::from_millis(lease_ms.max(0) as u64);
                }
                Ok(false) => {
                    // Retirement races the renewal it interrupted. A row that
                    // stopped being ours *because this worker settled it* is
                    // not a lost lease, and reporting it as one is how a
                    // signal that exists to be trusted becomes noise.
                    if stop.is_cancelled() {
                        break;
                    }
                    metrics.lease_event(LeaseEvent::Lost);
                    lost.cancel();
                    tracing::warn!(
                        queue,
                        row,
                        fence,
                        attempts,
                        "analysis lease lost: the row is no longer ours"
                    );
                    break;
                }
                Err(error) => {
                    if stop.is_cancelled() {
                        break;
                    }
                    metrics.lease_event(LeaseEvent::RenewFailed);
                    tracing::warn!(
                        queue, row, fence, attempts, %error,
                        "analysis lease renewal failed; retrying before the lease expires"
                    );
                }
            }
        }
    }
}

pub(crate) fn analysis_request_generation(
    file: &MediaFile,
    component: &str,
    pipeline_version: &str,
    video_identity: &str,
    force_rebuild: bool,
) -> String {
    if force_rebuild {
        uuid::Uuid::new_v4().to_string()
    } else {
        plurx_core::segplan::argv_fingerprint(&[
            "analysis-request".to_owned(),
            file.id.to_string(),
            file.size.to_string(),
            file.mtime.to_string(),
            component.to_owned(),
            pipeline_version.to_owned(),
            // Only fragment-index work attests a source, so only its
            // generations belong to an attestation regime. Stamping this
            // on `skip_markers` as well would re-request every semantic
            // pass and republish its annotations under a new generation
            // id, for a change that has nothing to do with them.
            if component == "fragment_index" {
                ANALYSIS_ATTESTATION_GENERATION.to_owned()
            } else {
                String::new()
            },
            // The identity is part of the generation, which is what lets
            // one file hold one request per identity instead of one
            // request that tombstones the rest.
            video_identity.to_owned(),
        ])
    }
}

/// Persist the exact copy recipe needed by a playback request before any
/// source read. The existing analysis worker owns attestation, indexing and
/// retries; playback keeps its usable rolling fallback while that work waits.
/// A viewer joins the resulting exact generation through a separate expiring
/// interest; ordinary discovery keeps its maintenance request class.
#[cfg(test)]
pub(crate) async fn enqueue_copy_preparation(
    store: &dyn Store,
    node_id: &str,
    file: &MediaFile,
    video: plurx_core::transcode::CopyVideoOptions,
) -> Result<AnalysisRequest, StoreError> {
    enqueue_copy_preparation_for_object(store, node_id, file, video, None).await
}

#[cfg(test)]
pub(crate) async fn enqueue_copy_preparation_for_object(
    store: &dyn Store,
    node_id: &str,
    file: &MediaFile,
    video: plurx_core::transcode::CopyVideoOptions,
    object_version: Option<&str>,
) -> Result<AnalysisRequest, StoreError> {
    enqueue_copy_preparation_for_object_with_viewer(
        store,
        node_id,
        file,
        video,
        object_version,
        None,
    )
    .await
}

#[derive(Clone, Debug)]
pub(crate) struct PlaybackViewerDemand {
    pub principal: plurx_core::playback_principal::PlaybackPrincipal,
    pub playback_id: String,
}

impl PlaybackViewerDemand {
    /// Shared source admission is not installed. Refuse explicitly rather
    /// than dropping typed viewer ownership from background or VOD work.
    pub(crate) fn require_local_authority(&self) -> Result<(), &'static str> {
        match &self.principal {
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id }
                if *user_id > 0 =>
            {
                Ok(())
            }
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { .. } => {
                Err("invalid local playback demand")
            }
            plurx_core::playback_principal::PlaybackPrincipal::Sharing { .. } => {
                Err("sharing playback demand requires typed source authority")
            }
        }
    }
}

pub(crate) async fn enqueue_copy_preparation_for_object_with_viewer(
    store: &dyn Store,
    node_id: &str,
    file: &MediaFile,
    video: plurx_core::transcode::CopyVideoOptions,
    object_version: Option<&str>,
    viewer: Option<&PlaybackViewerDemand>,
) -> Result<AnalysisRequest, StoreError> {
    if let Some(viewer) = viewer {
        viewer
            .require_local_authority()
            .map_err(|error| StoreError::Task(error.to_owned()))?;
    }
    let pipeline_version = crate::ffmpeg::fragment_index_engine_digest().await;
    let video_identity = crate::fragindex::identity_for(file, video).argv_fingerprint;
    let base = analysis_request_generation(
        file,
        "fragment_index",
        &pipeline_version,
        &video_identity,
        false,
    );
    let requested_generation = match object_version {
        Some(version) => plurx_core::segplan::argv_fingerprint(&[base, version.to_owned()]),
        None => base,
    };
    let now = clock_ms();
    let request = store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            component: "fragment_index".to_owned(),
            requested_generation,
            pipeline_version,
            video_identity,
            // A viewer waiting on this preparation makes it foreground work:
            // the row says so, and the queue's playback headroom admits it
            // even when background discovery has filled the active ceiling.
            priority: if viewer.is_some() {
                "foreground"
            } else {
                "normal"
            }
            .to_owned(),
            trigger: if viewer.is_some() {
                "playback"
            } else {
                "background"
            }
            .to_owned(),
            force_rebuild: false,
            target_node_id: node_id.to_owned(),
            not_before_ms: now,
            created_at_ms: now,
        })
        .await?;
    if let Some(viewer) = viewer {
        store
            .join_analysis_viewer(plurx_core::store::AnalysisViewerInterest {
                analysis_request_id: request.request_id.clone(),
                requested_generation: request.requested_generation.clone(),
                pipeline_version: request.pipeline_version.clone(),
                video_identity: request.video_identity.clone(),
                target_node_id: request.target_node_id.clone(),
                principal: viewer.principal.clone(),
                playback_id: viewer.playback_id.clone(),
                now_ms: clock_ms(),
            })
            .await?;
    }
    let _ = fragment_analysis_wake().send(());
    Ok(request)
}

pub(crate) fn clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(i64::MAX)
}

pub(crate) async fn subtitle_source_pipeline_version() -> String {
    format!(
        "subtitle-source:{}:{}",
        crate::subtitle_source::MANIFEST_VERSION,
        crate::ffmpeg::fragment_index_engine_digest().await
    )
}

fn subtitle_source_argv(
    input: &str,
    plan: &crate::subtitle_ride_along::RideAlongPlan,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-copyts".to_owned(),
        "-threads".to_owned(),
        "1".to_owned(),
        "-i".to_owned(),
        input.to_owned(),
    ];
    args.extend(plan.args());
    args
}

fn subtitle_source_covered(
    tracks: &[crate::subtitle_ride_along::ProbedTrack],
    rows: &[plurx_core::store::SubtitleSourcePublication],
) -> bool {
    tracks
        .iter()
        .all(|track| crate::subtitle_source::extracted_ordinal_covered(rows, track.ordinal))
}

/// The subtitle worker has no index pipe: the tee's null slave is its sole
/// stdout destination. A job-owned child and a separate stderr reader ensure
/// even a noisy decoder cannot deadlock a lease holder.
async fn run_subtitle_source_pass(
    plan: crate::subtitle_ride_along::RideAlongPlan,
    source: &std::fs::File,
    runtime_cache: &Path,
    lost: &CancellationToken,
    preempt_when_busy: Option<&TranscodeManager>,
    progress: &(dyn Fn() + Sync),
) -> Result<crate::subtitle_ride_along::PendingRideAlong, AnalysisResolutionError> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    if preempt_when_busy.is_some_and(|manager| !manager.pretranscode_worker_idle()) {
        return Err(AnalysisResolutionError::Retry {
            code: "foreground_preempted",
            charge_attempt: false,
        });
    }

    #[cfg(unix)]
    let input = "/dev/fd/3".to_owned();
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(source)
        .map_err(|_| AnalysisResolutionError::Terminal("source_unavailable"))?
        .to_string_lossy()
        .into_owned();
    let args = subtitle_source_argv(&input, &plan);
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let source_fd = source.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                let duplicate = libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(duplicate, 3) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    crate::ffmpeg::verify_windows_source_path(source, Path::new(&input))
        .map_err(|_| AnalysisResolutionError::Terminal("source_unavailable"))?;
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("subtitle source extraction"),
    )
    .map_err(|_| AnalysisResolutionError::Retry {
        code: "source_process_failed",
        charge_attempt: true,
    })?;
    let mut stderr = child.stderr.take().ok_or(AnalysisResolutionError::Retry {
        code: "source_process_failed",
        charge_attempt: true,
    })?;
    let mut scan = plan.stderr_scan();
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0_u8; 8192];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => scan.feed(&chunk[..read]),
                Err(_) => return None,
            }
        }
        scan.finish();
        Some(scan)
    });
    let deadline = tokio::time::sleep(Duration::from_secs(30 * 60));
    tokio::pin!(deadline);
    let status = loop {
        tokio::select! {
            () = lost.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stderr_task.abort();
                return Err(AnalysisResolutionError::ClaimLost);
            }
            () = &mut deadline => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stderr_task.abort();
                return Err(AnalysisResolutionError::Retry {
                    code: "source_pass_timeout",
                    charge_attempt: true,
                });
            }
            status = child.wait() => break status.map_err(|_| AnalysisResolutionError::Retry {
                code: "source_process_failed",
                charge_attempt: true,
            })?,
            () = tokio::time::sleep(Duration::from_millis(500)) => {
                progress();
                if preempt_when_busy.is_some_and(|manager| !manager.pretranscode_worker_idle()) {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    stderr_task.abort();
                    return Err(AnalysisResolutionError::Retry {
                        code: "foreground_preempted",
                        charge_attempt: false,
                    });
                }
            }
        }
    };
    progress();
    let scan = stderr_task.await.ok().flatten();
    if !status.success() {
        return Err(AnalysisResolutionError::Retry {
            code: "source_process_failed",
            charge_attempt: true,
        });
    }
    Ok(crate::subtitle_ride_along::PendingRideAlong::new(
        plan, scan,
    ))
}

pub(crate) async fn wait_analysis_deadline(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Read the probe once and ask [`crate::fragindex::video_identities`] which
/// copy pipelines this file has to be indexed for.
async fn fragment_index_video_identities(
    store: &dyn Store,
    file: &MediaFile,
    have_dovi: bool,
    convert: bool,
) -> Result<Vec<plurx_core::transcode::CopyVideoOptions>, StoreError> {
    let probe_json = store.get_file_probe_json(file.id).await?;
    Ok(crate::fragindex::video_identities(
        file,
        probe_json.as_deref(),
        have_dovi,
        convert,
    ))
}

/// The one pipeline a caller that can only carry one should work on.
///
/// The durable analysis request (`POST /files/{id}/analysis`) is admin-issued,
/// carries no client capabilities, and its store row binds one request to one
/// queued job, so it cannot fan out. It answers the identity the file is
/// **missing**, falling back to the first — the stripped pipeline — when every
/// identity is already present, which is what a forced rebuild wants.
///
/// Answering the first identity unconditionally, which is what this did when
/// there was only ever one, is the version that reads correctly and is
/// useless: the admin control is the only way to build an index on a node
/// where the background pass is turned off (`vod_index_mins = 0`), and on a
/// Dolby Vision title it would re-derive the pipeline that already exists.
/// The file would sit at `partial` through every click of a button that ran a
/// whole-file hash to change nothing.
/// Every copy-video identity this node would build for a file, paired with the
/// argv fingerprint that names it.
pub(crate) async fn fragment_index_video_identity_options(
    store: &dyn Store,
    file: &MediaFile,
    have_dovi: bool,
    convert: bool,
) -> Result<Vec<(plurx_core::transcode::CopyVideoOptions, String)>, StoreError> {
    let probe_json = store.get_file_probe_json(file.id).await?;
    Ok(
        crate::fragindex::video_identities(file, probe_json.as_deref(), have_dovi, convert)
            .into_iter()
            .map(|video| {
                let fingerprint = crate::fragindex::identity_for(file, video).argv_fingerprint;
                (video, fingerprint)
            })
            .collect(),
    )
}

/// Which identity a request is asking for.
///
/// A request that names one gets exactly that: three requests with distinct
/// identities must resolve to three different jobs, or two of them settle
/// `ready` against work they never did. A request that names none is either
/// pre-column or `skip_markers`, and keeps the old rule — the first identity
/// still lacking an index.
async fn fragment_index_requested_video_options(
    store: &dyn Store,
    file: &MediaFile,
    have_dovi: bool,
    convert: bool,
    requested_identity: &str,
) -> Result<plurx_core::transcode::CopyVideoOptions, StoreError> {
    let identities = fragment_index_video_identity_options(store, file, have_dovi, convert).await?;
    if !requested_identity.is_empty() {
        if let Some((video, _)) = identities
            .iter()
            .find(|(_, fingerprint)| fingerprint == requested_identity)
        {
            return Ok(*video);
        }
        // The request names an identity this node no longer emits — a
        // pipeline change, or a file whose probe facts moved. Superseded, not
        // "build something else and call it that".
        return Err(StoreError::Task(
            "requested copy-video identity is no longer produced for this source".to_owned(),
        ));
    }
    for (video, fingerprint) in &identities {
        if store
            .fragment_index(file.id, &crate::fragindex::identity_for(file, *video))
            .await?
            .is_none()
        {
            let _ = fingerprint;
            return Ok(*video);
        }
    }
    Ok(identities
        .first()
        .map(|(video, _)| *video)
        .unwrap_or_else(|| plurx_core::transcode::CopyVideoOptions::new(have_dovi, false)))
}

impl JobManager {
    /// Revalidate one bounded node-local page every interval. This deliberately
    /// does not take cluster job authority: both SQLite and hiqlite route the
    /// index rows through this process's local database, never through Raft.
    pub(crate) async fn fragment_index_validation_loop(
        self: Arc<Self>,
        shutdown: CancellationToken,
    ) {
        let mut ticker = tokio::time::interval(FRAGMENT_INDEX_VALIDATION_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => {}
            }
            match self
                .store
                .validate_fragment_index_page(FRAGMENT_INDEX_VALIDATION_PAGE)
                .await
            {
                Ok(report) => {
                    FRAGMENT_INDEX_VALIDATED.fetch_add(report.validated, Ordering::Relaxed);
                    FRAGMENT_INDEX_REFUSED.fetch_add(report.refused, Ordering::Relaxed);
                    FRAGMENT_INDEX_GONE.fetch_add(report.gone, Ordering::Relaxed);
                    if report.validated != 0 || report.refused != 0 || report.gone != 0 {
                        tracing::info!(
                            validated = report.validated,
                            refused = report.refused,
                            gone = report.gone,
                            remaining = report.remaining,
                            "validated a node-local fragment-index page"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "node-local fragment-index validation failed");
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn new(store: Arc<dyn Store>, artwork_dir: PathBuf) -> Self {
        Self::new_with_scan_prune_percent(
            store,
            artwork_dir,
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "test-node".to_owned(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        )
    }

    #[cfg(test)]
    pub(crate) fn test_new(store: Arc<dyn Store>, artwork_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self::new(store, artwork_dir))
    }

    #[cfg(test)]
    pub(crate) fn test_new_for_node(
        store: Arc<dyn Store>,
        artwork_dir: PathBuf,
        node_id: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self::new_with_scan_prune_percent(
            store,
            artwork_dir,
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            node_id.into(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        ))
    }

    fn new_with_scan_prune_percent(
        store: Arc<dyn Store>,
        artwork_dir: PathBuf,
        scan_prune_percent: u8,
        node_id: String,
        job_authority: Arc<dyn ClusterJobAuthority>,
    ) -> Self {
        let coordinator = StoreCoordinator::new(Arc::clone(&store), node_id)
            .expect("configured node id is a valid lease owner");
        JobManager {
            store,
            identity_repairs: Default::default(),
            coordinator,
            job_authority,
            membership: None,
            artwork_dir,
            scan_prune_percent,
            #[cfg(test)]
            tmdb_base: None,
            statuses: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            library_wake: tokio::sync::Notify::new(),
            library_readiness: Default::default(),
            subtitle_admissions: std::sync::Mutex::new(crate::admission::Admissions::new()),
            metrics: Arc::new(IntegrationMetrics::default()),
            producing: std::sync::atomic::AtomicBool::new(false),
            indexing: std::sync::atomic::AtomicBool::new(false),
            cluster_index_working: std::sync::atomic::AtomicBool::new(false),
            cluster_analysis_working: std::sync::atomic::AtomicBool::new(false),
            background_upkeep_running: std::sync::atomic::AtomicBool::new(false),
            dv_disk_working: std::sync::atomic::AtomicBool::new(false),
            dv_disk_capabilities: crate::dv_disk::DvDiskCapabilities::default(),
            last_analysis_prune_ms: AtomicI64::new(0),
            analysis_progress: std::sync::Mutex::new(HashMap::new()),
            analysis_progress_epoch: AtomicU64::new(0),
            analysis_metrics: Arc::new(AnalysisRuntimeMetrics::default()),
            #[cfg(test)]
            finished_analysis_rows: std::sync::Mutex::new(Vec::new()),
            now_producing: Mutex::new(None),
            stop_producing: std::sync::atomic::AtomicBool::new(false),
            pretranscode_refusals: Mutex::new(HashMap::new()),
            fragment_index_refusals: Mutex::new(HashMap::new()),
            shared_pass_marks: Mutex::new(HashMap::new()),
            last_pretranscode_cache_sweep_ms: AtomicI64::new(0),
            fragment_index_sweep_cursor: Mutex::new(None),
            subtitle_source_sweep_cursor: Mutex::new(None),
            subtitle_sidecar_sweep_cursor: Mutex::new(None),
            fresh_index_files: std::sync::Mutex::new(std::collections::VecDeque::new()),
            local_index_pass: std::sync::atomic::AtomicBool::new(false),
            fresh_noted_at: std::sync::Mutex::new(std::collections::HashMap::new()),
            subtitle_backfill_enqueued_process: AtomicU64::new(0),
            backfilling_genres: std::sync::atomic::AtomicBool::new(false),
            retrying_artwork: std::sync::atomic::AtomicBool::new(false),
            book_cover_workers: metadata::book::CoverMaterializationWorkers::default(),
            last_genre_backfill: Mutex::new(None),
        }
    }

    fn with_membership(
        mut self,
        membership: plurx_core::cluster::membership::MembershipManager,
    ) -> Self {
        self.membership = Some(membership);
        self
    }

    fn with_dv_disk_capabilities(
        mut self,
        capabilities: crate::dv_disk::DvDiskCapabilities,
    ) -> Self {
        self.dv_disk_capabilities = capabilities;
        self
    }

    pub(crate) async fn acquire_job(
        &self,
        resource: String,
    ) -> Result<Option<ActiveJobLease>, StoreError> {
        acquire_cluster_job(&self.coordinator, self.job_authority.as_ref(), resource).await
    }

    /// [`Self::acquire_job`] with an explicit TTL and heartbeat: the same gate
    /// and the same lease, for a job whose failover test shortens them.
    pub(crate) async fn acquire_job_with_policy(
        &self,
        resource: String,
        ttl: std::time::Duration,
        heartbeat: std::time::Duration,
    ) -> Result<Option<ActiveJobLease>, StoreError> {
        crate::job_lease::acquire_cluster_job_with_policy(
            &self.coordinator,
            self.job_authority.as_ref(),
            resource,
            ttl,
            heartbeat,
        )
        .await
    }

    pub(crate) async fn apply_identity_repair(
        &self,
        snapshot: &plurx_core::store::IdentityRepairSnapshot,
        plan: &plurx_core::store::IdentityRepairPlan,
    ) -> Result<Option<plurx_core::store::IdentityRepairOutcome>, StoreError> {
        let Some(lease) = self
            .acquire_job(format!("scan:library:{}", plan.library_id))
            .await?
        else {
            return Ok(None);
        };
        let outcome = lease
            .publisher(self.store.as_ref())
            .apply_identity_repair(snapshot, plan)
            .await;
        if let Err(error) = lease.release().await {
            tracing::warn!(error = %error, "identity repair lease cleanup was ambiguous");
        }
        outcome.map(Some)
    }

    /// Whether this node may run cluster-wide scheduled work right now.
    pub(crate) async fn may_run_cluster_jobs(&self) -> bool {
        self.job_authority.may_run_cluster_jobs().await
    }

    pub fn dv_disk_capabilities(&self) -> &crate::dv_disk::DvDiskCapabilities {
        &self.dv_disk_capabilities
    }

    fn require_dv_disk_tools(&self) -> Result<(), StoreError> {
        if self.dv_disk_capabilities.available {
            return Ok(());
        }
        Err(StoreError::Task(format!(
            "{} unavailable: {}",
            keys::LIBRARY_DV_DISK_CONVERT,
            self.dv_disk_capabilities
                .unavailable_reason()
                .unwrap_or("dovi_tool or mkvmerge is unavailable")
        )))
    }

    pub async fn queue_dv_file(
        &self,
        file_id: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError> {
        self.require_dv_disk_tools()?;
        self.store.queue_dv_conversion(file_id, clock_ms()).await
    }

    pub async fn queue_dv_library(
        &self,
        library_id: i64,
        retry_failed: bool,
    ) -> Result<DvConversionQueueBatch, StoreError> {
        self.require_dv_disk_tools()?;
        if self.store.get_library(library_id).await?.is_none() {
            return Err(StoreError::Task("library does not exist".to_owned()));
        }
        self.store
            .queue_library_dv_conversion_batch(
                library_id,
                clock_ms(),
                retry_failed,
                DV_CONVERSION_QUEUE_BATCH_MAX,
                true,
            )
            .await
    }

    pub async fn dv_disk_modes(&self) -> Result<BTreeMap<i64, DvDiskMode>, StoreError> {
        let raw = self
            .store
            .get_setting(keys::LIBRARY_DV_DISK_CONVERT)
            .await?;
        Ok(parse_dv_disk_modes(raw.as_deref()))
    }

    pub async fn set_dv_disk_mode(
        &self,
        library_id: i64,
        mode: DvDiskMode,
    ) -> Result<(), StoreError> {
        if self.store.get_library(library_id).await?.is_none() {
            return Err(StoreError::Task("library does not exist".to_owned()));
        }
        if mode != DvDiskMode::Off {
            self.require_dv_disk_tools()?;
        }
        let stored_mode = match mode {
            DvDiskMode::Off => DvConversionMode::Off,
            DvDiskMode::Manual => DvConversionMode::Manual,
            DvDiskMode::Auto => DvConversionMode::Auto,
        };
        if self
            .store
            .set_library_dv_conversion_mode(library_id, stored_mode)
            .await?
        {
            Ok(())
        } else {
            Err(StoreError::Task("library does not exist".to_owned()))
        }
    }

    pub async fn dv_disk_keep_original(&self) -> bool {
        parse_dv_disk_keep_original(
            self.store
                .get_setting(keys::LIBRARY_DV_DISK_KEEP_ORIGINAL)
                .await
                .ok()
                .flatten()
                .as_deref(),
        )
    }

    pub async fn dv_disk_parallel(&self) -> usize {
        parse_dv_disk_parallel(
            self.store
                .get_setting(keys::LIBRARY_DV_DISK_CONVERT_PARALLEL)
                .await
                .ok()
                .flatten()
                .as_deref(),
        )
    }

    /// One authority read for the three admin-facing conversion settings.
    pub async fn dv_disk_settings_snapshot(
        &self,
    ) -> Result<(BTreeMap<i64, DvDiskMode>, bool, usize), StoreError> {
        let settings = self.store.settings_snapshot().await?;
        Ok((
            parse_dv_disk_modes(
                settings
                    .get(keys::LIBRARY_DV_DISK_CONVERT)
                    .map(String::as_str),
            ),
            parse_dv_disk_keep_original(
                settings
                    .get(keys::LIBRARY_DV_DISK_KEEP_ORIGINAL)
                    .map(String::as_str),
            ),
            parse_dv_disk_parallel(
                settings
                    .get(keys::LIBRARY_DV_DISK_CONVERT_PARALLEL)
                    .map(String::as_str),
            ),
        ))
    }

    async fn enqueue_automatic_dv_conversions(&self) -> Result<DvConversionQueueBatch, StoreError> {
        let Some(lease) = self
            .acquire_job("media:dv-disk-convert:auto-discovery".to_owned())
            .await?
        else {
            return Ok(DvConversionQueueBatch::default());
        };
        let loss = lease.loss_token();
        let result = async {
            let cursor_key = self.local_job_key(keys::JOB_DV_DISK_AUTO_LIBRARY_CURSOR);
            let cursor = self
                .store
                .get_setting(&cursor_key)
                .await?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            let modes = self.dv_disk_modes().await?;
            let auto = modes
                .into_iter()
                .filter_map(|(library_id, mode)| (mode == DvDiskMode::Auto).then_some(library_id))
                .collect::<Vec<_>>();
            let selected = auto
                .iter()
                .copied()
                .find(|library_id| *library_id > cursor)
                .or_else(|| auto.first().copied());
            let batch = if let Some(library_id) = selected {
                if loss.is_cancelled() {
                    DvConversionQueueBatch::default()
                } else {
                    let batch = self
                        .store
                        .queue_library_dv_conversion_batch(
                            library_id,
                            clock_ms(),
                            false,
                            DV_CONVERSION_QUEUE_BATCH_MAX,
                            false,
                        )
                        .await?;
                    if !loss.is_cancelled() {
                        // Best-effort cursor: a failed write repeats bounded
                        // discovery work; it does not skip a library.
                        crate::store_result::observe(
                            crate::store_result::Operation::ResetDvQueueCursorAfterLoss,
                            crate::store_result::Discard::BestEffort,
                            self.store
                                .put_setting(&cursor_key, &library_id.to_string())
                                .await,
                        );
                    }
                    batch
                }
            } else {
                if cursor != 0 {
                    // Best-effort cursor reset: the next tick safely repeats
                    // the empty inventory pass if persistence is unavailable.
                    crate::store_result::observe(
                        crate::store_result::Operation::ResetDvQueueCursorAfterEmptyPage,
                        crate::store_result::Discard::BestEffort,
                        self.store.put_setting(&cursor_key, "0").await,
                    );
                }
                DvConversionQueueBatch::default()
            };
            Ok::<_, StoreError>(batch)
        }
        .await;
        let _ = lease.release().await;
        result
    }

    async fn work_dv_disk_queue(self: Arc<Self>) {
        const CANDIDATES_PER_TICK: i64 = 32;

        if self.dv_disk_working.swap(true, Ordering::Relaxed) {
            return;
        }
        let _guard = DvDiskWorkingGuard(Arc::clone(&self));
        self.reconcile_one_committed_dv_cleanup().await;
        self.reconcile_one_dv_recovery_guard().await;
        if !self.dv_disk_capabilities.available {
            return;
        }
        match self.enqueue_automatic_dv_conversions().await {
            Ok(batch) if batch.queued > 0 => {
                tracing::info!(
                    queued = batch.queued,
                    saturated = batch.saturated,
                    "queued automatic Dolby Vision conversions"
                )
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "discovering automatic Dolby Vision conversions");
                return;
            }
        }

        let cursor_key = self.local_job_key(keys::JOB_DV_DISK_CONVERT_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let candidates = match self
            .store
            .dv_conversion_candidates(cursor, CANDIDATES_PER_TICK)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "listing Dolby Vision conversion candidates");
                return;
            }
        };
        if candidates.is_empty() {
            if cursor != 0 {
                // Best-effort cursor reset: repeating the exhausted scan is
                // bounded and cannot omit conversion candidates.
                crate::store_result::observe(
                    crate::store_result::Operation::ResetDvQueueCursorAfterExhaustion,
                    crate::store_result::Discard::BestEffort,
                    self.store.put_setting(&cursor_key, "0").await,
                );
            }
            return;
        }

        let parallel = self.dv_disk_parallel().await;
        let keep_original = self.dv_disk_keep_original().await;
        let mut candidates = candidates.into_iter();
        let mut last_examined = cursor;
        let mut workers = tokio::task::JoinSet::new();
        for slot in 0..parallel {
            let slot_lease = match self
                .acquire_job(format!("media:dv-disk-convert:{slot}"))
                .await
            {
                Ok(Some(lease)) => lease,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(slot, %error, "acquiring Dolby Vision conversion slot");
                    continue;
                }
            };
            let mut selected = None;
            for candidate in candidates.by_ref() {
                last_examined = candidate.file_id;
                match self
                    .acquire_job(format!("media:dv-disk-convert:file:{}", candidate.file_id))
                    .await
                {
                    Ok(Some(file_lease)) => {
                        selected = Some((candidate, file_lease));
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(
                        file_id = candidate.file_id,
                        %error,
                        "acquiring Dolby Vision file lease"
                    ),
                }
            }
            let Some((candidate, file_lease)) = selected else {
                let _ = slot_lease.release().await;
                break;
            };
            let manager = Arc::clone(&self);
            workers.spawn(async move {
                // The file lease protects this identity; the slot lease
                // protects the cluster-wide I/O budget. Losing either proof
                // must stop the same tool process before a successor can use
                // the slot.
                let file_loss = file_lease.loss_token();
                let slot_loss = slot_lease.loss_token();
                let conversion_loss = CancellationToken::new();
                let cancel = conversion_loss.clone();
                let loss_watcher = tokio::spawn(async move {
                    tokio::select! {
                        () = file_loss.cancelled() => {}
                        () = slot_loss.cancelled() => {}
                    }
                    cancel.cancel();
                });
                manager
                    .process_dv_candidate(
                        candidate.file_id,
                        keep_original,
                        &file_lease,
                        &conversion_loss,
                    )
                    .await;
                loss_watcher.abort();
                if let Err(error) = file_lease.release().await {
                    tracing::warn!(%error, "releasing Dolby Vision file lease");
                }
                if let Err(error) = slot_lease.release().await {
                    tracing::warn!(%error, "releasing Dolby Vision conversion slot");
                }
            });
        }
        if last_examined != cursor {
            // Best-effort cursor advance: failure repeats examined candidates
            // instead of skipping work, while job claims prevent duplication.
            crate::store_result::observe(
                crate::store_result::Operation::AdvanceDvQueueCursor,
                crate::store_result::Discard::BestEffort,
                self.store
                    .put_setting(&cursor_key, &last_examined.to_string())
                    .await,
            );
        }
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "Dolby Vision conversion worker panicked");
            }
        }
    }

    /// Revisit one guardless committed conversion per worker tick. Publication
    /// prunes every full-size intermediate before the ledger commit, so this
    /// bounded pass only has to converge an owned marker directory left by a
    /// process crash during post-commit cleanup. The same file and slot leases
    /// as conversion work fence the filesystem mutation and its I/O budget.
    async fn reconcile_one_committed_dv_cleanup(&self) {
        const CURSOR: &str = "jobs.dv_disk_committed_cleanup_cursor";

        let cursor_key = self.local_job_key(CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut candidate = match self.store.dv_committed_cleanup_candidate(cursor).await {
            Ok(candidate) => candidate,
            Err(error) => {
                tracing::warn!(%error, "selecting committed Dolby Vision cleanup candidate");
                return;
            }
        };
        if candidate.is_none() && cursor != 0 {
            candidate = match self.store.dv_committed_cleanup_candidate(0).await {
                Ok(candidate) => candidate,
                Err(error) => {
                    tracing::warn!(%error, "wrapping committed Dolby Vision cleanup cursor");
                    return;
                }
            };
        }
        let Some(file_id) = candidate else {
            if cursor != 0 {
                // Best-effort recovery cursor reset: a failed reset only
                // repeats the bounded recovery inventory on the next tick.
                crate::store_result::observe(
                    crate::store_result::Operation::ResetDvRecoveryCursor,
                    crate::store_result::Discard::BestEffort,
                    self.store.put_setting(&cursor_key, "0").await,
                );
            }
            return;
        };

        let mut slot_lease = None;
        for slot in 0..self.dv_disk_parallel().await {
            match self
                .acquire_job(format!("media:dv-disk-convert:{slot}"))
                .await
            {
                Ok(Some(lease)) => {
                    slot_lease = Some(lease);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(slot, %error, "acquiring committed Dolby Vision cleanup slot")
                }
            }
        }
        let Some(slot_lease) = slot_lease else {
            return;
        };
        let file_lease = match self
            .acquire_job(format!("media:dv-disk-convert:file:{file_id}"))
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                let _ = slot_lease.release().await;
                return;
            }
            Err(error) => {
                tracing::warn!(file_id, %error, "acquiring committed Dolby Vision cleanup file lease");
                let _ = slot_lease.release().await;
                return;
            }
        };

        let file_loss = file_lease.loss_token();
        let slot_loss = slot_lease.loss_token();
        let cleanup_loss = CancellationToken::new();
        let cancel = cleanup_loss.clone();
        let loss_watcher = tokio::spawn(async move {
            tokio::select! {
                () = file_loss.cancelled() => {}
                () = slot_loss.cancelled() => {}
            }
            cancel.cancel();
        });
        match self.store.get_file(file_id).await {
            Ok(Some(file)) => {
                crate::dv_disk::cleanup_after_commit(
                    &file,
                    self.coordinator.node_id(),
                    &cleanup_loss,
                )
                .await;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(file_id, %error, "reading committed Dolby Vision cleanup file");
                loss_watcher.abort();
                let _ = file_lease.release().await;
                let _ = slot_lease.release().await;
                return;
            }
        }
        loss_watcher.abort();
        if let Err(error) = file_lease.release().await {
            tracing::warn!(file_id, %error, "releasing committed Dolby Vision cleanup file lease");
        }
        if let Err(error) = slot_lease.release().await {
            tracing::warn!(file_id, %error, "releasing committed Dolby Vision cleanup slot");
        }
        if let Err(error) = self
            .store
            .put_setting(&cursor_key, &file_id.to_string())
            .await
        {
            tracing::warn!(file_id, %error, "advancing committed Dolby Vision cleanup cursor");
        }
    }

    /// Converge exactly one non-cascading recovery guard whose file/conversion
    /// row is gone. This runs before tool capability checks: retiring a guard
    /// after confirmed scan deletion must not depend on dovi_tool or mkvmerge.
    async fn reconcile_one_dv_recovery_guard(&self) {
        const CURSOR: &str = "jobs.dv_disk_recovery_guard_cursor";

        let cursor_key = self.local_job_key(CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut candidates = match self.store.dv_recovery_guard_orphans(&cursor, 1).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "selecting orphaned Dolby Vision recovery guard");
                return;
            }
        };
        if candidates.is_empty() && !cursor.is_empty() {
            candidates = match self.store.dv_recovery_guard_orphans("", 1).await {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(%error, "wrapping Dolby Vision recovery-guard cursor");
                    return;
                }
            };
        }
        let Some(guard) = candidates.into_iter().next() else {
            if !cursor.is_empty() {
                // Best-effort guard cursor reset: failed persistence repeats
                // a bounded scan and cannot lose a recovery guard.
                crate::store_result::observe(
                    crate::store_result::Operation::ResetDvGuardCursor,
                    crate::store_result::Discard::BestEffort,
                    self.store.put_setting(&cursor_key, "").await,
                );
            }
            return;
        };
        let file_id = guard.file_id;
        let mut slot_lease = None;
        for slot in 0..self.dv_disk_parallel().await {
            match self
                .acquire_job(format!("media:dv-disk-convert:{slot}"))
                .await
            {
                Ok(Some(lease)) => {
                    slot_lease = Some(lease);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(slot, %error, "acquiring Dolby Vision recovery-guard cleanup slot")
                }
            }
        }
        let Some(slot_lease) = slot_lease else {
            return;
        };
        let file_lease = match self
            .acquire_job(format!("media:dv-disk-convert:file:{file_id}"))
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                let _ = slot_lease.release().await;
                return;
            }
            Err(error) => {
                tracing::warn!(file_id, %error, "acquiring Dolby Vision recovery-guard file lease");
                let _ = slot_lease.release().await;
                return;
            }
        };
        let file_loss = file_lease.loss_token();
        let slot_loss = slot_lease.loss_token();
        let cleanup_loss = CancellationToken::new();
        let cancel = cleanup_loss.clone();
        let loss_watcher = tokio::spawn(async move {
            tokio::select! {
                () = file_loss.cancelled() => {}
                () = slot_loss.cancelled() => {}
            }
            cancel.cancel();
        });
        let publisher = file_lease.publisher(self.store.as_ref());
        let cleanup = async {
            let mut state = guard.state;
            if matches!(
                state,
                DvRecoveryGuardState::Intent | DvRecoveryGuardState::Active
            ) {
                crate::dv_disk::remove_orphan_recovery_guard(
                    guard.file_id,
                    &guard.source_path,
                    &guard.recovery_path,
                    &guard.guard_id,
                    self.coordinator.node_id(),
                    &cleanup_loss,
                )
                .await
                .map_err(StoreError::Task)?;
                if cleanup_loss.is_cancelled() {
                    return Err(StoreError::Task(
                        "recovery-guard lease was lost before recording guard removal".to_owned(),
                    ));
                }
                if !publisher
                    .advance_dv_recovery_guard(
                        &guard.guard_id,
                        state,
                        DvRecoveryGuardState::GuardRemoved,
                        clock_ms(),
                    )
                    .await?
                {
                    return Err(StoreError::Task(
                        "recovery-guard ledger refused guard removal".to_owned(),
                    ));
                }
                state = DvRecoveryGuardState::GuardRemoved;
            }
            if state == DvRecoveryGuardState::GuardRemoved {
                crate::dv_disk::remove_orphan_recovery_scratch(
                    guard.file_id,
                    &guard.source_path,
                    &guard.guard_id,
                    &cleanup_loss,
                )
                .await
                .map_err(StoreError::Task)?;
                if cleanup_loss.is_cancelled() {
                    return Err(StoreError::Task(
                        "recovery-guard lease was lost before recording scratch removal".to_owned(),
                    ));
                }
                if !publisher
                    .advance_dv_recovery_guard(
                        &guard.guard_id,
                        DvRecoveryGuardState::GuardRemoved,
                        DvRecoveryGuardState::ScratchRemoved,
                        clock_ms(),
                    )
                    .await?
                {
                    return Err(StoreError::Task(
                        "recovery-guard ledger refused scratch removal".to_owned(),
                    ));
                }
                state = DvRecoveryGuardState::ScratchRemoved;
            }
            if state == DvRecoveryGuardState::ScratchRemoved && cleanup_loss.is_cancelled() {
                return Err(StoreError::Task(
                    "recovery-guard lease was lost before terminal row deletion".to_owned(),
                ));
            }
            if state == DvRecoveryGuardState::ScratchRemoved {
                // Keep the mounted source-parent capability alive through the
                // fenced Store mutation. An empty underlying mount point must
                // never make a missing scratch path authoritative.
                let _tombstone = crate::dv_disk::attest_orphan_recovery_tombstone(
                    guard.file_id,
                    &guard.source_path,
                    &guard.guard_id,
                )
                .await
                .map_err(StoreError::Task)?;
                if cleanup_loss.is_cancelled() {
                    return Err(StoreError::Task(
                        "recovery-guard lease was lost after tombstone attestation".to_owned(),
                    ));
                }
                if !publisher.delete_dv_recovery_guard(&guard.guard_id).await? {
                    return Err(StoreError::Task(
                        "recovery-guard ledger refused terminal row deletion".to_owned(),
                    ));
                }
            }
            Ok::<_, StoreError>(())
        }
        .await;
        if let Err(error) = cleanup {
            tracing::warn!(file_id, guard_id = %guard.guard_id, %error, "reconciling orphaned Dolby Vision recovery guard");
        }
        loss_watcher.abort();
        if let Err(error) = file_lease.release().await {
            tracing::warn!(file_id, %error, "releasing Dolby Vision recovery-guard file lease");
        }
        if let Err(error) = slot_lease.release().await {
            tracing::warn!(file_id, %error, "releasing Dolby Vision recovery-guard cleanup slot");
        }
        if let Err(error) = self.store.put_setting(&cursor_key, &guard.guard_id).await {
            tracing::warn!(file_id, %error, "advancing Dolby Vision recovery-guard cursor");
        }
    }

    async fn process_dv_candidate(
        &self,
        file_id: i64,
        keep_original: bool,
        lease: &ActiveJobLease,
        loss: &CancellationToken,
    ) {
        let publisher = lease.publisher(self.store.as_ref());
        let Some(file) = (match self.store.get_file(file_id).await {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(file_id, %error, "reading Dolby Vision source row");
                return;
            }
        }) else {
            return;
        };
        let Some(ledger) = (match self.store.dv_conversion(file_id).await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(file_id, %error, "reading Dolby Vision conversion ledger");
                return;
            }
        }) else {
            return;
        };

        let verified = if ledger.state == DvConversionState::Verified {
            let Some(bytes_after) = ledger.bytes_after else {
                tracing::warn!(
                    file_id,
                    "verified Dolby Vision row has no verified byte count"
                );
                return;
            };
            if let Err(error) = self.begin_dv_recovery_guard_intent(&publisher, &file).await {
                tracing::warn!(file_id, %error, "recording Dolby Vision recovery-guard intent");
                return;
            }
            let published_recovery_error = match crate::dv_disk::recover_published(
                &file,
                self.coordinator.node_id(),
                bytes_after,
                loss,
            )
            .await
            {
                Ok(crate::dv_disk::PublicationOutcome::Published(published)) => {
                    if let Err(error) = self
                        .finish_dv_publication(&publisher, &file, *published, loss)
                        .await
                    {
                        tracing::warn!(file_id, %error, "finishing recovered Dolby Vision publication");
                    }
                    return;
                }
                Ok(crate::dv_disk::PublicationOutcome::SafelyRolledBack { reason }) => {
                    if let Err(error) = self
                        .finish_dv_safe_rollback(&publisher, &file, loss, &reason)
                        .await
                    {
                        tracing::warn!(file_id, %error, "recording recovered Dolby Vision rollback");
                    }
                    return;
                }
                Err(error) => Some(error),
            };
            match crate::dv_disk::verify_existing(
                &file,
                self.coordinator.node_id(),
                loss,
                ledger.el_type.as_deref().and_then(|value| match value {
                    "mel" => Some("mel"),
                    "fel" => Some("fel"),
                    _ => None,
                }),
                bytes_after,
            )
            .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    // A verified row may own the only recoverable original in
                    // sibling scratch. A transient probe/read failure must not
                    // turn that recovery state into a retryable fresh queue
                    // entry, which could then strand the staged source. Leave
                    // the row verified so the next leased pass resumes the
                    // same publication artifacts.
                    tracing::warn!(
                        file_id,
                        %error,
                        published_recovery_error = published_recovery_error.as_deref(),
                        "verified Dolby Vision publication remains pending recovery"
                    );
                    return;
                }
            }
        } else {
            match self.store.dv_conversion_eligible(file_id).await {
                Ok(true) => {}
                Ok(false) => {
                    self.fail_dv_conversion(
                        &publisher,
                        &file,
                        loss,
                        "file is no longer an eligible Profile 7 Matroska source",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(file_id, %error, "rechecking Dolby Vision conversion eligibility");
                    return;
                }
            }
            match crate::fragment_index_cluster::inspect_source(&file).await {
                Ok(_) => {}
                Err(error) => {
                    self.fail_dv_conversion(&publisher, &file, loss, &error)
                        .await;
                    return;
                }
            };
            match publisher
                .mark_dv_conversion_running(file_id, file.size)
                .await
            {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(file_id, %error, "claiming Dolby Vision ledger row");
                    return;
                }
            }
            let tools = crate::dv_disk::DvDiskTools::from_environment();
            let verified = match crate::dv_disk::build_and_verify(
                &tools,
                &file,
                self.coordinator.node_id(),
                loss,
                keep_original,
            )
            .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    self.fail_dv_conversion(&publisher, &file, loss, &error)
                        .await;
                    return;
                }
            };
            if loss.is_cancelled() {
                return;
            }
            match publisher
                .mark_dv_conversion_verified(file_id, verified.el_type, verified.bytes_after)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.fail_dv_conversion(
                        &publisher,
                        &file,
                        loss,
                        "ledger refused the running-to-verified transition",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(file_id, %error, "recording verified Dolby Vision replacement");
                    return;
                }
            }
            if let Err(error) = self.begin_dv_recovery_guard_intent(&publisher, &file).await {
                tracing::warn!(file_id, %error, "recording Dolby Vision recovery-guard intent");
                return;
            }
            // The object version captured before the long pipeline is checked
            // again immediately before the first source rename below.
            let published = match crate::dv_disk::publish_verified(
                &file,
                self.coordinator.node_id(),
                loss,
                verified.bytes_after,
            )
            .await
            {
                Ok(crate::dv_disk::PublicationOutcome::Published(published)) => *published,
                Ok(crate::dv_disk::PublicationOutcome::SafelyRolledBack { reason }) => {
                    if let Err(error) = self
                        .finish_dv_safe_rollback(&publisher, &file, loss, &reason)
                        .await
                    {
                        tracing::warn!(file_id, %error, "recording Dolby Vision publication rollback");
                    }
                    return;
                }
                Err(error) => {
                    tracing::warn!(file_id, %error, "publishing verified Dolby Vision replacement");
                    return;
                }
            };
            if let Err(error) = self
                .finish_dv_publication(&publisher, &file, published, loss)
                .await
            {
                tracing::warn!(file_id, %error, "finishing Dolby Vision publication");
            }
            return;
        };

        if let Err(error) = self.begin_dv_recovery_guard_intent(&publisher, &file).await {
            tracing::warn!(file_id, %error, "recording Dolby Vision recovery-guard intent");
            return;
        }
        let published = match crate::dv_disk::publish_verified(
            &file,
            self.coordinator.node_id(),
            loss,
            verified.bytes_after,
        )
        .await
        {
            Ok(crate::dv_disk::PublicationOutcome::Published(published)) => *published,
            Ok(crate::dv_disk::PublicationOutcome::SafelyRolledBack { reason }) => {
                if let Err(error) = self
                    .finish_dv_safe_rollback(&publisher, &file, loss, &reason)
                    .await
                {
                    tracing::warn!(file_id, %error, "recording resumed Dolby Vision rollback");
                }
                return;
            }
            Err(error) => {
                tracing::warn!(file_id, %error, "resuming verified Dolby Vision publication");
                return;
            }
        };
        if published.bytes_after != verified.bytes_after {
            tracing::warn!(
                file_id,
                verified = verified.bytes_after,
                published = published.bytes_after,
                "published Dolby Vision size changed after verification"
            );
            return;
        }
        if let Err(error) = self
            .finish_dv_publication(&publisher, &file, published, loss)
            .await
        {
            tracing::warn!(file_id, %error, "finishing resumed Dolby Vision publication");
        }
    }

    async fn finish_dv_publication(
        &self,
        publisher: &PublicationStore<'_>,
        file: &MediaFile,
        published: crate::dv_disk::PublishedReplacement,
        loss: &CancellationToken,
    ) -> Result<(), StoreError> {
        let path = file.path.to_string_lossy();
        publisher
            .upsert_file(
                file.item_id,
                &path,
                published.size,
                published.mtime,
                &published.probe,
            )
            .await?;
        let original_path = crate::dv_disk::finalize_verified_original(
            file,
            self.coordinator.node_id(),
            published.bytes_after,
            loss,
        )
        .await
        .map_err(StoreError::Task)?;
        if original_path != published.original_path {
            return Err(StoreError::Task(
                "finalized original path disagrees with the verified publication".to_owned(),
            ));
        }
        let committed = match (
            published.recovery_guard_id.as_deref(),
            published.recovery_guard_path.as_deref(),
        ) {
            (Some(guard_id), Some(_)) if original_path.is_none() => {
                publisher
                    .mark_dv_conversion_committed_with_guard(
                        file.id,
                        guard_id,
                        published.bytes_after,
                        clock_ms(),
                    )
                    .await?
            }
            (None, None) if original_path.is_some() => {
                publisher
                    .mark_dv_conversion_committed(
                        file.id,
                        original_path.as_deref(),
                        published.bytes_after,
                        clock_ms(),
                    )
                    .await?
            }
            _ => {
                return Err(StoreError::Task(
                    "publication retention and recovery-guard facts are inconsistent".to_owned(),
                ));
            }
        };
        if !committed {
            return Err(StoreError::Task(
                "ledger refused the verified-to-committed transition".to_owned(),
            ));
        }
        crate::dv_disk::cleanup_after_commit(file, self.coordinator.node_id(), loss).await;
        tracing::info!(file_id = file.id, path = %path, "Dolby Vision conversion committed");
        Ok(())
    }

    async fn begin_dv_recovery_guard_intent(
        &self,
        publisher: &PublicationStore<'_>,
        file: &MediaFile,
    ) -> Result<(), StoreError> {
        let Some(intent) = crate::dv_disk::recovery_guard_intent(file)
            .await
            .map_err(StoreError::Task)?
        else {
            return Ok(());
        };
        if !publisher
            .begin_dv_recovery_guard(file.id, &intent.guard_id, &intent.recovery_path, clock_ms())
            .await?
        {
            return Err(StoreError::Task(
                "recovery-guard intent was not accepted for the verified conversion".to_owned(),
            ));
        }
        Ok(())
    }

    async fn finish_dv_safe_rollback(
        &self,
        publisher: &PublicationStore<'_>,
        file: &MediaFile,
        loss: &CancellationToken,
        reason: &str,
    ) -> Result<(), StoreError> {
        let guarded = publisher.dv_recovery_guard(file.id).await?.is_some();
        if !publisher
            .mark_dv_conversion_failed(file.id, reason, clock_ms())
            .await?
        {
            return Err(StoreError::Task(
                "ledger refused the safely-rolled-back transition".to_owned(),
            ));
        }
        if !guarded {
            crate::dv_disk::cleanup_after_failure(file, loss).await;
        }
        Ok(())
    }

    async fn fail_dv_conversion(
        &self,
        publisher: &PublicationStore<'_>,
        file: &MediaFile,
        loss: &tokio_util::sync::CancellationToken,
        error: &str,
    ) {
        if loss.is_cancelled() {
            return;
        }
        crate::dv_disk::cleanup_after_failure(file, loss).await;
        match publisher
            .mark_dv_conversion_failed(file.id, error, clock_ms())
            .await
        {
            Ok(true) => tracing::warn!(file_id = file.id, %error, "Dolby Vision conversion failed"),
            Ok(false) => {
                tracing::warn!(file_id = file.id, %error, "Dolby Vision failure was not recorded")
            }
            Err(store_error) => tracing::warn!(
                file_id = file.id,
                %error,
                %store_error,
                "recording Dolby Vision conversion failure"
            ),
        }
    }

    /// What the last genre-backfill pass did, if one has run since boot.
    pub async fn last_genre_backfill(&self) -> Option<GenreBackfillReport> {
        self.last_genre_backfill.lock().await.clone()
    }

    /// What the pre-transcode pass is working on, or `None` if none is.
    pub async fn producing_now(&self) -> Option<ProducingNow> {
        self.now_producing.lock().await.clone()
    }

    /// Persist an operator request before any source hashing begins. The
    /// request row is the acknowledgement boundary: once returned, a restart
    /// or leader change cannot forget the button press.
    pub async fn request_file_analysis(
        &self,
        file_id: i64,
        force_rebuild: bool,
        component: &str,
        trigger: &str,
    ) -> Result<(AnalysisRequest, bool), StoreError> {
        self.request_file_analysis_for_identity(file_id, force_rebuild, component, trigger, "")
            .await
    }

    /// Request one copy-video identity of a file, rather than whichever one
    /// happens to lack an index first.
    ///
    /// An empty `video_identity` is the old behaviour and stays the default:
    /// the resolver picks the next identity that needs building. Background
    /// discovery names one, so a file with three identities yields three
    /// requests instead of one that permanently tombstones the other two.
    pub async fn request_file_analysis_for_identity(
        &self,
        file_id: i64,
        force_rebuild: bool,
        component: &str,
        trigger: &str,
        video_identity: &str,
    ) -> Result<(AnalysisRequest, bool), StoreError> {
        if !matches!(
            component,
            "fragment_index" | "skip_markers" | "subtitle_source"
        ) {
            return Err(StoreError::Task(
                "unsupported analysis component".to_owned(),
            ));
        }
        if !matches!(trigger, "admin" | "background" | "playback") {
            return Err(StoreError::Task("unsupported analysis trigger".to_owned()));
        }
        if component == "subtitle_source" {
            if force_rebuild || !video_identity.is_empty() {
                return Err(StoreError::Task(
                    "subtitle_source has no forced or video-specific generation".to_owned(),
                ));
            }
            let file = self
                .store
                .get_file(file_id)
                .await?
                .ok_or_else(|| StoreError::Task("analysis file does not exist".to_owned()))?;
            let stamp = SubtitleSourceStamp {
                file_id,
                source_size: file.size,
                source_mtime: file.mtime,
                pipeline_version: subtitle_source_pipeline_version().await,
            };
            let request = self
                .store
                .enqueue_or_promote_subtitle_source(
                    &stamp,
                    if trigger == "playback" {
                        "foreground"
                    } else {
                        "normal"
                    },
                    clock_ms(),
                )
                .await?
                .ok_or_else(|| {
                    StoreError::Task("subtitle_source repair limit exhausted".to_owned())
                })?;
            return Ok((request, true));
        }
        let file = self
            .store
            .get_file(file_id)
            .await?
            .ok_or_else(|| StoreError::Task("analysis file does not exist".to_owned()))?;
        let now = clock_ms();
        let request_id = uuid::Uuid::new_v4().to_string();
        let pipeline_version = if component == "fragment_index" {
            crate::ffmpeg::fragment_index_engine_digest().await
        } else {
            crate::http::stream::CHAPTER_ANNOTATION_VERSION.to_owned()
        };
        let requested_generation = analysis_request_generation(
            &file,
            component,
            &pipeline_version,
            video_identity,
            force_rebuild,
        );
        let request = self
            .store
            .enqueue_analysis_request(&NewAnalysisRequest {
                request_id: request_id.clone(),
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                component: component.to_owned(),
                pipeline_version,
                video_identity: video_identity.to_owned(),
                requested_generation,
                priority: if force_rebuild { "forced" } else { "normal" }.to_owned(),
                trigger: trigger.to_owned(),
                force_rebuild,
                target_node_id: if component == "skip_markers" {
                    String::new()
                } else {
                    self.coordinator.node_id().to_owned()
                },
                not_before_ms: now,
                created_at_ms: now,
            })
            .await?;
        let joined = request.request_id != request_id;
        Ok((request, joined))
    }

    pub async fn analysis_queue_enabled(&self) -> bool {
        self.cluster_fragment_index_enabled().await
    }

    /// The queue switch is an operator choice. Developer readiness facts are
    /// advisory; none of them silently overrides an enabled switch.
    pub(crate) async fn subtitle_source_queue_enabled(&self) -> bool {
        self.cluster_fragment_index_enabled().await
            && setting_enabled(
                self.store
                    .get_setting(keys::SUBTITLE_CLUSTER_SOURCES)
                    .await
                    .unwrap_or(None),
            )
    }

    pub(crate) fn subtitle_backfill_enqueued_process(&self) -> u64 {
        self.subtitle_backfill_enqueued_process
            .load(Ordering::Relaxed)
    }

    pub(crate) fn start_probe_progress(
        self: &Arc<Self>,
        token: &plurx_core::store::background_jobs::JobToken,
        file_id: i64,
        total_bytes: u64,
    ) -> AnalysisProgressGuard {
        let guard = self.start_analysis_progress(
            (&token.job_id, ""),
            file_id,
            "media_probe",
            "probing",
            total_bytes,
            0,
        );
        guard.attach_attempt(token);
        guard
    }

    fn start_analysis_progress(
        self: &Arc<Self>,
        identity: (&str, &str),
        file_id: i64,
        component: &str,
        stage: &str,
        total_bytes: u64,
        total_media_ms: i64,
    ) -> AnalysisProgressGuard {
        let (job_id, target_node_id) = identity;
        let now = clock_ms();
        let registry_epoch = self
            .analysis_progress_epoch
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let progress_key = (job_id.to_owned(), target_node_id.to_owned());
        if progress.len() >= MAX_ANALYSIS_PROGRESS && !progress.contains_key(&progress_key) {
            if let Some(oldest) = progress
                .iter()
                .min_by_key(|(_, value)| (value.updated_at_ms, value.started_at_ms))
                .map(|(key, _)| key.clone())
            {
                if let Some(value) = progress.remove(&oldest) {
                    self.analysis_metrics.discard(&value);
                }
            }
        }
        let replaced = progress.insert(
            progress_key,
            AnalysisProgress {
                job_id: job_id.to_owned(),
                durable_job_id: None,
                durable_fence: None,
                file_id,
                item_id: 0,
                title: String::new(),
                component: component.to_owned(),
                stage: stage.to_owned(),
                bytes_read: 0,
                total_bytes,
                media_ms_examined: 0,
                total_media_ms: total_media_ms.max(0),
                fragments_indexed: 0,
                pgs_tracks: 0,
                text_tracks: 0,
                pgs_bytes_written: 0,
                shared_read_identities: 0,
                started_at_ms: now,
                updated_at_ms: now,
                elapsed_ms: 0,
                throughput_bps: 0,
                eta_ms: None,
                registry_epoch,
                rate_origin: None,
            },
        );
        if let Some(value) = replaced {
            self.analysis_metrics.discard(&value);
        }
        self.analysis_metrics.start(stage);
        AnalysisProgressGuard {
            jobs: Arc::clone(self),
            job_id: job_id.to_owned(),
            target_node_id: target_node_id.to_owned(),
            registry_epoch,
        }
    }

    fn update_analysis_progress(
        &self,
        job_id: &str,
        target_node_id: &str,
        stage: &str,
        bytes_read: u64,
        media_ms_examined: i64,
        fragments_indexed: usize,
    ) {
        let now = clock_ms();
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(value) = progress.get_mut(&(job_id.to_owned(), target_node_id.to_owned())) else {
            return;
        };
        self.analysis_metrics.transition(&value.stage, stage);
        value.stage = stage.to_owned();
        value.bytes_read = if value.total_bytes > 0 {
            bytes_read.min(value.total_bytes)
        } else {
            bytes_read
        };
        value.media_ms_examined = if value.total_media_ms > 0 {
            media_ms_examined.max(0).min(value.total_media_ms)
        } else {
            media_ms_examined.max(0)
        };
        value.fragments_indexed = fragments_indexed;
        value.updated_at_ms = now;
        // A later stage that counts from zero again starts a new origin.
        if value
            .rate_origin
            .is_none_or(|(origin_bytes, _)| value.bytes_read < origin_bytes)
        {
            value.rate_origin = Some((value.bytes_read, now));
        }
    }

    /// The progress callback both index paths hand a pass — the queue worker
    /// and the local background loop — so their rows read the same: bytes,
    /// media time and fragments, and for a pass that is also keeping PGS
    /// tracks, how many and what it has written, so that work is attributable
    /// from inside the product while it runs.
    fn index_pass_reporter(
        self: &Arc<Self>,
        job_id: &str,
        target_node_id: &str,
    ) -> crate::fragindex::SharedIndexProgress {
        let jobs = Arc::clone(self);
        let (job_id, target_node_id) = (job_id.to_owned(), target_node_id.to_owned());
        Arc::new(move |progress: &crate::fragindex::PassProgress| {
            jobs.update_analysis_progress(
                &job_id,
                &target_node_id,
                "fragment_index",
                progress.bytes_read,
                progress.media_ms,
                progress.fragments,
            );
            jobs.update_analysis_ride_along(
                &job_id,
                &target_node_id,
                progress.pgs_tracks,
                progress.pgs_bytes_written,
            );
        })
    }

    /// Record what a running index pass's PGS ride-along is doing on its
    /// progress row: how many tracks it keeps and the bytes written so far.
    fn update_analysis_ride_along(
        &self,
        job_id: &str,
        target_node_id: &str,
        pgs_tracks: usize,
        pgs_bytes_written: u64,
    ) {
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = progress.get_mut(&(job_id.to_owned(), target_node_id.to_owned())) {
            value.pgs_tracks = pgs_tracks;
            value.pgs_bytes_written = pgs_bytes_written;
        }
    }

    /// Mark a running row as one of `identities` built from one source read.
    fn set_analysis_shared_read(&self, job_id: &str, target_node_id: &str, identities: usize) {
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = progress.get_mut(&(job_id.to_owned(), target_node_id.to_owned())) {
            value.shared_read_identities = identities;
        }
    }

    fn update_analysis_subtitle_tracks(
        &self,
        job_id: &str,
        target_node_id: &str,
        pgs_tracks: usize,
        text_tracks: usize,
        bytes_written: u64,
    ) {
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = progress.get_mut(&(job_id.to_owned(), target_node_id.to_owned())) {
            value.pgs_tracks = pgs_tracks;
            value.text_tracks = text_tracks;
            value.pgs_bytes_written = bytes_written;
        }
    }

    fn set_analysis_progress_totals(
        &self,
        job_id: &str,
        target_node_id: &str,
        total_bytes: u64,
        total_media_ms: i64,
    ) {
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(value) = progress.get_mut(&(job_id.to_owned(), target_node_id.to_owned())) else {
            return;
        };
        value.total_bytes = total_bytes;
        value.total_media_ms = total_media_ms.max(0);
        value.updated_at_ms = clock_ms();
    }

    fn remove_analysis_progress(&self, job_id: &str, target_node_id: &str, registry_epoch: u64) {
        let mut progress = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if progress
            .get(&(job_id.to_owned(), target_node_id.to_owned()))
            .is_some_and(|value| value.registry_epoch == registry_epoch)
        {
            if let Some(value) = progress.remove(&(job_id.to_owned(), target_node_id.to_owned())) {
                self.analysis_metrics.finish(&value);
                #[cfg(test)]
                self.finished_analysis_rows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(value);
            }
        }
    }

    pub(crate) fn analysis_metrics_handle(&self) -> Arc<AnalysisRuntimeMetrics> {
        Arc::clone(&self.analysis_metrics)
    }

    pub fn analysis_progress_snapshot(&self) -> Vec<AnalysisProgress> {
        let now = clock_ms();
        let mut values = self
            .analysis_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for value in &mut values {
            value.elapsed_ms = now.saturating_sub(value.started_at_ms).max(0);
            (value.throughput_bps, value.eta_ms) = analysis_rate(
                value.bytes_read,
                value.total_bytes,
                value.rate_origin.unwrap_or((0, value.started_at_ms)),
                now,
            );
        }
        values.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then(left.job_id.cmp(&right.job_id))
        });
        values.truncate(MAX_ANALYSIS_PROGRESS);
        values
    }

    /// Publish (or clear) the title the pass is on. The pass owns this; it is
    /// crate-visible so the HTTP layer's tests can put the server in the state
    /// an admin actually complains about.
    pub(crate) async fn set_producing(&self, now: Option<ProducingNow>) {
        *self.now_producing.lock().await = now;
    }

    /// Ask a running pass to stop. It finishes the title it is on — killing an
    /// encoder mid-segment would throw away the part it has made, and the
    /// producer is built to resume from published boundaries, not from the
    /// middle of one. Returns whether there was anything to stop.
    pub fn stop_producing(&self) -> bool {
        if !self.producing.load(Ordering::Relaxed) {
            return false;
        }
        self.stop_producing.store(true, Ordering::Relaxed);
        true
    }

    /// Snapshot of all libraries' scan statuses, with live progress attached
    /// to any scan currently running.
    pub async fn all_statuses(&self) -> HashMap<i64, ScanStatus> {
        let mut map = self.statuses.lock().await.clone();
        let durable = self.durable_library_statuses().await;
        let live = self.live.lock().await;
        for (library, status) in durable {
            if !live.contains_key(&library) {
                map.insert(library, status);
            }
        }
        for (id, progress) in live.iter() {
            if let Some(status) = map.get_mut(id) {
                if status.running {
                    status.progress = Some(ProgressSnapshot::sample(progress));
                }
            }
        }
        map
    }

    /// Durably admit a library scan, joining compatible work already queued.
    /// `true` means accepted; an eligible worker may execute on another node.
    pub async fn trigger_scan(self: &Arc<Self>, library_id: i64) -> bool {
        self.trigger_scan_as(library_id, ScanTrigger::Manual).await
    }

    /// [`trigger_scan`], saying what asked for it.
    pub async fn trigger_scan_as(self: &Arc<Self>, library_id: i64, why: ScanTrigger) -> bool {
        self.trigger(library_id, false, why).await
    }

    /// Counters for `/metrics` and the system page.
    pub fn metrics(&self) -> &IntegrationMetrics {
        &self.metrics
    }

    /// Store-free counter handle for the Prometheus substate.
    pub(crate) fn metrics_handle(&self) -> Arc<IntegrationMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Like [`trigger_scan`], but forces a full metadata refresh — re-enriches
    /// even already-matched items (backfills season posters onto older shows).
    pub async fn trigger_refresh(self: &Arc<Self>, library_id: i64) -> bool {
        self.trigger(library_id, true, ScanTrigger::Manual).await
    }

    /// [`trigger_refresh`], saying what asked for it.
    pub async fn trigger_refresh_as(self: &Arc<Self>, library_id: i64, why: ScanTrigger) -> bool {
        self.trigger(library_id, true, why).await
    }

    async fn run_targeted(
        &self,
        requests: &[ScanRequest],
        publisher: &PublicationStore<'_>,
    ) -> Result<TargetedScan, TargetError> {
        library_work::check_active()?;
        let req = requests.first().ok_or_else(|| {
            TargetError::Store(StoreError::Task(
                "targeted scan group contained no waiters".to_owned(),
            ))
        })?;
        self.metrics.count_scan(ScanTrigger::Targeted);
        let library = self
            .store
            .get_library(req.library_id)
            .await
            .map_err(TargetError::Store)?
            .ok_or_else(|| TargetError::OutsideRoots {
                path: req.path.display().to_string(),
                roots: Vec::new(),
            })?;
        tracing::info!(
            target: "plurxd::integrate",
            library = req.library_id,
            path = %req.path.display(),
            correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
            source = req.source.as_deref().unwrap_or("-"),
            request = %req.id,
            waiters = requests.len(),
            "targeted scan requested"
        );
        let mut out = scan::scan_path_with_publication(publisher, &library, &req.path).await?;
        self.note_fresh_index_files(out.items.iter().map(|placed| placed.file_id).collect());
        for request in requests {
            library_work::check_active()?;
            for problem in self.apply_ids(request, &out.items, publisher).await {
                out.report.add_problem(problem);
            }
        }

        // The rows exist; without this they would have no artwork until
        // somebody pressed Scan. Bounded to what this request placed (and the
        // seasons/shows/folders above it) because Curator is holding the HTTP
        // connection open on this call — enriching the whole library here
        // would turn a per-episode import notification into a per-episode
        // full-library metadata pass.
        let placed: Vec<i64> = out.items.iter().map(|p| p.item_id).collect();
        let targets = self.enrich_targets(&placed).await;
        // A new episode often lands under a show that was enriched months
        // ago. The ordinary `force = false` queue quite correctly omits that
        // show, but episode/season enrichment is reached *through* the show,
        // so omitting it also strands every newly placed child without art.
        // Force only this bounded TV tree; `only` still prevents a targeted
        // notification from becoming a whole-library refresh.
        let refresh_existing_show = library.kind == LibraryKind::Shows && !library.anime;
        let repairs = if refresh_existing_show {
            // A first import still needs its newly created show/season cards.
            // On later imports, healthy ancestors are routes only. `placed`
            // names file-owning rows, so add only blank ancestors here.
            let mut repairs = placed.clone();
            let mut seen: HashSet<i64> = repairs.iter().copied().collect();
            for target in &targets {
                if seen.contains(target) {
                    continue;
                }
                match self.store.get_item(*target).await {
                    Ok(Some(item)) if needs_artwork_retry(&item) => {
                        seen.insert(*target);
                        repairs.push(*target);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => tracing::warn!(
                        item_id = *target,
                        "targeted scan ancestor disappeared before enrichment"
                    ),
                    Err(e) => tracing::warn!(
                        item_id = *target,
                        error = %e,
                        "targeted scan could not inspect ancestor artwork"
                    ),
                }
            }
            repairs
        } else {
            placed.clone()
        };
        library_work::check_active()?;
        let outcome = self
            .enrich(
                publisher,
                &library,
                refresh_existing_show,
                Some(&targets),
                Some(&repairs),
                None,
            )
            .await;
        outcome.report_provider_outage(&mut out.report);
        for request in requests {
            library_work::check_active()?;
            self.apply_book_hints(request, &out.items, publisher).await;
        }
        tracing::info!(
            target: "plurxd::integrate",
            library = req.library_id,
            items = targets.len(),
            matched = outcome.enrich.map(|r| r.matched).unwrap_or(0),
            correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
            "targeted scan enriched what it placed"
        );
        library_work::check_active()?;
        Ok(out)
    }

    /// Which item ids a targeted scan should enrich, given what it placed.
    ///
    /// Not the placed ids alone: `scan_path` returns the rows that own the
    /// *files*, which for a show are episodes — and episodes are not what
    /// `items_needing_metadata` selects, so filtering on them would enrich
    /// precisely nothing. The identity of an episode lives on its show, and a
    /// home video's poster is what its folder inherits, so every ancestor
    /// comes along too.
    async fn enrich_targets(&self, item_ids: &[i64]) -> Vec<i64> {
        let mut targets: Vec<i64> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        for id in item_ids {
            let mut current = *id;
            // Depth guard, not a shape assumption: library → show → season →
            // episode is three levels, home folders can nest deeper, and a
            // cycle in parent_id would otherwise hang the request.
            for _ in 0..16 {
                // Deduplicate the output, not the walk. A prior start may
                // have reached this row with too little depth budget left to
                // reach all of its ancestors; stopping here would make the
                // missing ancestor depend on filesystem traversal order.
                if seen.insert(current) {
                    targets.push(current);
                }
                match self.store.get_item(current).await {
                    Ok(Some(item)) => match item.parent_id {
                        Some(parent) => current = parent,
                        None => break,
                    },
                    // A row that vanished between the scan and here is not
                    // worth failing the request over; it simply gets no
                    // enrichment, exactly as if it had not been placed.
                    Ok(None) => {
                        tracing::warn!(item_id = current, "enrichment target disappeared");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(item_id = current, error = %e, "reading enrichment target");
                        break;
                    }
                }
            }
        }
        targets
    }

    /// The one enrichment path. Both scans call it; neither has its own copy.
    ///
    /// It is a method rather than two blocks because it used to be two
    /// blocks — or rather one, in `run_scan`, with `run_targeted` silently
    /// having none. Peer-ingested items got a row and never got artwork, and
    /// nothing about either function's shape said they were supposed to
    /// agree. Now they physically cannot drift apart: there is one place that
    /// knows a home library enriches locally, an anime library from AniList,
    /// and everything else from TMDB, and adding a fourth kind is one edit.
    ///
    /// `force` re-fetches already-matched items; `routes` narrows the provider
    /// entry points while `repairs` names the rows the caller actually wants
    /// changed. They differ when a season/episode needs its show only as the
    /// route into TMDB. `None` remains the whole-library behavior.
    async fn enrich(
        &self,
        publisher: &PublicationStore<'_>,
        library: &Library,
        force: bool,
        routes: Option<&[i64]>,
        repairs: Option<&[i64]>,
        repair_fence: Option<&ArtworkRepairFence>,
    ) -> EnrichOutcome {
        let mut outcome = EnrichOutcome::default();
        // Home libraries have no provider: their enrichment is local artwork.
        // Books currently keep file-derived identity and embedded media facts;
        // there is no provider to call. Anime uses AniList; movie/show
        // libraries use TMDB when a key is configured.
        // Recordings have a provider that already answered: the guide, at the
        // moment the capture was scheduled, written into the sidecar the scan
        // reads. Falling through this chain would reach TMDB, and a local
        // bulletin called *Eyewitness News* matches a dozen things there and
        // is none of them.
        if library.kind == LibraryKind::Recordings {
            return outcome;
        }
        if library.kind == LibraryKind::Home {
            outcome.local_art = Some(
                metadata::local::enrich_home_library_with_publication(
                    publisher,
                    &self.artwork_dir,
                    library.id,
                    force,
                    routes,
                )
                .await,
            );
        } else if library.kind == LibraryKind::Books {
            // File-derived EPUB facts are the standalone fallback. A paired
            // Curator handoff is applied after this pass and is source-ranked
            // above it by the store, so later scheduled scans cannot regress
            // explicit author/work/edition identity.
            outcome.books = Some(
                metadata::book::enrich_library_with_publication(
                    publisher,
                    &self.artwork_dir,
                    library.id,
                    force,
                    routes,
                )
                .await,
            );
        } else if library.anime {
            let client = AniListClient::new()
                .with_budget(publisher.provider_budget(Arc::clone(&self.store)));
            outcome.enrich = Some(
                metadata::enrich_anime_library_with_publication(
                    publisher,
                    &client,
                    &self.artwork_dir,
                    library.id,
                    force,
                    routes,
                    repair_fence,
                )
                .await,
            );
        } else {
            match self.store.get_setting(keys::TMDB_API_KEY).await {
                Ok(Some(key)) if !key.is_empty() => {
                    let tmdb = self
                        .tmdb_client(key)
                        .with_budget(publisher.provider_budget(Arc::clone(&self.store)));
                    outcome.enrich = Some(
                        metadata::enrich_library_for_targets_with_publication(
                            publisher,
                            &tmdb,
                            &self.artwork_dir,
                            Some(library.id),
                            force,
                            routes,
                            repairs.or(routes),
                            repair_fence,
                        )
                        .await,
                    );
                }
                Ok(_) => tracing::info!("no TMDB key configured; skipping enrichment"),
                Err(e) => tracing::warn!(error = %e, "reading TMDB key"),
            }
        }
        outcome
    }

    fn tmdb_client(&self, key: String) -> TmdbClient {
        #[cfg(test)]
        if let Some((base, image_base)) = &self.tmdb_base {
            return TmdbClient::new(key.as_str()).with_base(base, image_base);
        }
        TmdbClient::new(key)
    }

    /// Re-fetch artwork for one item and its ancestors, ignoring every "we
    /// already did this" marker. The per-item counterpart to a library
    /// refresh, for when a poster is wrong or missing on exactly one thing.
    pub async fn refresh_item_artwork(&self, item_id: i64) -> Result<EnrichOutcome, StoreError> {
        self.refresh_item_artwork_inner(item_id, None).await
    }

    /// Leader-arbitrated repair path. Every replicated provider mutation is
    /// conditioned on this owner/term proof inside its database statement.
    pub async fn refresh_item_artwork_fenced(
        &self,
        item_id: i64,
        repair_fence: &ArtworkRepairFence,
    ) -> Result<EnrichOutcome, StoreError> {
        self.refresh_item_artwork_inner(item_id, Some(repair_fence))
            .await
    }

    async fn refresh_item_artwork_inner(
        &self,
        item_id: i64,
        repair_fence: Option<&ArtworkRepairFence>,
    ) -> Result<EnrichOutcome, StoreError> {
        if repair_fence.is_some_and(|fence| fence.item_id != item_id) {
            return Err(StoreError::Task(
                "artwork repair fence does not name the requested item".to_owned(),
            ));
        }
        let Some(item) = self.store.get_item(item_id).await? else {
            return Ok(EnrichOutcome::default());
        };
        let Some(library) = self.store.get_library(item.library_id).await? else {
            return Ok(EnrichOutcome::default());
        };
        let Some(lease) = self.acquire_job("provider:artwork".to_owned()).await? else {
            return Err(StoreError::Task(
                "artwork provider pass is active on another cluster node".to_owned(),
            ));
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let outcome = tokio::select! {
            outcome = async {
                if library.kind == LibraryKind::Books
                    && item.book_metadata_source.as_deref()
                        == Some(BookMetadataSource::Curator.as_str())
                {
                    let expected = item.poster_path.iter().cloned().collect::<Vec<_>>();
                    let _ = metadata::book::materialize_curator_cover(
                        &publisher,
                        &self.artwork_dir,
                        item_id,
                        &expected,
                        repair_fence,
                    )
                    .await;
                    Ok(EnrichOutcome::default())
                } else {
                    // Episodes route through their show, but only the requested
                    // item may publish under its repair fence.
                    let targets = self.enrich_targets(&[item_id]).await;
                    let repairs = [item_id];
                    Ok(self
                        .enrich(
                            &publisher,
                            &library,
                            true,
                            Some(&targets),
                            Some(&repairs),
                            repair_fence,
                        )
                        .await)
                }
            } => outcome,
            () = lost.cancelled() => Err(StoreError::Task(
                "cluster artwork lease was lost".to_owned()
            )),
        };
        drop(publisher);
        let _ = lease.release().await;
        outcome
    }

    pub async fn reanalyze_files(
        &self,
        files: &[plurx_core::domain::MediaFile],
    ) -> Result<plurx_core::scan::ReprobeReport, StoreError> {
        let Some(lease) = self.acquire_job("repair:probe".to_owned()).await? else {
            return Err(StoreError::Task(
                "probe repair pass is active on another cluster node".to_owned(),
            ));
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let result = tokio::select! {
            result = scan::reprobe_files_with_publication(&publisher, files) => result,
            () = lost.cancelled() => Err(StoreError::Task(
                "cluster probe-repair lease was lost".to_owned(),
            )),
        };
        drop(publisher);
        let _ = lease.release().await;
        result
    }

    /// Recreate Home/Books bytes from media mounted on this voter without
    /// publishing catalogue fields. Books includes EPUB and audiobook covers;
    /// `None` means the library is provider-backed and therefore requires the
    /// cluster-wide source fence.
    pub async fn materialize_local_item_artwork(
        &self,
        item_id: i64,
        expected: &[String],
    ) -> Result<Option<bool>, StoreError> {
        let Some(item) = self.store.get_item(item_id).await? else {
            return Ok(Some(false));
        };
        let Some(library) = self.store.get_library(item.library_id).await? else {
            return Ok(Some(false));
        };
        match library.kind {
            LibraryKind::Home => Ok(Some(
                metadata::local::materialize_item_artwork(
                    self.store.as_ref(),
                    &self.artwork_dir,
                    item_id,
                    expected,
                )
                .await,
            )),
            LibraryKind::Books if matches!(item.kind, ItemKind::Book | ItemKind::Audiobook) => {
                metadata::book::materialize_item_cover(
                    self.store.as_ref(),
                    &self.artwork_dir,
                    item_id,
                    expected,
                    &self.book_cover_workers,
                )
                .await
            }
            LibraryKind::Books => Ok(None),
            // A recording's artwork would be the guide's programme image,
            // whose host is not on the approved artwork allowlist. Widening
            // that list is an SSRF decision of its own, not a side effect of
            // adding a library kind.
            LibraryKind::Recordings => Ok(None),
            LibraryKind::Movies | LibraryKind::Shows => Ok(None),
        }
    }

    /// Apply caller-supplied ids to what the scan placed.
    ///
    /// Best-effort by design: a failure here must not fail the scan. The
    /// files are indexed and playable either way, and a missing id degrades
    /// to the fuzzy title match plurx would have done anyway.
    async fn apply_ids(
        &self,
        req: &ScanRequest,
        items: &[PlacedFile],
        publisher: &PublicationStore<'_>,
    ) -> Vec<String> {
        let Some(ids) = req.ids.as_ref().filter(|i| !i.is_empty()) else {
            return Vec::new();
        };
        let mut problems = Vec::new();
        let mut seen = HashSet::new();
        for placed in items {
            if ids.episodeish {
                let Some(series_tmdb) = ids.series_tmdb else {
                    continue;
                };
                let Some(show_id) = self.show_root(placed.item_id).await else {
                    continue;
                };
                if !seen.insert(show_id) {
                    continue;
                }
                match publisher
                    .apply_series_tmdb_hint(req.library_id, show_id, series_tmdb)
                    .await
                {
                    Ok(SeriesHintOutcome::Applied | SeriesHintOutcome::AlreadyEqual) => {
                        tracing::info!(
                            target: "plurxd::integrate",
                            item = show_id,
                            tmdb = series_tmdb,
                            correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                            "applied caller-supplied series id"
                        );
                    }
                    Ok(SeriesHintOutcome::Conflict { current_tmdb_id }) => {
                        tracing::warn!(
                            target: "plurxd::integrate",
                            item = show_id,
                            current_tmdb = current_tmdb_id,
                            requested_tmdb = series_tmdb,
                            correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                            "caller-supplied series id conflicts with known metadata"
                        );
                        problems.push(format!(
                            "series TMDB hint {series_tmdb} conflicts with existing ID {current_tmdb_id} on show {show_id}; the existing ID was kept"
                        ));
                    }
                    Ok(SeriesHintOutcome::MissingOrWrongKind) => tracing::warn!(
                        target: "plurxd::integrate",
                        item = show_id,
                        tmdb = series_tmdb,
                        correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                        "series hint target is missing, outside the library, or not a show"
                    ),
                    Err(e) => tracing::warn!(
                        target: "plurxd::integrate",
                        item = show_id, error = %e,
                        correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                        "could not apply caller-supplied series id"
                    ),
                }
                continue;
            }

            if !seen.insert(placed.item_id) {
                continue;
            }
            let patch = MetadataPatch {
                tmdb_id: ids.tmdb,
                imdb_id: ids.imdb.clone(),
                ..Default::default()
            };
            match publisher.apply_metadata(placed.item_id, &patch).await {
                Ok(_) => tracing::info!(
                    target: "plurxd::integrate",
                    item = placed.item_id,
                    tmdb = patch.tmdb_id.unwrap_or(0),
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "applied caller-supplied ids"
                ),
                Err(e) => tracing::warn!(
                    target: "plurxd::integrate",
                    item = placed.item_id, error = %e,
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "could not apply caller-supplied ids; falling back to title matching"
                ),
            }
        }
        problems
    }

    /// Apply Curator's exact work/edition relation after local EPUB
    /// enrichment. Source precedence is also enforced in the store, so this
    /// ordering is defense in depth rather than a convention later scans can
    /// accidentally reverse.
    async fn apply_book_hints(
        &self,
        req: &ScanRequest,
        placed: &[PlacedFile],
        publisher: &PublicationStore<'_>,
    ) {
        let Some(hints) = req.book.as_ref() else {
            return;
        };
        let mut seen = HashSet::new();
        for file in placed {
            if !seen.insert(file.item_id) {
                continue;
            }
            let item = match self.store.get_item(file.item_id).await {
                Ok(Some(item)) if item.kind == hints.medium => item,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(item = file.item_id, error = %error, "reading Curator book target");
                    continue;
                }
            };
            let expected = item.poster_path.iter().cloned().collect::<Vec<_>>();
            let current_provider_cover = if item.book_metadata_source.as_deref()
                == Some(BookMetadataSource::Curator.as_str())
            {
                match metadata::book::has_curator_cover_origin(
                    self.store.as_ref(),
                    item.id,
                    item.book_edition_id.as_deref().unwrap_or_default(),
                    &expected,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(item = item.id, %error, "reading current Curator cover origin");
                        continue;
                    }
                }
            } else {
                false
            };
            let mut prepared_cover = None;
            if let Some(url) = hints.cover_url.as_deref() {
                match metadata::book::fetch_curator_cover(item.id, url).await {
                    Ok(cover) => {
                        let target = self.artwork_dir.join(cover.filename());
                        match metadata::reserve_artwork_publication(target) {
                            Ok(reservation) => prepared_cover = Some((cover, reservation, url)),
                            Err(error) => tracing::warn!(
                                item = item.id,
                                %error,
                                "reserving Curator cover publication"
                            ),
                        }
                    }
                    Err(error) => {
                        tracing::warn!(item = item.id, error = %error, "fetching Curator book cover");
                    }
                }
            }
            if !curator_pairing_can_advance(
                item.book_edition_id.as_deref(),
                current_provider_cover,
                &hints.edition_id,
                prepared_cover.is_some(),
            ) {
                tracing::warn!(
                    item = item.id,
                    current_edition = item.book_edition_id.as_deref().unwrap_or("-"),
                    requested_edition = %hints.edition_id,
                    "retaining Curator edition because its replacement cover is unavailable"
                );
                continue;
            }
            let (poster_path, required_origin) = if let Some((cover, reservation, url)) =
                prepared_cover.take()
            {
                let filename = cover.filename().to_owned();
                if let Err(error) =
                    metadata::book::publish_curator_cover(&self.artwork_dir, cover, reservation)
                        .await
                {
                    tracing::warn!(item = item.id, %error, "publishing Curator book cover");
                    continue;
                }
                let key =
                    metadata::book::curator_cover_origin_key(item.id, &hints.edition_id, &filename);
                let Some(origin) = metadata::book::curator_cover_origin_value(
                    &hints.edition_id,
                    Some(url),
                    Some(&filename),
                ) else {
                    tracing::warn!(
                        item = item.id,
                        "validated Curator cover origin became invalid"
                    );
                    continue;
                };
                if let Err(error) = publisher.put_setting_if_absent(&key, &origin).await {
                    tracing::warn!(item = item.id, %error, "persisting Curator cover origin");
                    continue;
                }
                (Some(filename), Some((key, origin)))
            } else {
                (None, None)
            };
            let patch = BookMetadataPatch {
                title: hints.title.clone(),
                author: hints.author.clone(),
                work_id: Some(hints.work_id.clone()),
                edition_id: Some(hints.edition_id.clone()),
                poster_path,
                source: BookMetadataSource::Curator,
                required_origin,
            };
            match publisher
                .apply_book_metadata_if_current(&item, &patch, None)
                .await
            {
                Ok(true) => {
                    tracing::info!(
                        target: "plurxd::integrate",
                        item = item.id,
                        work = %hints.work_id,
                        edition = %hints.edition_id,
                        correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                        "applied Curator book metadata"
                    );
                }
                Ok(false) => tracing::info!(
                    target: "plurxd::integrate",
                    item = item.id,
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "Curator pairing lost a concurrent update; retaining the newer item"
                ),
                Err(error) => tracing::warn!(
                    target: "plurxd::integrate",
                    item = item.id,
                    error = %error,
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "could not apply Curator book metadata"
                ),
            }
        }
    }

    /// Walk up to the item that carries a show's identity: an episode's ids
    /// belong to its series, not to the episode row.
    async fn show_root(&self, item_id: i64) -> Option<i64> {
        let mut current = self.store.get_item(item_id).await.ok()??;
        for _ in 0..4 {
            match current.parent_id {
                Some(parent) => current = self.store.get_item(parent).await.ok()??,
                None => break,
            }
        }
        Some(current.id)
    }

    async fn run_scan(
        &self,
        library_id: i64,
        progress: Arc<ScanProgress>,
        force_metadata: bool,
        lease: &ActiveJobLease,
    ) -> Result<TargetedScan, StoreError> {
        let mut status = ScanStatus {
            running: true,
            started_at: Some(now()),
            ..Default::default()
        };

        let library = match self.store.get_library(library_id).await {
            Ok(Some(lib)) => lib,
            Ok(None) => {
                self.finish(library_id, error_status("library not found"))
                    .await;
                return Err(StoreError::Task("library not found".into()));
            }
            Err(e) => {
                tracing::warn!(
                    library = library_id,
                    stage = "load_library",
                    error = %e,
                    "library scan failed"
                );
                self.finish(library_id, error_status(&e.to_string())).await;
                return Err(e);
            }
        };

        let publisher = lease.publisher(self.store.as_ref());
        match scan::scan_library_with_publication_and_prune_percent(
            &publisher,
            &library,
            Some(&progress),
            self.scan_prune_percent,
        )
        .await
        {
            Ok(report) => {
                self.note_fresh_index_files(std::mem::take(
                    &mut *progress
                        .changed_files
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                ));
                status.last_scan = Some(report);
            }
            Err(e) => {
                tracing::warn!(
                    library = library_id,
                    stage = "catalogue_scan",
                    error = %e,
                    "library scan failed"
                );
                self.finish(library_id, error_status(&e.to_string())).await;
                return Err(e);
            }
        }

        // Publish the scan result before enrichment starts, so the UI shows
        // real counts (and any problems) while metadata is still fetching.
        {
            let mut statuses = self.statuses.lock().await;
            if let Some(entry) = statuses.get_mut(&library_id) {
                entry.last_scan = status.last_scan.clone();
                entry.phase = Some("enriching".to_owned());
            }
        }

        // `None`: the whole library, which is what a full scan means. The
        // provider-choosing lives in `enrich` so the targeted path cannot
        // have a different idea of it.
        let outcome = self
            .enrich(&publisher, &library, force_metadata, None, None, None)
            .await;
        if let Some(scan) = status.last_scan.as_mut() {
            outcome.report_provider_outage(scan);
        }
        status.last_enrich = outcome.enrich;
        status.last_local_art = outcome.local_art;

        status.running = false;
        status.finished_at = Some(now());
        // Stamp the schedule from the *end* of the run, not the start: a scan
        // that takes 40 minutes on a 1-hour interval would otherwise be due
        // again 20 minutes later, and a library slower than its own interval
        // would scan without pause.
        if let Err(e) = publisher
            .mark_library_scanned(library_id, force_metadata)
            .await
        {
            tracing::warn!(
                error = %e,
                library = library_id,
                stage = "stamp_completion",
                "recording the library scan completion time failed"
            );
            self.finish(library_id, error_status(&e.to_string())).await;
            return Err(e);
        }
        crate::http::library_channels::notify_catalogue_mutation();
        let result = TargetedScan {
            report: status.last_scan.clone().unwrap_or_default(),
            items: Vec::new(),
        };
        self.finish(library_id, status).await;
        Ok(result)
    }

    /// The scheduler: ask [`crate::schedule::due_jobs`] once a minute, dispatch
    /// what it says, stamp what it ran.
    ///
    /// A minute is the resolution because the intervals are minutes and nothing
    /// here is urgent; the cost is one small query per tick. Scheduled and
    /// manual runs go through the same `trigger_*` methods, so a scheduled scan
    /// can't stack on top of a running one — `trigger` refuses, and the next
    /// tick tries again.
    ///
    /// The scheduler is cluster-wide work: every job it dispatches is one the
    /// cluster expects exactly one node to run. The eligibility check therefore
    /// sits on the tick, not on the spawn — a node that is a learner now may be
    /// a voter in ten minutes, and this loop has to start scheduling then
    /// without a restart. The individual leases are gated too, but skipping the
    /// tick keeps a learner from doing the reads and the log noise as well.
    ///
    /// The cluster integration feature exposes a process-
    /// local cadence override so real-daemon tests can wait on scheduler-owned
    /// prerequisites without adding a full minute to every fixture.
    pub async fn schedule_loop(self: Arc<Self>, transcode: Arc<TranscodeManager>) {
        self.scan_on_startup().await;
        #[cfg(feature = "cluster-integration-tests")]
        let interval = std::env::var("PLURX_TEST_SCHEDULER_TICK_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|milliseconds| (10..=60_000).contains(milliseconds))
            .map_or(Duration::from_secs(60), Duration::from_millis);
        #[cfg(not(feature = "cluster-integration-tests"))]
        let interval = Duration::from_secs(60);
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            self.schedule_tick(&transcode).await;
        }
    }

    /// One scheduler tick, gate included.
    ///
    /// Split out from the loop above so the gate is reachable without waiting
    /// a minute for the interval. Returns whether the tick dispatched: the
    /// answer is what a test can hold on to, and it is the whole difference
    /// between a learner that quietly does nothing and one that schedules the
    /// cluster's jobs a second time.
    async fn schedule_tick(self: &Arc<Self>, transcode: &Arc<TranscodeManager>) -> bool {
        if !self.may_run_cluster_jobs().await {
            tracing::debug!("skipping a scheduler tick: this node is not a committed voter");
            return false;
        }
        if let Err(error) = self.prepare_predictions().await {
            tracing::warn!(%error, "predictive preparation deferred");
        }
        if let Err(error) = self.prepare_hot_copies().await {
            tracing::warn!(%error, "hot artifact placement deferred");
        }
        if let Err(e) = self.run_due_jobs(transcode).await {
            tracing::warn!(error = %e, "scheduler tick failed");
        }
        true
    }

    /// Scan every library once at boot, if the operator asked for it.
    ///
    /// The interval schedule alone can't cover a server that was *off* while
    /// files landed: its clock only starts when the process does, so a machine
    /// powered up at noon on a daily schedule ignores everything added
    /// overnight until midnight. Off by default, like every other job.
    ///
    /// The delay is not politeness — a scan competes with the first plays of
    /// the morning for the same disks — and it is also what keeps a crash-loop
    /// from turning into a scan-loop against the media volume.
    async fn scan_on_startup(self: &Arc<Self>) {
        const SETTLE: std::time::Duration = std::time::Duration::from_secs(30);
        match self.store.get_setting(keys::JOB_SCAN_ON_STARTUP).await {
            Ok(Some(v)) if v.trim() == "1" => {}
            _ => return,
        }
        tokio::time::sleep(SETTLE).await;
        // After the settle, not before: the answer that matters is the one at
        // the moment work would start.
        if !self.may_run_cluster_jobs().await {
            tracing::debug!("startup scan skipped: this node is not a committed voter");
            return;
        }
        let libraries = match self.store.list_libraries().await {
            Ok(libraries) => libraries,
            Err(e) => {
                tracing::warn!(error = %e, "startup scan skipped: cannot list libraries");
                return;
            }
        };
        for library in libraries {
            if self.trigger_scan_as(library.id, ScanTrigger::Startup).await {
                tracing::info!(library = library.id, name = %library.name, "startup scan started");
            }
        }
    }

    /// Independent durable consumers: discovery keeps its existing cadence,
    /// while admitted work is retried with bounded, jittered read-only polling.
    pub(crate) async fn background_work_loop(self: Arc<Self>, transcode: Arc<TranscodeManager>) {
        self.background_work_loop_with_ready(transcode, None).await;
    }

    async fn background_work_loop_with_ready(
        self: Arc<Self>,
        transcode: Arc<TranscodeManager>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) {
        use plurx_core::store::background_jobs::JobKind;
        let preparation = async {
            let mut pacing = crate::background_jobs::IdlePoll::new();
            // Draining lists queued rows, a replicated read; once a minute is
            // prompt enough for a switch an operator just turned off.
            let mut last_drain: Option<std::time::Instant> = None;
            loop {
                let kinds = [
                    JobKind::TranscodePrepare,
                    JobKind::CopyOutputPrepare,
                    JobKind::EncodedOutputPrepare,
                ];
                let before = crate::background_jobs::accepted_claims(&kinds);
                // Each lane opens on its own setting: speculative work on the
                // discovery schedule, viewer-demand output preparation on its
                // Developer switch. The disabled output kinds are drained on
                // every node whether or not anything here can execute them.
                // An unreadable switch closes the output lane for this tick
                // and skips the drain: an error is not the operator saying off.
                let (lanes, output_read) = self.read_preparation_lanes().await;
                if output_read
                    && last_drain.is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
                {
                    last_drain = Some(std::time::Instant::now());
                    self.drain_disabled_output_preparation(lanes.output).await;
                }
                let lanes = self.executable_lanes(lanes).await;
                if !lanes.is_empty() {
                    Arc::clone(&self)
                        .work_pretranscode_queue(Arc::clone(&transcode), lanes)
                        .await;
                }
                let progressed = crate::background_jobs::accepted_claims(&kinds) != before;
                tokio::time::sleep(pacing.delay(progressed)).await;
            }
        };
        let fragments = async {
            let mut pacing = crate::background_jobs::IdlePoll::new();
            let mut wake = fragment_analysis_wake().subscribe();
            if let Some(ready) = ready {
                let _ = ready.send(());
            }
            loop {
                let kinds = [JobKind::FragmentIndexBuild, JobKind::ArtifactHydrate];
                let before = crate::background_jobs::accepted_claims(&kinds);
                if self
                    .job_authority
                    .may_execute_job(JobKind::FragmentIndexBuild)
                    .await
                    && self.cluster_fragment_index_enabled().await
                    && (!self.cluster_analysis_working.load(Ordering::Acquire)
                        || !self.cluster_index_working.load(Ordering::Acquire))
                {
                    // Keep listening for viewer wakeups while a source read or
                    // artifact build owns its own fenced pass. The independent
                    // local guards above and in the worker coalesce duplicate
                    // wakes without blocking this dispatch loop for minutes.
                    let jobs = Arc::clone(&self);
                    let transcode = Arc::clone(&transcode);
                    tokio::spawn(async move {
                        jobs.work_cluster_fragment_index_queue(transcode).await;
                    });
                }
                let progressed = crate::background_jobs::accepted_claims(&kinds) != before;
                tokio::select! {
                    _ = wake.recv() => {},
                    () = tokio::time::sleep(pacing.delay(progressed)) => {},
                }
            }
        };
        let libraries = async {
            let mut pacing = crate::background_jobs::IdlePoll::new();
            loop {
                let progressed = self.work_library_queue(&transcode, None).await;
                tokio::select! {
                    () = self.library_wake.notified() => {},
                    () = tokio::time::sleep(pacing.delay(progressed)) => {},
                }
            }
        };
        let subtitles = async {
            let mut pacing = crate::background_jobs::IdlePoll::new();
            loop {
                let progressed = self.work_subtitle_queue(&transcode).await;
                tokio::time::sleep(pacing.delay(progressed)).await;
            }
        };
        tokio::join!(preparation, fragments, libraries, subtitles);
    }

    async fn maintain_background_queue(self: Arc<Self>) {
        if self.background_upkeep_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let _guard = BackgroundUpkeepGuard(Arc::clone(&self));
        if !self.may_run_cluster_jobs().await {
            return;
        }
        if let Err(error) = self.store.maintain_jobs(clock_ms()).await {
            tracing::warn!(%error, "durable queue upkeep unavailable");
        }
        if let Err(error) = self.store.import_legacy_jobs(clock_ms()).await {
            tracing::warn!(%error, "durable queue legacy import unavailable");
        }
    }

    async fn run_due_jobs(
        self: &Arc<Self>,
        transcode: &Arc<TranscodeManager>,
    ) -> Result<(), plurx_core::error::StoreError> {
        // Receipt expiry and cancelled-owner cleanup continue even when every
        // background feature preference is off. The Store skips idle writes.
        let upkeep = Arc::clone(self);
        tokio::spawn(async move {
            upkeep.maintain_background_queue().await;
        });
        let libraries = self.store.list_libraries().await?;
        let global = GlobalSchedule {
            probe_retry_mins: self.job_interval(keys::JOB_PROBE_RETRY_MINS).await,
            last_probe_retry: self.job_stamp(keys::JOB_LAST_PROBE_RETRY).await,
            // The one job whose absent setting is not "off". A default of 0
            // here would ship the exact bug this job exists to fix: every
            // item whose poster download failed would sit there with a blank
            // card until someone found a button to press, which is the state
            // that made the job necessary in the first place.
            artwork_retry_mins: self
                .job_interval_or(
                    keys::JOB_ARTWORK_RETRY_MINS,
                    keys::ARTWORK_RETRY_DEFAULT_MINS,
                )
                .await,
            last_artwork_retry: self.job_stamp(keys::JOB_LAST_ARTWORK_RETRY).await,
            transcode_cleanup_mins: self.job_interval(keys::JOB_TRANSCODE_CLEANUP_MINS).await,
            last_transcode_cleanup: self
                .job_stamp(&self.local_job_key(keys::JOB_LAST_TRANSCODE_CLEANUP))
                .await,
            telemetry_retain_days: self
                .job_interval_or(
                    keys::TELEMETRY_RETAIN_DAYS,
                    keys::TELEMETRY_RETAIN_DEFAULT_DAYS,
                )
                .await,
            network_priors: self
                .store
                .get_setting(keys::PLAYBACK_NETWORK_PRIORS)
                .await?
                .is_some_and(|value| value.trim() == "1"),
            last_telemetry_prune: self
                .job_stamp(&self.local_job_key(keys::JOB_LAST_TELEMETRY_PRUNE))
                .await,
            cache_produce_mins: self.job_interval(keys::JOB_CACHE_PRODUCE_MINS).await,
            last_cache_produce: self.job_stamp(keys::JOB_LAST_CACHE_PRODUCE).await,
            // VOD is the only HLS presentation, so an absent index cadence
            // cannot mean "leave the library permanently unplayable". An
            // explicit stored zero still pauses indexing.
            vod_index_mins: self.job_interval_or(keys::VOD_INDEX_MINS, 15).await,
            last_vod_index: self
                .job_stamp(&self.local_job_key(keys::JOB_LAST_VOD_INDEX))
                .await,
        };
        for job in due_jobs(now(), &libraries, global) {
            match job {
                DueJob::Scan(id) => {
                    if self.trigger_scan_as(id, ScanTrigger::Scheduled).await {
                        tracing::info!(library = id, "scheduled scan queued");
                    }
                }
                DueJob::Refresh(id) => {
                    if self.trigger_refresh_as(id, ScanTrigger::Scheduled).await {
                        tracing::info!(library = id, "scheduled metadata refresh queued");
                    }
                }
                // Server-wide jobs are stamped before dispatch so one failure
                // cannot retry every minute forever.
                DueJob::RetryProbes => {
                    if let Some(lease) = self.acquire_job("repair:probe".to_owned()).await? {
                        let lost = lease.loss_token();
                        let publisher = lease.publisher(self.store.as_ref());
                        self.stamp(keys::JOB_LAST_PROBE_RETRY, &publisher).await;
                        let result: Result<(), StoreError> = tokio::select! {
                            result = async {
                                let files = self.store.files_missing_probe(None).await?;
                                if !files.is_empty() {
                                    let report =
                                        scan::reprobe_files_with_publication(&publisher, &files)
                                            .await?;
                                    tracing::info!(
                                        attempted = report.attempted,
                                        repaired = report.repaired,
                                        still_failing = report.still_failing,
                                        "scheduled re-probe finished"
                                    );
                                }
                                Ok(())
                            } => result,
                            () = lost.cancelled() => Err(StoreError::Task(
                                "cluster probe-repair lease was lost".to_owned(),
                            )),
                        };
                        drop(publisher);
                        let _ = lease.release().await;
                        result?;
                    }
                }
                DueJob::RetryArtwork => {
                    // Unlike probe retry, this can perform hundreds of paced
                    // provider calls. Keep it off the scheduler task so scans
                    // and disk cleanup remain dispatchable while it runs.
                    let state = Arc::clone(self);
                    tokio::spawn(async move { state.artwork_retry_pass().await });
                }
                DueJob::CleanupTranscode => {
                    self.stamp_local(keys::JOB_LAST_TRANSCODE_CLEANUP).await;
                    // Startup clears proven previous-process scratch under the daemon
                    // lock before admission. Retirement alone owns live-process paths.
                    // The cache is swept here as well as before each producer
                    // run, and the redundancy is the point: production and
                    // eviction are different settings, and a server whose
                    // producer was turned off after filling the cache still has
                    // to be able to get its disk back.
                    if let Some((root, node)) = transcode.cache_location() {
                        crate::cachekeep::sweep_with_readers(
                            &self.store,
                            root,
                            node,
                            transcode.cache_readers(),
                            now(),
                        )
                        .await;
                    }
                }
                DueJob::PruneTelemetry => {
                    let removed = self.prune_telemetry(now()).await?;
                    self.stamp_local(keys::JOB_LAST_TELEMETRY_PRUNE).await;
                    if removed > 0 {
                        tracing::info!(removed, "pruned aged playback telemetry");
                    }
                }
                DueJob::BuildFragmentIndexes => {
                    // Node-local work on node-local files, so the lease is
                    // only about not running two passes on THIS node — the
                    // single-flight guard inside does the rest.
                    let state = Arc::clone(self);
                    let transcode = Arc::clone(transcode);
                    tokio::spawn(async move {
                        state.build_fragment_indexes(transcode).await;
                    });
                }
                DueJob::ProduceCache => {
                    // Candidate generation is the singleton half. It only
                    // ranks and enqueues; every node's worker below competes
                    // for distinct execution rows.
                    let state = Arc::clone(self);
                    let transcode = Arc::clone(transcode);
                    tokio::spawn(async move {
                        state.enqueue_pretranscode_pass(transcode).await;
                    });
                }
            }
        }

        // Discovery's tick also acts as a local wake hint. The independent
        // durable consumer handles restart/retry latency between these ticks;
        // both paths share the same physical worker guard.
        if global.cache_produce_mins > 0 {
            let state = Arc::clone(self);
            let transcode = Arc::clone(transcode);
            tokio::spawn(async move {
                // Not narrowed by job authority, as before the lanes existed:
                // this pass also runs the node's local cachekeep sweep, which
                // a node owes its own cache whatever rows it may execute.
                // `claim_pretranscode` still checks authority per kind.
                let lanes = state.preparation_lanes().await;
                if !lanes.is_empty() {
                    Arc::clone(&state)
                        .work_pretranscode_queue(transcode, lanes)
                        .await;
                }
            });
        }

        // Discovery stays on the configured library cadence, but execution is
        // an active shared queue: every scheduler tick lets voters compete for
        // the two cluster-wide media-read slots. A request miss can therefore
        // enqueue an exact key without waiting for another discovery pass.
        if setting_enabled(
            self.store
                .get_setting(keys::VOD_INDEX_CLUSTER_CACHE)
                .await
                .unwrap_or(None),
        ) {
            let state = Arc::clone(self);
            let transcode = Arc::clone(transcode);
            tokio::spawn(async move {
                state.work_cluster_fragment_index_queue(transcode).await;
            });
        }

        // Permanent conversion is an active durable queue. Discovery honors
        // each library's `auto` policy; manual rows use the same worker and do
        // not wait for a separate cadence.
        // Orphan recovery guards are filesystem lifecycle state, not a media
        // conversion. Always dispatch the bounded worker so it can retire one
        // orphan even when this node has no usable dovi_tool/mkvmerge; the
        // worker returns before discovery/conversion when tools are absent.
        let state = Arc::clone(self);
        tokio::spawn(async move { state.work_dv_disk_queue().await });

        // Not a `DueJob`: there is no interval to decide about. It runs on
        // every tick while it is armed and stops by disarming itself, which
        // is the whole of its schedule — putting that through `due_jobs`
        // would be a scheduling decision that isn't one.
        if metadata::genres::is_armed(self.store.as_ref()).await {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.genre_backfill_pass().await });
        }
        // Also not a `DueJob`, and deliberately not hung off the VOD index
        // tick: `vod_index_mins = 0` is an operator-offered "Paused", and a
        // schema migration whose data half a preference can switch off is a
        // library that silently keeps a column of nulls.
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_dolby_vision_facts().await });
        }
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_video_codec_tags().await });
        }
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_field_order().await });
        }
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_luminance_facts().await });
        }
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_frame_luminance().await });
        }
        {
            let state = Arc::clone(self);
            tokio::spawn(async move { state.backfill_unsignalled_transfer().await });
        }
        Ok(())
    }

    /// Run one bounded artwork-retry batch outside the scheduler task.
    async fn artwork_retry_pass(self: Arc<Self>) {
        if self.retrying_artwork.swap(true, Ordering::Relaxed) {
            tracing::debug!("an artwork retry pass is already running; skipping this one");
            return;
        }
        let _guard = ArtworkRetryGuard(Arc::clone(&self));
        match self.acquire_job("provider:artwork".to_owned()).await {
            Ok(Some(lease)) => {
                let lost = lease.loss_token();
                let publisher = lease.publisher(self.store.as_ref());
                self.stamp(keys::JOB_LAST_ARTWORK_RETRY, &publisher).await;
                let result = tokio::select! {
                    result = self.sweep_artwork_with_publication(
                        keys::ARTWORK_RETRY_BACKOFF_SECS,
                        &publisher,
                    ) => result,
                    () = lost.cancelled() => Err(StoreError::Task(
                        "cluster artwork lease was lost".to_owned(),
                    )),
                };
                match result {
                    Ok(report) => tracing::debug!(
                        repaired = report.repaired,
                        "cluster artwork retry publication complete"
                    ),
                    Err(error) => {
                        tracing::warn!(error = %error, "artwork retry sweep failed");
                    }
                }
                drop(publisher);
                let _ = lease.release().await;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(error = %error, "artwork retry lease failed"),
        }
    }

    /// Delete a bounded amount of expired telemetry. Ten small batches keep a
    /// long-disabled node from monopolizing SQLite when retention is re-armed;
    /// another daily pass continues the backlog without an unbounded delete.
    async fn prune_telemetry(&self, now_secs: i64) -> Result<u64, StoreError> {
        const BATCH: i64 = 1_000;
        const MAX_BATCHES: usize = 10;
        let retain_days = self
            .job_interval_or(
                keys::TELEMETRY_RETAIN_DAYS,
                keys::TELEMETRY_RETAIN_DEFAULT_DAYS,
            )
            .await;
        let mut removed = 0;
        if retain_days > 0 {
            let before_ms = now_secs
                .saturating_sub(retain_days.saturating_mul(24 * 60 * 60))
                .saturating_mul(1_000);
            for _ in 0..MAX_BATCHES {
                let batch = self.store.prune_playback_events(before_ms, BATCH).await?;
                removed += batch;
                if batch < BATCH as u64 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        let prior_before_ms = now_secs
            .saturating_sub(keys::NETWORK_PRIOR_RETAIN_DAYS * 24 * 60 * 60)
            .saturating_mul(1_000);
        for _ in 0..MAX_BATCHES {
            let batch = self
                .store
                .prune_network_priors(prior_before_ms, BATCH)
                .await?;
            removed += batch;
            if batch < BATCH as u64 {
                break;
            }
            tokio::task::yield_now().await;
        }
        Ok(removed)
    }

    /// One paced, resumable pass of the genre backfill (S3).
    ///
    /// Spawned rather than run inline for the reason the producer is: a pass
    /// is up to [`metadata::genres::BATCH`] paced provider calls, and the tick
    /// it would otherwise block is what starts scans and sweeps. The guard is
    /// what makes "every tick" safe — a pass still going when the next minute
    /// arrives is not joined by a second one reading the same cursor.
    async fn genre_backfill_pass(self: Arc<Self>) {
        if self.backfilling_genres.swap(true, Ordering::Relaxed) {
            tracing::debug!("a genre backfill pass is already running; skipping this one");
            return;
        }
        let _guard = GenreBackfillGuard(Arc::clone(&self));
        let lease = match self.acquire_job("provider:genres".to_owned()).await {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %error, "genre backfill lease failed");
                return;
            }
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());

        // No key configured is not a reason to skip the pass: anime libraries
        // enrich from AniList, which needs none, and those titles are exactly
        // as entitled to genres as the rest.
        let tmdb = match self.store.get_setting(keys::TMDB_API_KEY).await {
            Ok(Some(key)) if !key.is_empty() => Some(
                TmdbClient::new(key)
                    .with_budget(publisher.provider_budget(Arc::clone(&self.store))),
            ),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, "genre backfill: reading TMDB key");
                None
            }
        };
        let anilist =
            AniListClient::new().with_budget(publisher.provider_budget(Arc::clone(&self.store)));
        let report = tokio::select! {
            report = metadata::genres::backfill_pass_with_publication(
                &publisher,
                tmdb.as_ref(),
                &anilist,
                metadata::genres::PACE,
            ) => report,
            () = lost.cancelled() => {
                tracing::warn!("genre backfill stopped after losing its cluster lease");
                None
            },
        };
        if let Some(report) = report {
            for problem in &report.problems {
                tracing::error!(problem = %problem, "genre backfill problem");
            }
            *self.last_genre_backfill.lock().await = Some(report);
        }
        drop(publisher);
        let _ = lease.release().await;
    }

    /// Singleton half of speculative production: rank likely titles and put
    /// their immutable source generations on the distributed queue.
    async fn enqueue_pretranscode_pass(self: Arc<Self>, transcode: Arc<TranscodeManager>) {
        let lease = match self.acquire_job("candidate:pretranscode".to_owned()).await {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %error, "pre-transcode candidate lease failed");
                return;
            }
        };
        let publisher = lease.publisher(self.store.as_ref());
        self.stamp(keys::JOB_LAST_CACHE_PRODUCE, &publisher).await;
        let lost = lease.loss_token();
        use crate::produce;
        let discoveries = match self.prediction_discoveries().await {
            Ok(discoveries) => discoveries,
            Err(error) => {
                tracing::warn!(%error, "candidate generator could not read demand");
                drop(publisher);
                let _ = lease.release().await;
                return;
            }
        };
        let mut candidates = Vec::with_capacity(PRODUCE_MAX_PER_PASS);
        for prediction in discoveries {
            let discovery = prediction.discovery;
            let file = if let Some(file_id) = prediction.file_id {
                self.store.get_file(file_id).await.ok().flatten()
            } else {
                self.store
                    .files_for_item(discovery.item_id)
                    .await
                    .ok()
                    .and_then(|files| files.into_iter().next())
            };
            let Some(file) = file else {
                continue;
            };
            if !produce::worth_producing(&file) {
                continue;
            }
            candidates.push((
                produce::Candidate {
                    file_id: file.id,
                    item_id: discovery.item_id,
                    title: discovery.title,
                    reason: discovery.reason,
                },
                file,
            ));
            if candidates.len() == PRODUCE_MAX_PER_PASS {
                break;
            }
        }
        let total = candidates.len();
        let mut enqueued = 0_u64;
        let mut skipped = 0_u64;
        let mut reasons = std::collections::BTreeMap::<&'static str, u64>::new();
        let policy = match transcode.try_pretranscode_policy_snapshot().await {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, "candidate generation could not read transcode policy");
                drop(publisher);
                let _ = lease.release().await;
                return;
            }
        };
        for (i, (c, file)) in candidates.into_iter().enumerate() {
            if lost.is_cancelled() {
                tracing::warn!("candidate generation stopped after losing its cluster lease");
                skipped += (total - i) as u64;
                reasons.insert("lease_lost", (total - i) as u64);
                break;
            }
            let height = policy.target_height(&file);
            let decoder = match file.video_codec.as_deref() {
                Some("h264" | "avc") => "h264",
                Some("hevc" | "h265" | "hevc10") => "hevc",
                Some("vp8") => "vp8",
                Some("vp9") => "vp9",
                Some("av1") => "av1",
                Some("mpeg4") => "mpeg4",
                Some("mpeg2video") => "mpeg2video",
                _ => {
                    skipped += 1;
                    *reasons.entry("unsupported_decoder_contract").or_default() += 1;
                    continue;
                }
            };
            // Dolby Vision needs an RPU-aware renderer capability, not the
            // ordinary HDR tone-map bit. Keep it on the live path until the
            // queue contract can express that exact proof.
            if file.hdr.as_deref() == Some("dolby_vision") {
                skipped += 1;
                *reasons
                    .entry("dolby_renderer_contract_pending")
                    .or_default() += 1;
                continue;
            }
            let Some(duration_ms) = file.duration_ms.filter(|duration| *duration > 0) else {
                skipped += 1;
                *reasons.entry("missing_duration").or_default() += 1;
                continue;
            };
            let Some(peak_kbps) = crate::transcode::ladder(file.height)
                .into_iter()
                .find(|rung| rung.height == height)
                .map(|rung| rung.peak_kbps)
            else {
                skipped += 1;
                *reasons.entry("missing_output_contract").or_default() += 1;
                continue;
            };
            const GENERATION_OVERHEAD_BYTES: i64 = 64 * 1024 * 1024;
            let artifact_bytes = duration_ms
                .saturating_mul(i64::from(peak_kbps))
                .saturating_div(8);
            // Retained parts plus one complete copied generation are the
            // portable worst case on filesystems without hard-link support.
            let scratch_bytes = artifact_bytes
                .saturating_mul(2)
                .saturating_add(GENERATION_OVERHEAD_BYTES);
            let mut hasher = Sha256::new();
            for value in [
                file.id.to_string(),
                file.size.to_string(),
                file.mtime.to_string(),
                height.to_string(),
                policy.generation.clone(),
            ] {
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
            let requirements = PretranscodeRequirements {
                version: PretranscodeRequirements::VERSION,
                decoder: decoder.to_owned(),
                acceptable_encoder_families: policy.acceptable_encoder_families(),
                output_contract: "hls-mpegts-v1".to_owned(),
                tone_map: file.hdr.is_some(),
                output_grade: "sdr".to_owned(),
                scratch_bytes,
            };
            let created_at_ms = clock_ms();
            let priority_base = match c.reason {
                produce::REASON_IN_PROGRESS => 300,
                produce::REASON_NEXT_UP => 200,
                _ => 100,
            };
            let job = NewPretranscodeJob {
                id: uuid::Uuid::new_v4().to_string(),
                dedupe_key: hex::encode(hasher.finalize()),
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                target_height: height,
                policy_generation: policy.generation.clone(),
                requirements_json: serde_json::to_string(&requirements)
                    .expect("bounded pre-transcode requirements serialize"),
                reason: produce::queue_reason(c.reason)
                    .expect("ranked discovery reasons have stable queue identifiers")
                    .to_owned(),
                priority: priority_base + i64::try_from(total.saturating_sub(i)).unwrap_or(0),
                not_before_ms: created_at_ms,
                created_at_ms,
            };
            match publisher.enqueue_durable_pretranscode(&job).await {
                Ok(true) => {
                    enqueued += 1;
                    tracing::info!(
                        job = job.id,
                        file = file.id,
                        title = %c.title,
                        reason = c.reason,
                        height,
                        "queued speculative transcode"
                    );
                }
                Ok(false) => {
                    skipped += 1;
                    *reasons.entry("already_active_or_ready").or_default() += 1;
                }
                Err(error) => {
                    skipped += 1;
                    *reasons.entry("enqueue_failed").or_default() += 1;
                    tracing::warn!(title = %c.title, %error, "could not queue speculative transcode");
                    if lost.is_cancelled() {
                        break;
                    }
                }
            }
        }
        crate::telemetry::emit(
            Arc::clone(&self.store),
            PlaybackEvent {
                at_unix_ms: clock_ms(),
                event: "producer_candidates".to_owned(),
                extra: Some(
                    serde_json::json!({
                        "enqueued": enqueued,
                        "skipped": skipped,
                        "reasons": reasons,
                    })
                    .to_string(),
                ),
                ..PlaybackEvent::default()
            },
        );
        drop(publisher);
        let _ = lease.release().await;
    }

    /// Non-singleton half: every idle compatible node drains distinct queue
    /// rows through the existing preemptible producer.
    /// Build fragment indexes for files that have none.
    ///
    /// Bounded three ways on purpose, because this reads whole files off the
    /// same disks a playback reads from and buys a viewer nothing today:
    /// [`INDEX_MAX_PER_PASS`] files, [`INDEX_WINDOW`] of wall clock, and
    /// [`INDEX_FILE_BUDGET`] per file. It also declines to start while the
    /// pre-transcode worker is busy, for the same reason that worker declines
    /// to start while a foreground session is.
    ///
    /// VOD is the only segmented presentation, so a missing current index is
    /// an explicit `vod_index_pending` refusal. Empty or preempted passes must
    /// therefore remain due instead of consuming the full cadence before the
    /// scanner has published the files they need to examine.
    /// Forget node-local VOD rows whose file no longer exists.
    ///
    /// Bounded per tick rather than exhaustive: the rows are small, nothing
    /// depends on them going promptly, and a sweep that walked a whole library
    /// would compete with the indexing pass it runs in front of. Ordered by
    /// file id so consecutive ticks make progress rather than re-examining the
    /// same window.
    ///
    /// Failure is not worth interrupting the tick for. These rows are a leak,
    /// not a correctness problem -- an index whose file is gone stops matching
    /// on identity long before anyone could serve from it.
    async fn sweep_orphaned_vod_rows(&self) {
        const SWEEP_WINDOW: i64 = 512;

        let held = match self.store.vod_row_file_ids(SWEEP_WINDOW).await {
            Ok(ids) if !ids.is_empty() => ids,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(error = %error, "listing node-local VOD rows to sweep");
                return;
            }
        };
        let alive = match self.store.surviving_file_ids(&held).await {
            Ok(alive) => alive,
            Err(error) => {
                tracing::warn!(error = %error, "checking which indexed files still exist");
                return;
            }
        };

        let mut indexes = 0usize;
        let mut plans = 0usize;
        for file_id in held.iter().filter(|id| !alive.contains(id)) {
            match self.store.forget_fragment_index(*file_id).await {
                Ok(true) => indexes += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(file = file_id, error = %error, "forgetting a stale index");
                }
            }
            match self.store.forget_rendition_plans(*file_id).await {
                Ok(count) => plans += count,
                Err(error) => {
                    tracing::warn!(file = file_id, error = %error, "forgetting stale plans");
                }
            }
        }
        if indexes > 0 || plans > 0 {
            tracing::info!(
                indexes,
                plans,
                examined = held.len(),
                "swept node-local VOD rows for files that are gone"
            );
        }
    }

    /// Fill in the Dolby Vision columns from the probe JSON already stored for
    /// each file (PLAYBACK-CAPS-V2-PLAN §4.3).
    ///
    /// The incremental scanner skips unchanged files, so without this an
    /// existing library would never gain the columns short of a destructive
    /// re-add — the same reason `hdr_format` needed a backfill when it was
    /// added. No ffprobe runs: every fact comes out of JSON that was captured
    /// at scan time and has been sitting in the row ever since.
    ///
    /// Walks a node-local cursor, strictly forward, and stamps itself done
    /// when a window comes back empty. The cursor is what makes it terminate:
    /// some rows can never be fixed here — a Dolby Vision codec tag whose
    /// stored probe carries no configuration record needs a real re-probe,
    /// which is an operator action rather than a boot side-effect — and
    /// without a cursor they would sit at the front of every window forever,
    /// re-read every tick, hiding every fixable row behind them.
    ///
    /// The label moves with the columns only when it would actually change. A
    /// label naming no profile is wrong rather than merely sparse, and leaving
    /// it keeps the file unclaimable by every client — but `hdr_format` is an
    /// input to `copy_video_args`, so rewriting one re-keys that file's
    /// fragment index and orphans what was built. Comparing first makes the
    /// common case structurally free instead of empirically free.
    ///
    /// `bl_compat_id` is now an input to `copy_video_args` too — it is what
    /// `transcode::dolby_vision_has_compatible_base` reads before the label —
    /// and this backfill writes the columns unconditionally. That is free only
    /// while the stored label is the one `scan::probe::dolby_vision_label`
    /// derives from those same columns, so the two answers move together and
    /// a rewritten column cannot change the argv on its own. `probe.rs`'s
    /// `the_derived_label_is_the_label_the_scan_used_to_write` is what holds
    /// that; if the label's marker mapping and the compatibility-id set ever
    /// drift apart, this becomes a re-key of every Dolby Vision row.
    async fn backfill_dolby_vision_facts(self: Arc<Self>) {
        const BACKFILL_PER_TICK: i64 = 256;

        match self.store.get_setting(keys::JOB_DV_BACKFILL_DONE).await {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "reading the Dolby Vision backfill stamp");
                return;
            }
        }
        // One node at a time. The rows are replicated, so every voter running
        // this simultaneously would be N times the Raft writes for one
        // outcome — and a learner, which may not run cluster jobs at all,
        // would be doing them uninvited.
        let lease = match self.acquire_job("catalogue:dv-facts".to_owned()).await {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "Dolby Vision backfill lease failed");
                return;
            }
        };
        let _lease = lease;
        let cursor_key = self.local_job_key(keys::JOB_DV_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let pending = match self
            .store
            .files_missing_dolby_vision(cursor, BACKFILL_PER_TICK)
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "listing files for the Dolby Vision backfill");
                return;
            }
        };
        if pending.is_empty() {
            // Nothing left ahead of the cursor. Stamped rather than left to
            // re-ask, because the only rows that could still appear behind it
            // are ones a future scan writes, and a scan writes the columns
            // itself.
            if let Err(error) = self
                .store
                .put_setting(keys::JOB_DV_BACKFILL_DONE, "1")
                .await
            {
                tracing::warn!(%error, "stamping the Dolby Vision backfill as complete");
            } else {
                tracing::info!("dv backfill: complete");
            }
            return;
        }
        let mut updated = 0usize;
        let mut relabelled = 0usize;
        let mut without_record = 0usize;
        let mut walked = cursor;
        for (file_id, probe_json, stored_label) in pending {
            walked = walked.max(file_id);
            let facts = serde_json::from_str::<serde_json::Value>(&probe_json)
                .ok()
                .map(|value| plurx_core::scan::probe::parse_probe_json(&value));
            let Some(probe) = facts.filter(|probe| !probe.dolby_vision.is_empty()) else {
                // The row says Dolby Vision and its stored JSON has no
                // configuration record to prove it. Nothing here can fix that;
                // the cursor walks past so it cannot block the rest.
                without_record += 1;
                continue;
            };
            // Only when it would actually change. See the doc comment: this is
            // what keeps the re-index cost at zero rather than merely small.
            let label = probe
                .hdr_format
                .as_deref()
                .filter(|derived| Some(*derived) != stored_label.as_deref());
            if label.is_some() {
                relabelled += 1;
            }
            if let Err(error) = self
                .store
                .set_file_dolby_vision(file_id, probe.dolby_vision, label)
                .await
            {
                tracing::warn!(file_id, %error, "writing backfilled Dolby Vision facts");
                // Do NOT walk past a row this pass failed to write: a
                // transient store error must leave it due, not skipped.
                walked = walked.min(file_id.saturating_sub(1));
                break;
            }
            updated += 1;
        }
        if walked > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &walked.to_string())
                .await
            {
                tracing::warn!(%error, "advancing the Dolby Vision backfill cursor");
            }
        }
        if updated > 0 || without_record > 0 {
            tracing::info!(
                updated,
                relabelled,
                without_record,
                cursor = walked,
                "dv backfill: filled Dolby Vision columns from stored probe data"
            );
        }
    }

    /// Recover the first playable video's sample-entry label from the probe
    /// JSON already stored with each catalogue row.
    ///
    /// The scanner writes this column for new and changed files. This bounded
    /// job exists for unchanged libraries: it never opens media or launches
    /// ffprobe, and it fences every write with the exact path, size, mtime and
    /// probe snapshot that produced the recovered tag.
    async fn backfill_video_codec_tags(self: Arc<Self>) {
        const BACKFILL_PER_TICK: i64 = 256;

        match self
            .store
            .get_setting(keys::JOB_VIDEO_CODEC_TAG_BACKFILL_DONE)
            .await
        {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "reading the video codec tag backfill stamp");
                return;
            }
        }
        let lease = match self
            .acquire_job("catalogue:video-codec-tag".to_owned())
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "video codec tag backfill lease failed");
                return;
            }
        };
        let _lease = lease;
        let cursor_key = self.local_job_key(keys::JOB_VIDEO_CODEC_TAG_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let pending = match self
            .store
            .files_missing_video_codec_tag(cursor, BACKFILL_PER_TICK)
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "listing files for the video codec tag backfill");
                return;
            }
        };
        if pending.is_empty() {
            if let Err(error) = self
                .store
                .put_setting(keys::JOB_VIDEO_CODEC_TAG_BACKFILL_DONE, "1")
                .await
            {
                tracing::warn!(%error, "stamping the video codec tag backfill as complete");
            } else {
                tracing::info!("video codec tag backfill: complete");
            }
            return;
        }

        let mut updated = 0usize;
        let mut unknown = 0usize;
        let mut fenced = 0usize;
        let mut walked = cursor;
        for candidate in pending {
            walked = walked.max(candidate.id);
            let recovered = serde_json::from_str::<serde_json::Value>(&candidate.probe_json)
                .ok()
                .map(|value| plurx_core::scan::probe::parse_probe_json(&value))
                .and_then(|probe| probe.video_codec_tag);
            let Some(tag) = recovered else {
                unknown += 1;
                continue;
            };
            match self.store.set_file_video_codec_tag(&candidate, &tag).await {
                Ok(true) => updated += 1,
                Ok(false) => fenced += 1,
                Err(error) => {
                    tracing::warn!(
                        file_id = candidate.id,
                        %error,
                        "writing a backfilled video codec tag"
                    );
                    walked = walked.min(candidate.id.saturating_sub(1));
                    break;
                }
            }
        }
        if walked > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &walked.to_string())
                .await
            {
                tracing::warn!(%error, "advancing the video codec tag backfill cursor");
            }
        }
        tracing::info!(
            updated,
            unknown,
            fenced,
            cursor = walked,
            "video codec tag backfill: considered stored probe rows"
        );
    }

    async fn backfill_field_order(self: Arc<Self>) {
        const BACKFILL_PER_TICK: i64 = 256;

        match self
            .store
            .get_setting(keys::JOB_FIELD_ORDER_BACKFILL_DONE)
            .await
        {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "reading the field-order backfill stamp");
                return;
            }
        }
        let lease = match self.acquire_job("catalogue:field-order".to_owned()).await {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "field-order backfill lease failed");
                return;
            }
        };
        let _lease = lease;
        let cursor_key = self.local_job_key(keys::JOB_FIELD_ORDER_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let page = match plurx_core::store::field_order_backfill_page(
            self.store.as_ref(),
            cursor,
            BACKFILL_PER_TICK,
        )
        .await
        {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, "field-order backfill page failed");
                return;
            }
        };
        if page.complete {
            tracing::info!("field-order backfill: complete");
            return;
        }
        if let Some((file_id, error)) = &page.write_error {
            tracing::warn!(file_id, %error, "writing a backfilled field order");
        }
        if page.cursor > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &page.cursor.to_string())
                .await
            {
                tracing::warn!(%error, "advancing the field-order backfill cursor");
            }
        }
        tracing::info!(
            updated = page.updated,
            fenced = page.fenced,
            cursor = page.cursor,
            "field-order backfill: considered stored probe rows"
        );
    }

    /// Classify existing HDR rows from their retained stream document. This
    /// never opens media: rows whose document carries no luminance record —
    /// HEVC that keeps MDCV/CLL only as SEI, the common case — are stamped
    /// `none`. An unchanged file is never rescanned, so nothing else would
    /// revisit them; [`Self::backfill_frame_luminance`] reads their first
    /// frame once this walk is complete.
    async fn backfill_luminance_facts(self: Arc<Self>) {
        const BACKFILL_PER_TICK: i64 = 256;
        if !matches!(
            self.store
                .get_setting(keys::JOB_LUMINANCE_BACKFILL_DONE)
                .await,
            Ok(None)
        ) {
            return;
        }
        let Ok(Some(_lease)) = self.acquire_job("catalogue:luminance".to_owned()).await else {
            return;
        };
        let cursor_key = self.local_job_key(keys::JOB_LUMINANCE_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let pending = match self
            .store
            .files_missing_luminance(cursor, BACKFILL_PER_TICK)
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "listing files for the luminance backfill");
                return;
            }
        };
        if pending.is_empty() {
            match self
                .store
                .put_setting(keys::JOB_LUMINANCE_BACKFILL_DONE, "1")
                .await
            {
                Ok(()) => tracing::info!("luminance backfill: complete"),
                Err(error) => tracing::warn!(%error, "stamping the luminance backfill complete"),
            }
            return;
        }
        let mut walked = cursor;
        let mut updated = 0usize;
        let mut fenced = 0usize;
        for candidate in pending {
            walked = walked.max(candidate.id);
            let recovered = stored_luminance(&candidate.probe_json);
            let source = if recovered.max_cll.is_some()
                || recovered.max_fall.is_some()
                || recovered.mastering_max_luminance.is_some()
            {
                "stream"
            } else {
                "none"
            };
            match self
                .store
                .set_file_luminance(
                    &candidate,
                    recovered.max_cll,
                    recovered.max_fall,
                    recovered.mastering_max_luminance,
                    source,
                )
                .await
            {
                Ok(true) => updated += 1,
                Ok(false) => fenced += 1,
                Err(error) => {
                    tracing::warn!(file_id = candidate.id, %error, "writing backfilled luminance facts");
                    walked = walked.min(candidate.id.saturating_sub(1));
                    break;
                }
            }
        }
        if walked > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &walked.to_string())
                .await
            {
                tracing::warn!(%error, "advancing the luminance backfill cursor");
            }
        }
        tracing::info!(
            updated,
            fenced,
            cursor = walked,
            "luminance backfill: considered stored probe rows"
        );
    }

    /// Read the first frame of every HDR row the stored-document walk left
    /// `none`, with the scanner's own bounded probe.
    ///
    /// Without this, a title mastered at 4,000 nits whose metadata lives only
    /// in frame side data tone-maps against the 1,000-nit policy default for
    /// as long as its file is unchanged: the scanner runs this read only when
    /// it probes a file, and it probes only new or changed files.
    ///
    /// What it does not cover: a row the scanner writes `none` *after* this
    /// walk has passed its id. The scanner writes `none` only once it has
    /// already attempted this same frame read on that file, at scan time, so
    /// such a row is not one that never had the read — it is one whose read
    /// found nothing or failed then, and it waits for the file's next change
    /// like every other scan-time probe fact. This walk exists for the rows
    /// that never had a frame read at all: those the stored-document walk
    /// classified from a retained stream document.
    async fn backfill_frame_luminance(self: Arc<Self>) {
        self.backfill_frame_luminance_with(
            |path| async move {
                plurx_core::scan::probe::first_frame_luminance(
                    &path,
                    "catalogue luminance backfill",
                )
                .await
            },
            plurx_core::scan::probe::self_check,
        )
        .await;
    }

    /// [`Self::backfill_frame_luminance`] with the frame read supplied, so
    /// the walk's state transitions are testable without media.
    ///
    /// One small page per tick, because each candidate opens and decodes
    /// media. The cursor only moves past a row on evidence about that row;
    /// anything that says "this node cannot read right now" stops the page
    /// where it is, so an unavailable library or a broken ffprobe is retried
    /// rather than walked to `done` having read nothing. Per candidate:
    ///
    /// - stored document not PQ/HLG without a stream luminance record (the
    ///   scanner's own eligibility for this read; the stored-document walk
    ///   stamps every unclassified HDR row, including a Dolby Vision stream
    ///   whose base layer is neither): skipped;
    /// - file on disk is not the catalogued size/mtime: skipped, because the
    ///   rescan that change triggers runs this same read;
    /// - file cannot be stat'ed: its library root is checked with the
    ///   scanner's root-availability read. Root available → the file is gone
    ///   or a dangling link, a stale row the next scan reconciles: skipped.
    ///   Root unavailable (share unmounted, media not on this node) → stop;
    /// - frame carries luminance: written as `frame`, fenced to the exact row
    ///   snapshot and to the row still being `none`;
    /// - frame carries none: stays `none`, cursor moves on;
    /// - ffprobe refused the file (a [`plurx_core::error::ProbeError::is_file_verdict`]): stays
    ///   `none`, cursor moves on — a new probe document is what earns it
    ///   another read (open question 2 of the tone-map plan). A page in
    ///   which more than one read was attempted and every one was refused
    ///   asks ffprobe's own self-check (see [`settle_refused_page`]): if it
    ///   passes, the refusals are verdicts on those files and the cursor moves
    ///   past them; only a failing self-check holds the cursor, at the first
    ///   refused row;
    /// - ffprobe could not run or did not finish (not a verdict): stop;
    /// - the store write fails: stop.
    ///
    /// The `done` stamp therefore follows a walk in which every row passed
    /// was skipped on its own facts, read, or refused by a reader that
    /// passed its own self-check — the rows it leaves `none` are the ones a
    /// frame read cannot improve until their file changes.
    async fn backfill_frame_luminance_with<F, Fut, G, GFut>(&self, read_frame: F, self_check: G)
    where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<
            Output = Result<
                Option<plurx_core::scan::probe::FrameLuminance>,
                plurx_core::error::ProbeError,
            >,
        >,
        G: FnOnce() -> GFut,
        GFut: std::future::Future<Output = bool>,
    {
        const FRAME_READS_PER_TICK: i64 = 16;
        // Rows become `none` through the stored-document walk; starting
        // before it finishes would let this cursor pass ids it later stamps.
        if !matches!(
            self.store
                .get_setting(keys::JOB_LUMINANCE_BACKFILL_DONE)
                .await,
            Ok(Some(_))
        ) {
            return;
        }
        match self
            .store
            .get_setting(keys::JOB_LUMINANCE_FRAME_BACKFILL_DONE)
            .await
        {
            Ok(None) => {}
            Ok(Some(_)) => return,
            Err(error) => {
                tracing::warn!(%error, "reading the luminance frame backfill stamp");
                return;
            }
        }
        let lease = match self
            .acquire_job("catalogue:luminance-frame".to_owned())
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "luminance frame backfill lease failed");
                return;
            }
        };
        let lost = lease.loss_token();
        let cursor_key = self.local_job_key(keys::JOB_LUMINANCE_FRAME_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let page = match self
            .store
            .files_without_luminance_facts(cursor, FRAME_READS_PER_TICK)
            .await
        {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, "listing files for the luminance frame backfill");
                let _ = lease.release().await;
                return;
            }
        };
        if page.is_empty() {
            match self
                .store
                .put_setting(keys::JOB_LUMINANCE_FRAME_BACKFILL_DONE, "1")
                .await
            {
                Ok(()) => tracing::info!("luminance frame backfill: complete"),
                Err(error) => {
                    tracing::warn!(%error, "stamping the luminance frame backfill complete");
                }
            }
            let _ = lease.release().await;
            return;
        }
        let roots: Vec<PathBuf> = match self.store.list_libraries().await {
            Ok(libraries) => libraries
                .into_iter()
                .flat_map(|library| library.paths)
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "listing library roots for the luminance frame backfill");
                let _ = lease.release().await;
                return;
            }
        };

        let mut walked = cursor;
        let mut tally = FrameBackfillTally::default();
        let mut before_first_refusal = None;
        for candidate in page {
            if lost.is_cancelled() {
                break;
            }
            if stored_luminance(&candidate.probe_json)
                .luminance_source
                .as_deref()
                != Some("none")
            {
                tally.ineligible += 1;
                walked = candidate.id;
                continue;
            }
            let path = PathBuf::from(&candidate.path);
            match scan::file_stat(&path).await {
                Ok(identity) if identity == (candidate.size, candidate.mtime) => {}
                Ok(_) => {
                    tally.changed += 1;
                    walked = candidate.id;
                    continue;
                }
                Err(error) => {
                    let root = roots
                        .iter()
                        .filter(|root| path.starts_with(root))
                        .max_by_key(|root| root.as_os_str().len());
                    // A row under no configured root belongs to no library
                    // the scanner walks; there is no mount to wait for.
                    let root_available = match root {
                        Some(root) => scan::library_root_available(root).await,
                        None => true,
                    };
                    if root_available {
                        tally.gone += 1;
                        walked = candidate.id;
                        continue;
                    }
                    tracing::info!(
                        file_id = candidate.id,
                        %error,
                        "luminance frame backfill: library root unavailable here; retrying next tick"
                    );
                    break;
                }
            }
            match read_frame(path).await {
                Ok(Some(frame)) => match self
                    .store
                    .set_file_frame_luminance(
                        &candidate,
                        frame.max_cll,
                        frame.max_fall,
                        frame.mastering_max_luminance,
                    )
                    .await
                {
                    Ok(true) => tally.observed += 1,
                    Ok(false) => tally.fenced += 1,
                    Err(error) => {
                        tracing::warn!(
                            file_id = candidate.id,
                            %error,
                            "writing first-frame luminance facts"
                        );
                        break;
                    }
                },
                Ok(None) => tally.absent += 1,
                Err(error) if error.is_file_verdict() => {
                    tally.refused += 1;
                    before_first_refusal.get_or_insert(walked);
                    tracing::warn!(
                        file_id = candidate.id,
                        %error,
                        "ffprobe refused the first frame; the row stays none"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        file_id = candidate.id,
                        %error,
                        "luminance frame backfill: ffprobe could not read here; retrying next tick"
                    );
                    break;
                }
            }
            walked = candidate.id;
        }
        walked = settle_refused_page(
            "luminance frame backfill",
            &tally,
            walked,
            before_first_refusal.unwrap_or(cursor),
            self_check,
        )
        .await;
        if walked > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &walked.to_string())
                .await
            {
                tracing::warn!(%error, "advancing the luminance frame backfill cursor");
            }
        }
        let _ = lease.release().await;
        tracing::info!(
            observed = tally.observed,
            absent = tally.absent,
            refused = tally.refused,
            fenced = tally.fenced,
            changed = tally.changed,
            gone = tally.gone,
            ineligible = tally.ineligible,
            cursor = walked,
            "luminance frame backfill: read first frames"
        );
    }

    /// Re-probe catalogued SDR titles whose stored stream document left the
    /// transfer to the bitstream (lab F4, 2026-10-11).
    ///
    /// The scanner now asks the first frame when a 10-bit-or-deeper HEVC, AV1
    /// or VP9 stream (or a BT.2020 one) reports its transfer `unknown`, so a
    /// PQ or HLG title whose HDR lives only in its VUI/SEI is catalogued HDR.
    /// A scan is incremental on size and mtime, so the rows catalogued before
    /// that are never probed again; this walk gives each of them the same
    /// bounded first-frame read, once per leaf probe pipeline digest. The
    /// child appears on the Activity page as "catalogue HDR re-probe of
    /// titles whose stream left the transfer unsignalled", with a stop beside
    /// it.
    async fn backfill_unsignalled_transfer(self: Arc<Self>) {
        // A node that cannot name its FFprobe cannot say which classification
        // it would complete the walk for.
        let Some(digest) = plurx_core::scan::probe::pipeline_digest().await else {
            return;
        };
        self.backfill_unsignalled_transfer_with(
            &digest,
            |path| async move {
                plurx_core::scan::probe::first_frame_document(&path, UNSIGNALLED_TRANSFER_PURPOSE)
                    .await
            },
            plurx_core::scan::probe::self_check,
        )
        .await;
    }

    /// [`Self::backfill_unsignalled_transfer`] with the pipeline digest and
    /// the frame read supplied, so the walk is testable without media.
    ///
    /// It follows [`Self::backfill_frame_luminance_with`] row for row: one
    /// cluster lease (`catalogue:unsignalled-transfer`) so one node walks; at
    /// most [`UNSIGNALLED_TRANSFER_READS_PER_TICK`] reads per tick; the cursor
    /// moves past a row only on evidence about that row (ineligible document,
    /// changed or vanished file, a frame read, or a refusal by an ffprobe that
    /// passes its own self-check) and stops where this node cannot read
    /// (library root unavailable, ffprobe could not run or finish or fails its
    /// self-check, a store write failed).
    /// A frame that says PQ or HLG writes the HDR columns fenced to the exact
    /// row snapshot and to the row still being SDR; any other frame leaves
    /// the row as it is. The completion stamp holds the digest it completed
    /// for, so another classification version or FFprobe build walks again.
    async fn backfill_unsignalled_transfer_with<F, Fut, G, GFut>(
        &self,
        digest: &str,
        read_frame: F,
        self_check: G,
    ) where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, plurx_core::error::ProbeError>>,
        G: FnOnce() -> GFut,
        GFut: std::future::Future<Output = bool>,
    {
        const PAGE: i64 = 256;
        match self
            .store
            .get_setting(keys::JOB_UNSIGNALLED_TRANSFER_BACKFILL_DONE)
            .await
        {
            Ok(Some(done)) if done == digest => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "reading the unsignalled-transfer re-probe stamp");
                return;
            }
        }
        let lease = match self
            .acquire_job("catalogue:unsignalled-transfer".to_owned())
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "unsignalled-transfer re-probe lease failed");
                return;
            }
        };
        let lost = lease.loss_token();
        let cursor_key = self.local_job_key(keys::JOB_UNSIGNALLED_TRANSFER_BACKFILL_CURSOR);
        let cursor = self
            .store
            .get_setting(&cursor_key)
            .await
            .ok()
            .flatten()
            .and_then(|value| {
                let (for_digest, id) = value.trim().rsplit_once(':')?;
                (for_digest == digest).then(|| id.parse::<i64>().ok())?
            })
            .unwrap_or(0);
        let page = match self
            .store
            .files_with_unsignalled_transfer(cursor, PAGE)
            .await
        {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, "listing files for the unsignalled-transfer re-probe");
                let _ = lease.release().await;
                return;
            }
        };
        if page.is_empty() {
            match self
                .store
                .put_setting(keys::JOB_UNSIGNALLED_TRANSFER_BACKFILL_DONE, digest)
                .await
            {
                Ok(()) => tracing::info!(digest, "unsignalled-transfer re-probe: complete"),
                Err(error) => {
                    tracing::warn!(%error, "stamping the unsignalled-transfer re-probe complete");
                }
            }
            let _ = lease.release().await;
            return;
        }
        let roots: Vec<PathBuf> = match self.store.list_libraries().await {
            Ok(libraries) => libraries
                .into_iter()
                .flat_map(|library| library.paths)
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "listing library roots for the unsignalled-transfer re-probe");
                let _ = lease.release().await;
                return;
            }
        };

        let mut walked = cursor;
        let mut tally = FrameBackfillTally::default();
        let mut before_first_refusal = None;
        let mut reads = 0usize;
        for candidate in page {
            if lost.is_cancelled() || reads >= UNSIGNALLED_TRANSFER_READS_PER_TICK {
                break;
            }
            if !plurx_core::scan::probe::stored_document_leaves_transfer_to_bitstream(
                &candidate.probe_json,
            ) {
                tally.ineligible += 1;
                walked = candidate.id;
                continue;
            }
            let path = PathBuf::from(&candidate.path);
            match scan::file_stat(&path).await {
                Ok(identity) if identity == (candidate.size, candidate.mtime) => {}
                Ok(_) => {
                    tally.changed += 1;
                    walked = candidate.id;
                    continue;
                }
                Err(error) => {
                    let root = roots
                        .iter()
                        .filter(|root| path.starts_with(root))
                        .max_by_key(|root| root.as_os_str().len());
                    let root_available = match root {
                        Some(root) => scan::library_root_available(root).await,
                        None => true,
                    };
                    if root_available {
                        tally.gone += 1;
                        walked = candidate.id;
                        continue;
                    }
                    tracing::info!(
                        file_id = candidate.id,
                        %error,
                        "unsignalled-transfer re-probe: library root unavailable here; retrying next tick"
                    );
                    break;
                }
            }
            reads += 1;
            match read_frame(path).await {
                Ok(frame) => {
                    match plurx_core::scan::probe::classify_frame_dynamic_range(
                        &candidate.probe_json,
                        &frame,
                    ) {
                        Some(observed) => match self
                            .store
                            .set_file_frame_dynamic_range(&candidate, &observed)
                            .await
                        {
                            Ok(true) => {
                                tally.observed += 1;
                                tracing::info!(
                                    file_id = candidate.id,
                                    hdr = %observed.hdr,
                                    "unsignalled-transfer re-probe: the first frame is HDR; recatalogued"
                                );
                            }
                            Ok(false) => tally.fenced += 1,
                            Err(error) => {
                                tracing::warn!(
                                    file_id = candidate.id,
                                    %error,
                                    "writing the first frame's HDR classification"
                                );
                                break;
                            }
                        },
                        None => tally.absent += 1,
                    }
                }
                Err(error) if error.is_file_verdict() => {
                    tally.refused += 1;
                    before_first_refusal.get_or_insert(walked);
                    tracing::warn!(
                        file_id = candidate.id,
                        %error,
                        "ffprobe refused the first frame; the row stays SDR"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        file_id = candidate.id,
                        %error,
                        "unsignalled-transfer re-probe: ffprobe could not read here; retrying next tick"
                    );
                    break;
                }
            }
            walked = candidate.id;
        }
        walked = settle_refused_page(
            "unsignalled-transfer re-probe",
            &tally,
            walked,
            before_first_refusal.unwrap_or(cursor),
            self_check,
        )
        .await;
        if walked > cursor {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &format!("{digest}:{walked}"))
                .await
            {
                tracing::warn!(%error, "advancing the unsignalled-transfer re-probe cursor");
            }
        }
        let _ = lease.release().await;
        tracing::info!(
            recatalogued = tally.observed,
            still_sdr = tally.absent,
            refused = tally.refused,
            fenced = tally.fenced,
            changed = tally.changed,
            gone = tally.gone,
            ineligible = tally.ineligible,
            cursor = walked,
            "unsignalled-transfer re-probe: read first frames"
        );
    }

    /// One bounded page of the subtitle-source store's sweep.
    ///
    /// Its own rule and cursor: `sweep_local_orphans` considers only index
    /// blobs, and the subtitle cache's LRU never sees this directory. A catalog
    /// read that fails stops the page and keeps the cursor, exactly as the
    /// index sweep does, so the same directories are asked about next tick.
    /// Remove the extracted subtitle sidecars of files that are gone — a
    /// deleted library's included. Bounded per tick like the other sweeps.
    async fn sweep_orphaned_subtitle_sidecars(&self, transcode: &TranscodeManager) {
        const SIDECAR_SWEEP_PAGE: usize = 512;
        let cursor = *self.subtitle_sidecar_sweep_cursor.lock().await;
        match crate::subtitles::sweep_orphaned_sidecars(
            self.store.as_ref(),
            transcode.subtitle_cache_dir(),
            cursor,
            SIDECAR_SWEEP_PAGE,
        )
        .await
        {
            Ok(outcome) => {
                if outcome.removed > 0 {
                    tracing::info!(
                        removed = outcome.removed,
                        "removed the subtitle sidecars of deleted files"
                    );
                }
                *self.subtitle_sidecar_sweep_cursor.lock().await = outcome.next;
            }
            Err(error) => tracing::warn!(%error, "sweeping orphaned subtitle sidecars"),
        }
    }

    async fn sweep_subtitle_sources(&self, transcode: &TranscodeManager) {
        let root = crate::subtitle_source::store_root(transcode.runtime_cache_dir());
        let cursor = self.subtitle_source_sweep_cursor.lock().await.clone();
        match crate::subtitle_source::sweep(
            self.store.as_ref(),
            self.coordinator.node_id(),
            &root,
            cursor.as_deref(),
            crate::subtitle_source::SWEEP_PAGE,
            crate::subtitle_source::MAX_STORE_BYTES,
        )
        .await
        {
            Ok(outcome) => {
                if outcome.removed + outcome.evicted + outcome.deferred + outcome.unnamed > 0 {
                    tracing::info!(
                        removed = outcome.removed,
                        evicted = outcome.evicted,
                        deferred = outcome.deferred,
                        unnamed = outcome.unnamed,
                        "reconciled stored subtitle sources"
                    );
                }
                if let Some(footprint) = outcome.footprint {
                    crate::subtitle_source::record_footprint(footprint);
                }
                *self.subtitle_source_sweep_cursor.lock().await = Some(outcome.next);
            }
            Err(error) => {
                tracing::warn!(%error, "reconciling stored subtitle sources");
            }
        }
        // Stages a crashed or killed pass left behind. A live pass's stage is
        // minutes old at most; an hour is a leftover.
        let stale = crate::subtitle_ride_along::sweep_stale_stages(
            &root,
            crate::subtitle_ride_along::STALE_STAGE_AGE,
        )
        .await;
        if stale > 0 {
            tracing::info!(stale, "removed abandoned PGS ride-along stages");
        }
    }

    /// Judge and publish a pass's PGS tracks — both paths, cluster and not.
    ///
    /// Called after the build future has returned, so a preemption can no
    /// longer race the verdict, and only after the pass's freshness checks:
    /// `source_still_matches` is the caller's reading of the held source
    /// (the cluster worker's `source_still_matches`, the non-cluster fence's
    /// `unchanged()`), and the catalog check here is the cluster worker's
    /// `still_current`. A pass that raced a rescan must not recreate a
    /// directory the store's sweep just removed. A failed publish is logged
    /// and leaves the store as it was, so the next pass rides again.
    pub(crate) async fn settle_ride_along(
        &self,
        file: &MediaFile,
        pending: crate::subtitle_ride_along::PendingRideAlong,
        source_still_matches: bool,
    ) -> Option<crate::subtitle_source::Manifest> {
        use crate::subtitle_ride_along::{record_discard, Discard};

        let file_id = pending.file_id();
        if !source_still_matches {
            tracing::info!(
                file_id,
                "the source moved during the pass; its PGS ride-along is discarded"
            );
            record_discard(Discard::SourceMoved);
            return None;
        }
        let still_current = self
            .store
            .get_file(file.id)
            .await
            .ok()
            .flatten()
            .is_some_and(|current| current.size == file.size && current.mtime == file.mtime);
        if !still_current {
            tracing::info!(
                file_id,
                "the catalog moved during the pass; its PGS ride-along is discarded"
            );
            record_discard(Discard::CatalogMoved);
            return None;
        }
        // The gate read the switch when the pass began. Turned off since, the
        // consumers already ignore the store; publishing now would add tracks
        // under a switch that says off, so the switch stops the pass it was
        // turned off during, not only the next one.
        if !crate::subtitle_source::enabled(self.store.as_ref()).await {
            tracing::info!(
                file_id,
                "stored subtitle tracks were turned off during the pass; its PGS ride-along is discarded"
            );
            record_discard(Discard::SwitchOff);
            return None;
        }
        // The publication row must carry a portable sampled digest. The
        // object version used by the fragment-index source memo includes this
        // host's device and inode, so it cannot identify bytes on a peer.
        let memo = match crate::fragment_index_cluster::inspect_source(file).await {
            Ok(version) => self
                .store
                .fragment_index_source(self.coordinator.node_id(), file.id, &version)
                .await
                .ok()
                .flatten(),
            Err(_) => None,
        };
        let attested = match crate::fragment_index_cluster::attest_source(
            self.coordinator.node_id(),
            file,
            memo.as_ref(),
            &|_| {},
        )
        .await
        {
            Ok(attested) => attested,
            Err(reason) => {
                tracing::warn!(file_id, %reason, "attesting subtitle ride-along source");
                record_discard(Discard::SourceMoved);
                return None;
            }
        };
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
            || crate::fragment_index_cluster::inspect_source(file)
                .await
                .ok()
                .as_deref()
                != Some(attested.observation.object_version.as_str())
        {
            record_discard(Discard::SourceMoved);
            return None;
        }
        // Persist the already verified observation, allowing read-only status
        // to bind publication receipts to this object without rehashing media.
        if let Err(error) = self
            .store
            .record_fragment_index_source(&attested.observation)
            .await
        {
            tracing::warn!(file_id, %error, "recording subtitle source observation");
            return None;
        }
        let harvest = pending.judge().await;
        let verdicts: Vec<_> = harvest
            .outcomes()
            .iter()
            .map(|outcome| (outcome.ordinal, outcome.verdict))
            .collect();
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
            || crate::fragment_index_cluster::inspect_source(file)
                .await
                .ok()
                .as_deref()
                != Some(attested.observation.object_version.as_str())
        {
            record_discard(Discard::SourceMoved);
            return None;
        }
        match harvest
            .publish_with_cluster(
                self.store.as_ref(),
                self.coordinator.node_id(),
                &attested.observation.source_sha256,
                None,
            )
            .await
        {
            Ok(manifest) => {
                tracing::info!(file_id, ?verdicts, "published the pass's PGS tracks");
                Some(manifest)
            }
            Err(error) => {
                tracing::warn!(file_id, %error, "publishing the pass's PGS tracks");
                None
            }
        }
    }

    /// Whether a Dolby Vision title scan for `file_id` should wait: within
    /// [`TITLE_SCAN_INDEX_WAIT`] of the file being noted, the
    /// node-local index pass has the file at the front of its queue and its
    /// ride-along reads the title's RPUs in the same read. Once the pass has
    /// examined the file, built or not, the scan no longer waits.
    #[cfg(test)]
    pub(crate) fn set_local_index_pass(&self, local: bool) {
        self.local_index_pass.store(local, Ordering::Relaxed);
    }

    pub(crate) fn title_scan_awaits_index_pass(&self, file_id: i64) -> bool {
        self.local_index_pass.load(Ordering::Relaxed)
            && self
                .fresh_index_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&file_id)
            && self
                .fresh_noted_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&file_id)
                .is_some_and(|noted| noted.elapsed() < TITLE_SCAN_INDEX_WAIT)
    }

    /// Offer an index pass's Dolby Vision title aggregate to the title's
    /// demand — both paths, cluster and not, after the caller's freshness
    /// checks. A refusal or failure leaves the demand to the title job.
    pub(crate) async fn settle_dv_title(
        &self,
        file: &MediaFile,
        pending: crate::fragindex::PendingDvTitle,
    ) {
        let output = plurx_core::store::dv_title_jobs::AnalysisTitleOutput {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            scanner_digest: pending.scanner_digest,
            stats: pending.stats,
        };
        match self.store.publish_dv_title_from_analysis(output).await {
            Ok(outcome) => tracing::info!(
                file_id = file.id,
                ?outcome,
                "the index pass offered its Dolby Vision title aggregate"
            ),
            Err(error) => tracing::warn!(
                file_id = file.id,
                %error,
                "publishing the index pass's Dolby Vision title aggregate"
            ),
        }
    }

    /// A pass has examined `file_id`: it no longer goes ahead of the backlog.
    fn settle_index_priority(&self, transcode: &TranscodeManager, file_id: i64) {
        transcode.settle_local_index_demand(file_id);
        self.fresh_index_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|fresh| *fresh != file_id);
        self.fresh_noted_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&file_id);
    }

    /// Queue the files a scan added or changed for the local index pass.
    ///
    /// Bounded like the demand list: the oldest fall back to the backlog,
    /// which is where they would have been without this. On a node whose
    /// pass runs the cluster queue instead, nothing consumes this and the
    /// bound is all that keeps it.
    pub(crate) fn note_fresh_index_files(&self, changed: Vec<i64>) {
        self.note_fresh_index_files_at(changed, std::time::Instant::now());
    }

    pub(crate) fn note_fresh_index_files_at(&self, changed: Vec<i64>, at: std::time::Instant) {
        const FRESH_INDEX_CAP: usize = 4_096;
        if changed.is_empty() {
            return;
        }
        let mut fresh = self
            .fresh_index_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut noted = self
            .fresh_noted_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh.retain(|file_id| !changed.contains(file_id));
        for file_id in &changed {
            noted.insert(*file_id, at);
        }
        fresh.extend(changed);
        while fresh.len() > FRESH_INDEX_CAP {
            if let Some(dropped) = fresh.pop_front() {
                noted.remove(&dropped);
            }
        }
    }

    async fn build_fragment_indexes(self: Arc<Self>, transcode: Arc<TranscodeManager>) {
        if self.indexing.swap(true, Ordering::Relaxed) {
            return;
        }
        let _running = IndexingGuard(Arc::clone(&self));
        // Before building anything, give back what belongs to files that are
        // gone. Node-local rows and a replicated `files` table share no
        // transaction, so a hook inside `delete_files` could only ever clean
        // the node that ran the delete -- and never a node that was down at
        // the time. Each node asking, on its own tick, converges everywhere.
        self.sweep_orphaned_vod_rows().await;
        // Node-local like the rows above, and on both the cluster and the
        // non-cluster path: every node reconciles its own store on its own
        // tick, whether or not it holds a discovery slot.
        self.sweep_subtitle_sources(&transcode).await;
        self.sweep_orphaned_subtitle_sidecars(&transcode).await;

        let cluster_cache_enabled =
            match self.store.get_setting(keys::VOD_INDEX_CLUSTER_CACHE).await {
                Ok(value) => setting_enabled(value),
                Err(error) => {
                    tracing::warn!(%error, "could not read the cluster fragment-index gate");
                    false
                }
            };
        self.local_index_pass
            .store(!cluster_cache_enabled, Ordering::Relaxed);
        if cluster_cache_enabled {
            if self.may_run_cluster_jobs().await {
                self.discover_subtitle_sources(Arc::clone(&transcode)).await;
                self.discover_cluster_fragment_indexes(transcode).await;
            }
            return;
        }

        let deadline = std::time::Instant::now() + INDEX_WINDOW;
        let have_dovi = transcode.dv_strippable();
        // The live answer, not the one this process booted with. The indexer
        // is what builds the converting identity a Dolby Vision client is
        // later served, so an operator who turns the conversion on in
        // Settings and gets a boot-latched `false` here has a switch that
        // says convert, a decision path that agrees, and nothing that ever
        // builds the index the player then waits on — forever, and only a
        // restart heals it.
        let convert = transcode.dv_convert_enabled().await;
        let runtime_cache = transcode.runtime_cache_dir().to_path_buf();
        let max_index_attempts = self.analysis_retry_policy().await.max_attempts;
        let node_id = self.coordinator.node_id().to_owned();
        let libraries = match self.store.list_libraries().await {
            Ok(libraries) => libraries,
            Err(error) => {
                tracing::warn!(error = %error, "fragment indexing could not list libraries");
                return;
            }
        };

        let mut paths = Vec::new();
        for library in libraries {
            match self.store.library_file_paths(library.id).await {
                Ok(library_paths) => paths.extend(library_paths),
                Err(error) => {
                    tracing::warn!(library = library.id, error = %error, "listing files to index");
                }
            }
        }
        let cursor_key = self.local_job_key(keys::JOB_VOD_INDEX_CURSOR);
        let cursor = self.job_stamp(&cursor_key).await;
        // Demand drives order, not throughput: the same bounds below apply,
        // a refused title and a new one are simply examined before the
        // backlog.
        let fresh: Vec<i64> = self
            .fresh_index_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect();
        let paths = prioritised_index_paths(paths, cursor, &transcode.local_index_demand(), &fresh);

        let mut built = 0usize;
        let mut built_file_ids = Vec::new();
        let mut attempted = 0usize;
        let mut last_examined = None;
        let mut cursor_at = None;
        // The heavy lane has one permit, and every other heavy worker
        // try-acquires it, so a waiter would win every turn. The pass waits
        // for the lane once; after that it takes the lane only when it is
        // free and nobody is queued for it, and otherwise ends here.
        let mut took_queued_turn = false;
        for (examined, (file_id, _path, backlog)) in paths.into_iter().enumerate() {
            // Both bounds stop different runaways: attempts bound the
            // whole-file reads a pass will start, while examined bounds a
            // fully indexed library's queries. Attempts count identities
            // rather than files now, and the check stays here, at the top of
            // the FILE loop, on purpose — see the identity loop below.
            if attempted >= INDEX_MAX_PER_PASS
                || examined >= INDEX_MAX_EXAMINED_PER_PASS
                || std::time::Instant::now() >= deadline
            {
                break;
            }
            // Foreground demand ends the pass; other background work (a
            // Dolby Vision title scan holding its admission) does not -- the
            // read below queues behind it for the heavy lane.
            if transcode.background_yield_reason().is_some() {
                return;
            }
            last_examined = Some(file_id);
            if backlog {
                cursor_at = Some(file_id);
            }
            let Ok(Some(file)) = self.store.get_file(file_id).await else {
                self.settle_index_priority(&transcode, file_id);
                continue;
            };
            if !crate::copyseg::supports(file.video_codec.as_deref()) {
                self.settle_index_priority(&transcode, file_id);
                continue;
            }
            let videos = match fragment_index_video_identities(
                self.store.as_ref(),
                &file,
                have_dovi,
                convert,
            )
            .await
            {
                Ok(videos) => videos,
                Err(error) => {
                    tracing::warn!(file_id, %error, "reading probe for fragment index");
                    self.settle_index_priority(&transcode, file_id);
                    continue;
                }
            };
            // One file, one index per pipeline a client can request.
            //
            // No pass bound is re-checked in here, and that is the whole
            // design: a file this loop starts, it finishes. The cursor is
            // stamped per file, and `ordered_index_paths` resumes strictly
            // *after* it, so a break in the middle of an identity set would
            // hand the rest of that set to the next full wrap of the library —
            // hours on a mid-size one, and never at all for a file whose first
            // identity reliably eats the whole window. Both bounds are checked
            // at the top of the file loop instead, which costs at most one
            // file's worth of overshoot per pass and buys the invariant that
            // makes the cursor safe.
            // Which of the file's identities still need a pass. Same checks,
            // same order as when each had a pass of its own.
            let mut missing = Vec::with_capacity(videos.len());
            for video in videos {
                let identity = crate::fragindex::identity_for(&file, video);
                match self.store.fragment_index(file_id, &identity).await {
                    Ok(Some(index)) => {
                        if !matches!(file.video_codec.as_deref(), Some("hevc" | "h265")) {
                            continue;
                        }
                        // Size/mtime alone miss inode/ctime-only replacement.
                        // The local pass must refresh such proofs even when
                        // the cluster request resolver is disabled. A current
                        // refusal is already analyzed and must not rescan.
                        match crate::fragment_index_cluster::inspect_source(&file).await {
                            Ok(object)
                                if index.promotion.hevc_configuration.as_ref().is_some_and(
                                    |proof| proof.current_on_node(&object, &node_id),
                                ) =>
                            {
                                continue
                            }
                            Ok(_) => {}
                            Err(error) => {
                                tracing::debug!(file_id, %error, "local HEVC index source unavailable");
                                break;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        // The sidecar is failing reads. Give up on the whole
                        // file rather than the identity: the alternative is to
                        // stop asking "is this already built?" and go straight
                        // to a multi-minute whole-file pass whose store write
                        // is about to fail too.
                        tracing::warn!(file_id, error = %error, "reading a fragment index");
                        break;
                    }
                }
                // What the last attempt learned. Without this the pass spends
                // the same whole-file read — up to thirty minutes of this
                // node's disk — on every wrap of the library, for a file that
                // already answered, while `vodserve` says `vod_index_pending`
                // for a title that may never have an index at all.
                //
                // Checked per identity rather than per file: a Dolby Vision
                // title holds up to three, and one refusing is not the others
                // refusing. It costs a cheap keyed read per identity examined,
                // which is what `ordered_index_paths`' own bound is for.
                let previous = match self.store.fragment_index_outcome(file_id, &identity).await {
                    Ok(Some(outcome)) if !outcome.is_due(clock_ms()) => continue,
                    Ok(previous) => previous,
                    Err(error) => {
                        tracing::warn!(file_id, error = %error, "reading a fragment index outcome");
                        break;
                    }
                };
                missing.push((video, identity, previous));
            }
            if missing.is_empty() {
                continue;
            }
            // One read of the source builds every missing identity (§5.4),
            // under this node's heavy-background lane, held for this file
            // only. The pass's first read waits for the lane in its FIFO queue
            // behind whoever holds it (a Dolby Vision title scan) instead of
            // abandoning the pass whenever the pool is not idle; later reads
            // in the same pass take it only if it is free with nobody queued
            // (a speech pass waiting goes first). Either is still refused when
            // foreground work holds the pool. Deliberately a return: it skips
            // the cursor stamp, so the file stays due rather than half done.
            let admission = if took_queued_turn {
                transcode.admit_fragment().await
            } else {
                took_queued_turn = true;
                transcode.admit_fragment_queued().await
            };
            let Some(admission) = admission else {
                tracing::info!(
                    file_id,
                    "local fragment index left the heavy lane to other work before its read"
                );
                return;
            };
            let admission = Arc::new(admission);
            attempted += missing.len();
            // Asked per pass, so the switch turning off stops the next
            // pass from riding without a restart.
            let ride_along = crate::subtitle_ride_along::RideAlongGate::open(
                Arc::clone(&self.store),
                &runtime_cache,
            )
            .await;
            // A progress row per identity, like the queue worker's, so Content
            // analysis shows this pass — what it keeps, and that its rows are
            // one read — on an install that never turns the queue on. Keyed by
            // file and pipeline, on this node; dropped when the pass ends.
            let progress_jobs = missing
                .iter()
                .map(|(_, identity, _)| local_index_job_id(file_id, identity))
                .collect::<Vec<_>>();
            let _progress = progress_jobs
                .iter()
                .map(|job| {
                    self.start_analysis_progress(
                        (job, &node_id),
                        file_id,
                        "fragment_index",
                        "fragment_index",
                        file.size.max(0) as u64,
                        file.duration_ms.unwrap_or(0),
                    )
                })
                .collect::<Vec<_>>();
            if progress_jobs.len() > 1 {
                for job in &progress_jobs {
                    self.set_analysis_shared_read(job, &node_id, progress_jobs.len());
                }
            }
            let reporters = progress_jobs
                .iter()
                .map(|job| self.index_pass_reporter(job, &node_id))
                .collect::<Vec<_>>();
            let reporter: crate::fragindex::SharedIndexProgress =
                Arc::new(move |progress: &crate::fragindex::PassProgress| {
                    for reporter in &reporters {
                        reporter(progress);
                    }
                });
            let Some(mut builds) = local_index_until_foreground(
                &transcode,
                &admission,
                crate::fragindex::build_riding_shared(
                    &file,
                    &missing
                        .iter()
                        .map(|(video, _, _)| *video)
                        .collect::<Vec<_>>(),
                    &runtime_cache,
                    missing
                        .iter()
                        .map(|(_, _, previous)| {
                            index_file_budget(file.duration_ms, file.size, previous.as_ref())
                        })
                        .max()
                        .unwrap_or_else(|| index_file_budget(file.duration_ms, file.size, None)),
                    ride_along.as_ref(),
                    Some(reporter),
                ),
            )
            .await
            else {
                tracing::info!(
                    file_id,
                    "local fragment index yielded to foreground playback"
                );
                return;
            };
            // §5.6 for the local pass: a shared read that failed as a pass —
            // the child failed or the budget ran out — says nothing about what
            // each identity does alone, so each of those is read again on its
            // own now rather than recorded against every identity at once.
            if missing.len() > 1
                && builds
                    .iter()
                    .any(|build| failed_as_a_shared_pass(&build.outcome))
            {
                tracing::info!(
                    file_id,
                    identities = missing.len(),
                    "a shared index read failed as a pass; building its identities one at a time"
                );
                for (slot, (video, _, previous)) in missing.iter().enumerate() {
                    if !builds
                        .get(slot)
                        .is_some_and(|build| failed_as_a_shared_pass(&build.outcome))
                    {
                        continue;
                    }
                    if transcode.fragment_must_yield(&admission) {
                        // As above: no cursor stamp, the file stays due.
                        return;
                    }
                    attempted += 1;
                    let Some(build) = local_index_until_foreground(
                        &transcode,
                        &admission,
                        crate::fragindex::build_riding(
                            &file,
                            *video,
                            &runtime_cache,
                            index_file_budget(file.duration_ms, file.size, previous.as_ref()),
                            None,
                            Some(self.index_pass_reporter(&progress_jobs[slot], &node_id)),
                        ),
                    )
                    .await
                    else {
                        tracing::info!(
                            file_id,
                            "local fragment index retry yielded to foreground playback"
                        );
                        return;
                    };
                    builds[slot] = build;
                }
            }
            for ((_, identity, _), build) in missing.iter().zip(builds) {
                let crate::fragindex::IndexBuild {
                    outcome,
                    ride_along: harvest,
                    dv_title,
                    source_unchanged,
                } = build;
                let refusal = match outcome {
                    crate::fragindex::IndexOutcome::Built(mut index) => {
                        if !source_unchanged {
                            continue;
                        }
                        if let Some(proof) = index.promotion.hevc_configuration.as_mut() {
                            proof.source_node_id = node_id.clone();
                        }
                        if let Err(error) = self.store.put_fragment_index(file_id, &index).await {
                            tracing::warn!(file_id, error = %error, "storing a fragment index");
                        } else {
                            built += 1;
                            if built_file_ids.last() != Some(&file_id) {
                                built_file_ids.push(file_id);
                            }
                        }
                        if let Some(pending) = harvest {
                            self.settle_ride_along(&file, pending, source_unchanged)
                                .await;
                        }
                        if let Some(pending) = dv_title {
                            self.settle_dv_title(&file, pending).await;
                        }
                        continue;
                    }
                    // These make an HLS title unavailable, so the reason stays
                    // in the ordinary operator log — and is now also recorded,
                    // so the next pass knows what this one found out.
                    crate::fragindex::IndexOutcome::Unsupported(reason) => {
                        tracing::warn!(file_id, "file cannot be indexed: {reason}");
                        (plurx_core::segplan::IndexRefusal::Unsupported, reason)
                    }
                    crate::fragindex::IndexOutcome::Failed(failure) => {
                        tracing::warn!(
                            file_id,
                            code = failure.code.as_str(),
                            rows = failure.rows,
                            reason = %failure.reason,
                            "fragment index failed"
                        );
                        let rows = u32::try_from(failure.rows).unwrap_or(u32::MAX);
                        match self
                            .store
                            .record_fragment_index_typed_outcome(
                                file_id,
                                identity,
                                failure.code,
                                failure.transient_allowlisted,
                                &failure.reason,
                                rows,
                                &failure.diagnostic,
                                max_index_attempts,
                            )
                            .await
                        {
                            Ok(recorded) => tracing::info!(
                                file_id,
                                code = failure.code.as_str(),
                                attempts = recorded.attempts,
                                retryable = recorded.typed_retryable.unwrap_or(false),
                                retry_deadline_ms = recorded.retry_deadline_ms,
                                "recorded typed fragment-index failure"
                            ),
                            Err(error) => tracing::warn!(
                                file_id,
                                error = %error,
                                "recording typed fragment-index failure"
                            ),
                        }
                        continue;
                    }
                };
                match self
                    .store
                    .record_fragment_index_outcome(file_id, identity, refusal.0, &refusal.1)
                    .await
                {
                    Ok(recorded) => tracing::info!(
                        file_id,
                        outcome = refusal.0.code(),
                        attempts = recorded.attempts,
                        retry_in_s = recorded
                            .next_attempt_at_ms
                            .saturating_sub(clock_ms())
                            .max(0)
                            / 1_000,
                        terminal = !refusal.0.is_retryable(),
                        "recorded why this pipeline has no fragment index"
                    ),
                    Err(error) => tracing::warn!(
                        file_id,
                        error = %error,
                        "recording a fragment index outcome"
                    ),
                }
            }
            self.settle_index_priority(&transcode, file_id);
        }
        // The scheduler can tick before a first-run library has been created
        // or before its initial scan has published any files. Stamping that
        // empty pass would make a default-on VOD server refuse every new title
        // for a full cadence. A pass that examined media is complete even when
        // everything was already current or unsupported; an empty or
        // preempted pass remains due for the next minute tick.
        if let Some(file_id) = cursor_at {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &file_id.to_string())
                .await
            {
                tracing::warn!(error = %error, key = cursor_key, "recording VOD index cursor failed");
            }
        }
        if last_examined.is_some() {
            self.stamp_local(keys::JOB_LAST_VOD_INDEX).await;
        }
        if attempted > 0 {
            tracing::info!(
                attempted,
                built,
                built_files = ?built_file_ids,
                "fragment indexing pass finished"
            );
        }
    }

    /// Fill cold subtitle sources only while this node is idle. The cluster
    /// lease fences competing discovery passes; the request store still owns
    /// exact per-stamp de-duplication with a viewer's foreground enqueue.
    pub(crate) async fn discover_subtitle_sources(
        self: &Arc<Self>,
        transcode: Arc<TranscodeManager>,
    ) {
        const BACKFILL_PER_TICK: i64 = 8;
        if !self.subtitle_source_queue_enabled().await
            || !setting_enabled(
                self.store
                    .get_setting(keys::SUBTITLE_BACKFILL)
                    .await
                    .unwrap_or(None),
            )
            || !transcode.pretranscode_worker_idle()
            || !self.may_run_cluster_jobs().await
        {
            return;
        }
        let lease = match self
            .acquire_job("media:subtitle-source:backfill".to_owned())
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "acquiring subtitle-source backfill lease");
                return;
            }
        };
        let lost = lease.loss_token();
        let now = clock_ms();
        let candidates = self
            .store
            .subtitle_backfill_candidates(i64::MAX, i64::MAX, now, BACKFILL_PER_TICK)
            .await;
        let pipeline_version = subtitle_source_pipeline_version().await;
        let mut enqueued = 0_i64;
        let mut considered = 0_i64;
        match candidates {
            Ok(candidates) => {
                for candidate in candidates {
                    if lost.is_cancelled() || !transcode.pretranscode_worker_idle() {
                        break;
                    }
                    considered += 1;
                    let file = match self.store.get_file(candidate.file_id).await {
                        Ok(Some(file))
                            if file.size == candidate.source_size
                                && file.mtime == candidate.source_mtime =>
                        {
                            file
                        }
                        _ => continue,
                    };
                    if crate::subtitle_source::is_mpegts_container(&file) {
                        continue;
                    }
                    let stamp = SubtitleSourceStamp {
                        file_id: file.id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                        pipeline_version: pipeline_version.clone(),
                    };
                    match self
                        .store
                        .enqueue_or_promote_subtitle_source(&stamp, "normal", clock_ms())
                        .await
                    {
                        Ok(Some(request)) if request.state == "queued" => {
                            enqueued += 1;
                            self.subtitle_backfill_enqueued_process
                                .fetch_add(1, Ordering::Relaxed);
                            crate::telemetry::record_subtitle_source(
                                crate::telemetry::SubtitleSourceMetric::RequestBackground,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            file_id = file.id,
                            %error,
                            "enqueuing subtitle-source backfill"
                        ),
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "discovering subtitle-source backfill"),
        }
        if let Err(error) = lease.release().await {
            tracing::warn!(%error, "releasing subtitle-source backfill lease");
        }
        if considered > 0 {
            tracing::info!(
                considered,
                enqueued,
                "subtitle-source backfill pass finished"
            );
        }
    }

    async fn discover_cluster_fragment_indexes(self: &Arc<Self>, transcode: Arc<TranscodeManager>) {
        let mut permit = None;
        for slot in 0..2 {
            match self
                .acquire_job(format!("media:fragment-index:{slot}"))
                .await
            {
                Ok(Some(lease)) => {
                    permit = Some(lease);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(slot, %error, "acquiring fragment-index discovery slot");
                }
            }
        }
        let Some(permit) = permit else { return };
        let permit_lost = permit.loss_token();
        self.discover_cluster_fragment_indexes_with_permit(transcode, &permit_lost)
            .await;
        if let Err(error) = permit.release().await {
            tracing::warn!(%error, "releasing fragment-index discovery slot");
        }
    }

    /// Catalog retention consumes its own success interval, including no-op
    /// passes. The shared lease/stamp coordinates both discovery permits and
    /// all voters; a busy discovery must not invent cursor progress.
    async fn prune_fragment_index_catalog(
        &self,
        cache_root: &std::path::Path,
        permit_lost: &tokio_util::sync::CancellationToken,
    ) {
        const CLEANUP_MINS: i64 = 15;
        const RETAIN_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let key = keys::JOB_LAST_FRAGMENT_INDEX_CLEANUP;
        if permit_lost.is_cancelled()
            || !crate::schedule::due(now(), self.job_stamp(key).await, CLEANUP_MINS)
        {
            return;
        }
        let lease = match self
            .acquire_job("media:fragment-index:cleanup".to_owned())
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "acquiring fragment-index cleanup lease");
                return;
            }
        };
        // Another permit may have completed while this one waited for a lease.
        if !permit_lost.is_cancelled()
            && !lease.loss_token().is_cancelled()
            && crate::schedule::due(now(), self.job_stamp(key).await, CLEANUP_MINS)
        {
            match self
                .store
                .prune_cluster_fragment_indexes(clock_ms().saturating_sub(RETAIN_MS), 128)
                .await
            {
                Ok(cache_keys) => {
                    // Stamp success before local I/O. Zero removed rows still
                    // used the maintenance interval; discovery stays due.
                    if !lease.loss_token().is_cancelled() {
                        self.stamp(key, &lease.publisher(self.store.as_ref())).await;
                    }
                    for cache_key in cache_keys {
                        crate::fragment_index_cluster::remove_local_blob(cache_root, &cache_key)
                            .await;
                    }
                }
                Err(error) => tracing::warn!(%error, "pruning fragment-index catalog generations"),
            }
        }
        if let Err(error) = lease.release().await {
            tracing::warn!(%error, "releasing fragment-index cleanup lease");
        }
    }

    async fn discover_cluster_fragment_indexes_with_permit(
        self: &Arc<Self>,
        transcode: Arc<TranscodeManager>,
        permit_lost: &tokio_util::sync::CancellationToken,
    ) {
        let node_id = self.coordinator.node_id().to_owned();
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            tracing::warn!(
                "cluster fragment indexing requires a daemon restart after engine change"
            );
            return;
        }
        let cache_root = crate::fragment_index_cluster::cache_root(transcode.runtime_cache_dir());
        self.prune_fragment_index_catalog(&cache_root, permit_lost)
            .await;
        let sweep_cursor = self.fragment_index_sweep_cursor.lock().await.clone();
        match crate::fragment_index_cluster::sweep_local_orphans(
            self.store.as_ref(),
            &cache_root,
            sweep_cursor.as_deref(),
            256,
        )
        .await
        {
            Ok((removed, next)) => {
                *self.fragment_index_sweep_cursor.lock().await = Some(next);
                if removed > 0 {
                    tracing::info!(removed, "removed orphaned fragment-index cache files");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "reconciling local fragment-index blobs");
            }
        }
        let libraries = match self.store.list_libraries().await {
            Ok(libraries) => libraries,
            Err(error) => {
                tracing::warn!(%error, "cluster fragment indexing could not list libraries");
                return;
            }
        };
        let mut paths = Vec::new();
        for library in libraries {
            match self.store.library_file_paths(library.id).await {
                Ok(library_paths) => paths.extend(library_paths),
                Err(error) => tracing::warn!(
                    library = library.id,
                    %error,
                    "listing files for cluster fragment indexing"
                ),
            }
        }
        let cursor_key = self.local_job_key(keys::JOB_VOD_INDEX_CURSOR);
        let cursor = self.job_stamp(&cursor_key).await;
        let paths = ordered_cluster_index_paths(paths, cursor, &node_id);
        let deadline = std::time::Instant::now() + INDEX_WINDOW;
        let mut attempted = 0_usize;
        let mut enqueued = 0_usize;
        let mut last_examined = None;

        for (examined, (file_id, _)) in paths.into_iter().enumerate() {
            if permit_lost.is_cancelled()
                || attempted >= INDEX_MAX_PER_PASS
                || examined >= INDEX_MAX_EXAMINED_PER_PASS
                || std::time::Instant::now() >= deadline
            {
                break;
            }
            if !transcode.pretranscode_worker_idle() {
                break;
            }
            last_examined = Some(file_id);
            let Ok(Some(file)) = self.store.get_file(file_id).await else {
                continue;
            };
            if !crate::copyseg::supports(file.video_codec.as_deref()) {
                continue;
            }
            attempted += 1;
            // One request per copy-video identity this file lacks, rather
            // than one request for whichever identity happened to be first.
            //
            // The old shape is why no converting Dolby Vision index exists
            // anywhere on the fleet: a request resolved the first identity
            // missing an index, and its row then became the dedup tombstone
            // that stopped discovery ever asking for the others. A P7 title
            // got its stripped identity and never its converting one, so
            // only a play attempt or an admin request could reach it.
            //
            // `attempted` still counts files, so the per-pass budget means
            // what it always meant: this widens what one file asks for, not
            // how many files a pass walks.
            let identities = match fragment_index_video_identity_options(
                self.store.as_ref(),
                &file,
                transcode.dv_strippable(),
                transcode.dv_convert_enabled().await,
            )
            .await
            {
                Ok(identities) => identities,
                Err(error) => {
                    tracing::warn!(file_id, %error, "listing copy-video identities to index");
                    continue;
                }
            };
            for (video, fingerprint) in identities {
                if self
                    .store
                    .fragment_index(file_id, &crate::fragindex::identity_for(&file, video))
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }
                match self
                    .request_file_analysis_for_identity(
                        file_id,
                        false,
                        "fragment_index",
                        "background",
                        &fingerprint,
                    )
                    .await
                {
                    Ok((_, false)) => enqueued += 1,
                    Ok((_, true)) => {}
                    Err(error) => {
                        tracing::warn!(file_id, %error, "queueing background fragment analysis");
                    }
                }
            }
        }

        if let Some(file_id) = last_examined {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &file_id.to_string())
                .await
            {
                tracing::warn!(%error, key = cursor_key, "recording cluster index cursor failed");
            }
            self.stamp_local(keys::JOB_LAST_VOD_INDEX).await;
        }

        if attempted > 0 {
            tracing::info!(
                attempted,
                enqueued,
                "cluster fragment-index discovery pass finished"
            );
        }
    }

    pub(crate) async fn work_cluster_fragment_index_queue(
        self: Arc<Self>,
        transcode: Arc<TranscodeManager>,
    ) {
        if !self
            .job_authority
            .may_execute_job(plurx_core::store::background_jobs::JobKind::FragmentIndexBuild)
            .await
        {
            return;
        }
        // A source attestation can take minutes. It owns its own single-consumer
        // fence, leaving the artifact and delivery pass available to another
        // wake while the analysis reader is active. Store claims retain their
        // durable owner/fence and do not depend on this local pass guard.
        // Settling, pruning and artifact delivery are cluster-wide duties
        // and stay with the voters. Resolving is not: a learner allowed to
        // execute this job (above) resolves the requests targeted at itself,
        // which no other node ever claims.
        let may_produce = self.may_run_cluster_jobs().await;
        let analysis = if !self.cluster_analysis_working.swap(true, Ordering::AcqRel) {
            let analysis_state = Arc::clone(&self);
            let analysis_transcode = Arc::clone(&transcode);
            Some(tokio::spawn(async move {
                let _analysis_guard = ClusterAnalysisWorkingGuard(Arc::clone(&analysis_state));
                analysis_state
                    .resolve_analysis_requests(analysis_transcode)
                    .await;
            }))
        } else {
            None
        };
        if self.cluster_index_working.swap(true, Ordering::AcqRel) {
            if let Some(analysis) = analysis {
                let _ = analysis.await;
            }
            return;
        }
        let _guard = ClusterIndexWorkingGuard(Arc::clone(&self));
        let fragment_engine_current = crate::ffmpeg::fragment_index_engine_is_current().await;

        let now = clock_ms();
        if may_produce {
            if let Err(error) = self.store.settle_analysis_requests(now).await {
                tracing::warn!(%error, "settling analysis requests");
            }
        }
        const ANALYSIS_PRUNE_INTERVAL_MS: i64 = 60 * 60 * 1_000;
        const ANALYSIS_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let last_prune = self.last_analysis_prune_ms.load(Ordering::Relaxed);
        if may_produce
            && now.saturating_sub(last_prune) >= ANALYSIS_PRUNE_INTERVAL_MS
            && self
                .last_analysis_prune_ms
                .compare_exchange(last_prune, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            match self
                .store
                .prune_analysis_requests(now.saturating_sub(ANALYSIS_RETENTION_MS), 256)
                .await
            {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "pruned analysis request history")
                }
                Ok(_) => {}
                Err(error) => {
                    self.last_analysis_prune_ms.store(0, Ordering::Relaxed);
                    tracing::warn!(%error, "pruning analysis request history");
                }
            }
        }
        if !fragment_engine_current {
            drop(_guard);
            if let Some(analysis) = analysis {
                let _ = analysis.await;
            }
            return;
        }

        if may_produce {
            self.enqueue_artifact_deliveries().await;
        }
        let built = self.drain_cluster_fragment_index_slot(transcode).await;
        if built > 0 {
            tracing::info!(built, "cluster fragment-index queue pass finished");
        }
        drop(_guard);
        if let Some(analysis) = analysis {
            let _ = analysis.await;
        }
    }

    pub(crate) fn execution_authority(&self) -> Arc<dyn ClusterJobAuthority> {
        Arc::clone(&self.job_authority)
    }

    pub(crate) async fn enqueue_artifact_deliveries(&self) {
        match self.store.artifact_repairs(true).await {
            Ok(repairs) => {
                for repair in repairs.into_iter().take(16) {
                    if let Err(error) = self.store.enqueue_artifact_repair(repair, clock_ms()).await
                    {
                        tracing::warn!(%error, "admitting finite artifact repair");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "reading artifact repair plans"),
        }
        match self.store.delivery_intents(clock_ms()).await {
            Ok(intents) => {
                for intent in intents {
                    if let Err(error) = self.store.enqueue_delivery(intent, clock_ms()).await {
                        tracing::warn!(%error, "admitting artifact delivery");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "reading artifact delivery obligations"),
        }
    }

    async fn resolve_analysis_requests(self: &Arc<Self>, transcode: Arc<TranscodeManager>) {
        const MAX_REQUESTS_PER_PASS: usize = 2;
        /// Hard background I/O budget. HEVC copy proof hashes the full source;
        /// other consumers still sample. Large/slow files may exhaust this
        /// budget and remain unverified, with bounded charged retries.
        const ATTEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

        if !self.cluster_fragment_index_enabled().await {
            return;
        }
        let node_id = self.coordinator.node_id().to_owned();
        let retry_policy = self.analysis_retry_policy().await;
        let engine_sha256 = crate::ffmpeg::fragment_index_engine_digest().await;
        let have_dovi = transcode.dv_strippable();
        // A learner takes only what names it; untargeted requests any node
        // may take stay with the voters.
        let targeted_only = !self.may_run_cluster_jobs().await;
        for _ in 0..MAX_REQUESTS_PER_PASS {
            if !self.cluster_fragment_index_enabled().await {
                break;
            }
            let now = clock_ms();
            let request = match self
                .store
                .claim_analysis_request_for_capacity(
                    &node_id,
                    Some(&engine_sha256),
                    now,
                    now.saturating_add(retry_policy.lease_ms),
                    !transcode.pretranscode_worker_idle(),
                    targeted_only,
                )
                .await
            {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "claiming an analysis request");
                    break;
                }
            };
            let _progress = self.start_analysis_progress(
                (&request.request_id, &request.target_node_id),
                request.file_id,
                &request.component,
                "probing",
                request.source_size.max(0) as u64,
                0,
            );

            let stop = tokio_util::sync::CancellationToken::new();
            let lost = tokio_util::sync::CancellationToken::new();
            let heartbeat = {
                let store = Arc::clone(&self.store);
                let request_id = request.request_id.clone();
                let node_id = node_id.clone();
                let metrics = Arc::clone(&self.analysis_metrics);
                let stop = stop.clone();
                let lost = lost.clone();
                let fence = request.fence;
                let beat = LeaseHeartbeat {
                    queue: "analysis-request",
                    row: request.request_id.clone(),
                    fence: request.fence,
                    attempts: request.attempts,
                    known_expiry_ms: request.lease_expires_ms,
                    lease_ms: retry_policy.lease_ms,
                    renew_every: retry_policy.renew_every(),
                    metrics,
                    stop,
                    lost,
                };
                tokio::spawn(async move {
                    beat.run(move |now, expires_at| {
                        let store = Arc::clone(&store);
                        let request_id = request_id.clone();
                        let node_id = node_id.clone();
                        async move {
                            store
                                .renew_analysis_request(
                                    &request_id,
                                    &node_id,
                                    fence,
                                    now,
                                    expires_at,
                                )
                                .await
                        }
                    })
                    .await;
                })
            };

            let outcome = self
                .resolve_analysis_request(
                    &request,
                    &node_id,
                    &engine_sha256,
                    have_dovi,
                    transcode.as_ref(),
                    &stop,
                    &lost,
                    ATTEST_TIMEOUT,
                )
                .await;
            // Before the outcome write, for the same reason the job runner
            // retires first: a tick landing between the write and the cancel
            // renews a row the write has already settled.
            stop.cancel();
            let _ = heartbeat.await;
            if let Err(resolution_error) = outcome {
                let now = clock_ms();
                match resolution_error {
                    AnalysisResolutionError::ClaimLost => {}
                    AnalysisResolutionError::Retry {
                        code,
                        charge_attempt,
                    } => {
                        let delay_ms =
                            retry_policy.backoff_ms(&request.request_id, request.attempts.max(1));
                        match self
                            .store
                            .retry_analysis_request(
                                &request,
                                code,
                                now,
                                now.saturating_add(delay_ms),
                                charge_attempt,
                            )
                            .await
                        {
                            Ok(true) => {
                                // Best effort: the retry transition already
                                // committed; this row is diagnostic history.
                                crate::store_result::observe(
                                    crate::store_result::Operation::RecordAnalysisRetryWaitPhase,
                                    crate::store_result::Discard::BestEffort,
                                    self.store
                                        .record_analysis_request_phase(
                                            &request,
                                            "retry_wait",
                                            Some(code),
                                            now,
                                        )
                                        .await,
                                );
                            }
                            Ok(false) => {}
                            Err(error) => tracing::warn!(
                                request_id = request.request_id,
                                %error,
                                "queueing an analysis request retry"
                            ),
                        }
                    }
                    AnalysisResolutionError::Terminal(code) => {
                        match self
                            .store
                            .fail_analysis_request(
                                &request.request_id,
                                &node_id,
                                request.fence,
                                code,
                                now,
                            )
                            .await
                        {
                            Ok(true) => {
                                // Best effort: the failed terminal state is
                                // durable already; this row explains it.
                                crate::store_result::observe(
                                    crate::store_result::Operation::RecordAnalysisFailedPhase,
                                    crate::store_result::Discard::BestEffort,
                                    self.store
                                        .record_analysis_request_phase(
                                            &request,
                                            "failed",
                                            Some(code),
                                            now,
                                        )
                                        .await,
                                );
                            }
                            Ok(false) => {}
                            Err(error) => tracing::warn!(
                                request_id = request.request_id,
                                %error,
                                "failing an analysis request"
                            ),
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_subtitle_source_request(
        &self,
        request: &AnalysisRequest,
        node_id: &str,
        file: &MediaFile,
        runtime_cache: &Path,
        preempt_when_busy: Option<&TranscodeManager>,
        fence: &crate::background_jobs::JobFence,
        lost: &CancellationToken,
    ) -> Result<(), AnalysisResolutionError> {
        if !request.video_identity.is_empty()
            || !request.target_node_id.is_empty()
            || request.pipeline_version != subtitle_source_pipeline_version().await
        {
            return Err(AnalysisResolutionError::Terminal(
                "pipeline_version_unavailable",
            ));
        }
        if !self.subtitle_source_queue_enabled().await
            || !crate::subtitle_source::enabled(self.store.as_ref()).await
        {
            return Err(AnalysisResolutionError::Retry {
                code: "pipeline_version_unavailable",
                charge_attempt: false,
            });
        }
        if crate::subtitle_source::is_mpegts_container(file) {
            return Err(AnalysisResolutionError::Terminal("stored_probe_invalid"));
        }
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        self.update_analysis_progress(
            &request.request_id,
            &request.target_node_id,
            "hashing",
            0,
            0,
            0,
        );
        if !self
            .store
            .record_analysis_request_phase(request, "hashing", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        let memo = match crate::fragment_index_cluster::inspect_source(file).await {
            Ok(version) => self
                .store
                .fragment_index_source(node_id, file.id, &version)
                .await
                .ok()
                .flatten(),
            Err(_) => None,
        };
        let progress = |bytes| {
            self.update_analysis_progress(
                &request.request_id,
                &request.target_node_id,
                "hashing",
                bytes,
                0,
                0,
            );
        };
        let attested = tokio::select! {
            () = lost.cancelled() => return Err(AnalysisResolutionError::ClaimLost),
            result = tokio::time::timeout(
                Duration::from_secs(10 * 60),
                crate::fragment_index_cluster::attest_source(node_id, file, memo.as_ref(), &progress)
            ) => match result {
                Ok(Ok(attested)) => attested,
                Ok(Err(_)) => return Err(AnalysisResolutionError::Retry {
                    code: "source_attestation_failed",
                    charge_attempt: true,
                }),
                Err(_) => return Err(AnalysisResolutionError::Retry {
                    code: "source_attestation_timeout",
                    charge_attempt: true,
                }),
            },
        };
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
            || crate::fragment_index_cluster::inspect_source(file)
                .await
                .ok()
                .as_deref()
                != Some(attested.observation.object_version.as_str())
        {
            return Err(AnalysisResolutionError::Terminal("source_superseded"));
        }
        let raw = crate::ffmpeg::held_source_index_probe_json(
            &attested.handle,
            Duration::from_secs(30),
            Some(lost),
        )
        .await;
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        let raw = raw.map_err(|_| AnalysisResolutionError::Terminal("stored_probe_invalid"))?;
        self.store
            .record_fragment_index_source(&attested.observation)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_catalog_write_failed",
                charge_attempt: true,
            })?;
        let tracks = crate::subtitle_ride_along::eligible_tracks_from_probe(&raw);
        let rows = self
            .store
            .list_subtitle_source_publications(file.id, file.size, file.mtime)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_catalog_read_failed",
                charge_attempt: true,
            })?;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|row| row.source_attestation == attested.observation.source_sha256)
            .collect();
        let result_key = format!(
            "subtitle-source:{}:{}:{}:{}",
            file.id, file.size, file.mtime, request.pipeline_version
        );
        if subtitle_source_covered(&tracks, &rows) {
            tracing::info!(
                file_id = file.id,
                "subtitle_source request already covered; no source read"
            );
            self.complete_subtitle_source_request(request, &result_key, fence)
                .await?;
            return Ok(());
        }
        let gate =
            crate::subtitle_ride_along::RideAlongGate::open(Arc::clone(&self.store), runtime_cache)
                .await
                .ok_or(AnalysisResolutionError::Retry {
                    code: "source_unavailable",
                    charge_attempt: true,
                })?;
        let plan =
            crate::subtitle_ride_along::plan_tracks(&gate, file.id, &attested.handle, &tracks)
                .await;
        let Some(plan) = plan else {
            if tracks.is_empty() {
                self.complete_subtitle_source_request(request, &result_key, fence)
                    .await?;
                return Ok(());
            }
            // A local pass may have committed its manifest before the
            // replicated publication writes, or a migration may introduce
            // the publication table after that manifest. Reassert only
            // verified, locally extracted rows instead of rereading media.
            let root = crate::subtitle_source::store_root(runtime_cache);
            let _ = crate::subtitle_ride_along::publish_existing_manifest_rows(
                self.store.as_ref(),
                node_id,
                &root,
                file.id,
                &attested.handle,
                &attested.observation.source_sha256,
                Some((fence, request)),
            )
            .await;
            if let Ok(rows) = self
                .store
                .list_subtitle_source_publications(file.id, file.size, file.mtime)
                .await
            {
                let rows: Vec<_> = rows
                    .into_iter()
                    .filter(|row| row.source_attestation == attested.observation.source_sha256)
                    .collect();
                if subtitle_source_covered(&tracks, &rows) {
                    self.complete_subtitle_source_request(request, &result_key, fence)
                        .await?;
                    return Ok(());
                }
            }
            // A concurrently running local pass may hold the file claim;
            // retry after its ordinary backoff. An old failed ride latch or
            // an unreadable manifest is also possible, so charge the attempt
            // rather than keeping an immortal queued row.
            return Err(AnalysisResolutionError::Retry {
                code: "queue_full_or_busy",
                charge_attempt: false,
            });
        };
        let stage = plan.stage().to_owned();
        let track_count = plan.tracks().len();
        let pgs_count = tracks
            .iter()
            .filter(|track| {
                plan.tracks().contains(&track.ordinal)
                    && track.kind == crate::subtitle_ride_along::ProbedKind::Pgs
            })
            .count();
        let text_count = track_count.saturating_sub(pgs_count);
        self.update_analysis_subtitle_tracks(
            &request.request_id,
            &request.target_node_id,
            pgs_count,
            text_count,
            0,
        );
        self.update_analysis_progress(
            &request.request_id,
            &request.target_node_id,
            "subtitle_source",
            0,
            0,
            0,
        );
        if !self
            .store
            .record_analysis_request_phase(request, "staged", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        let meter = || {
            self.update_analysis_subtitle_tracks(
                &request.request_id,
                &request.target_node_id,
                pgs_count,
                text_count,
                crate::subtitle_ride_along::stage_bytes(&stage),
            );
        };
        let started = Instant::now();
        tracing::info!(
            file_id = file.id,
            node_id,
            pgs_count,
            text_count,
            "subtitle_source job reading source"
        );
        let pending = run_subtitle_source_pass(
            plan,
            &attested.handle,
            runtime_cache,
            lost,
            if request.priority == "foreground" {
                None
            } else {
                preempt_when_busy
            },
            &meter,
        )
        .await?;
        crate::telemetry::record_subtitle_source(crate::telemetry::SubtitleSourceMetric::JobBytes(
            file.size.max(0) as u64,
        ));
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
            || crate::fragment_index_cluster::inspect_source(file)
                .await
                .ok()
                .as_deref()
                != Some(attested.observation.object_version.as_str())
        {
            return Err(AnalysisResolutionError::Terminal("source_superseded"));
        }
        let current = self
            .store
            .get_file(file.id)
            .await
            .ok()
            .flatten()
            .is_some_and(|current| current.size == file.size && current.mtime == file.mtime);
        if !current {
            return Err(AnalysisResolutionError::Terminal("source_superseded"));
        }
        if !self
            .store
            .record_analysis_request_phase(request, "publishing", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        let harvest = pending.judge().await;
        let mut representation_count = 0_usize;
        for outcome in harvest.outcomes() {
            for representation in &outcome.representations {
                representation_count += 1;
                let event = match representation.verdict {
                    crate::subtitle_source::Verdict::Kept => {
                        crate::telemetry::SubtitleSourceMetric::VerdictKept
                    }
                    crate::subtitle_source::Verdict::Empty => {
                        crate::telemetry::SubtitleSourceMetric::VerdictEmpty
                    }
                    crate::subtitle_source::Verdict::Malformed => {
                        crate::telemetry::SubtitleSourceMetric::VerdictMalformed
                    }
                    crate::subtitle_source::Verdict::Transient => {
                        crate::telemetry::SubtitleSourceMetric::VerdictTransient
                    }
                };
                crate::telemetry::record_subtitle_source(event);
            }
        }
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
            || crate::fragment_index_cluster::inspect_source(file)
                .await
                .ok()
                .as_deref()
                != Some(attested.observation.object_version.as_str())
        {
            return Err(AnalysisResolutionError::Terminal("source_superseded"));
        }
        let published = harvest
            .publish_guarded_worker(
                self.store.as_ref(),
                node_id,
                &attested.observation.source_sha256,
                lost,
                request,
                fence,
            )
            .await;
        let receipt = match published {
            Ok(receipt) => receipt,
            Err(_) if lost.is_cancelled() => return Err(AnalysisResolutionError::ClaimLost),
            Err(_) => {
                return Err(AnalysisResolutionError::Retry {
                    code: "queue_write_failed",
                    charge_attempt: true,
                });
            }
        };
        if lost.is_cancelled() {
            if let Err(error) = receipt.rollback(self.store.as_ref()).await {
                tracing::error!(file_id = file.id, %error, "compensating lost subtitle-source publication");
            }
            return Err(AnalysisResolutionError::ClaimLost);
        }
        if !self
            .store
            .record_analysis_request_phase(request, "publishing", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            if let Err(error) = receipt.rollback(self.store.as_ref()).await {
                tracing::error!(file_id = file.id, %error, "compensating lost subtitle-source publication");
            }
            return Err(AnalysisResolutionError::ClaimLost);
        }
        tracing::info!(
            file_id = file.id,
            node_id,
            pgs_count,
            text_count,
            representation_count,
            bytes_read = file.size.max(0),
            elapsed_ms = started.elapsed().as_millis(),
            "subtitle_source job published"
        );
        match self
            .complete_subtitle_source_request(request, &result_key, fence)
            .await
        {
            Ok(()) => {
                receipt.commit().await;
                Ok(())
            }
            Err(error) => {
                if let Err(rollback_error) = receipt.rollback(self.store.as_ref()).await {
                    tracing::error!(file_id = file.id, %rollback_error, "compensating unsettled subtitle-source publication");
                }
                Err(error)
            }
        }
    }

    async fn complete_subtitle_source_request(
        &self,
        request: &AnalysisRequest,
        result_key: &str,
        fence: &crate::background_jobs::JobFence,
    ) -> Result<(), AnalysisResolutionError> {
        if !fence
            .write_subtitle(
                request,
                plurx_core::store::background_jobs_subtitle::SubtitleJobWrite::Complete {
                    result_key: result_key.to_owned(),
                },
            )
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "queue_write_failed",
                charge_attempt: true,
            })?
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        self.analysis_metrics.publication("subtitle_source", false);
        crate::store_result::observe(
            crate::store_result::Operation::RecordPublishedAnalysisPhase,
            crate::store_result::Discard::BestEffort,
            self.store
                .record_analysis_request_phase(request, "published", None, clock_ms())
                .await,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_analysis_request(
        &self,
        request: &AnalysisRequest,
        node_id: &str,
        engine_sha256: &str,
        have_dovi: bool,
        transcode: &TranscodeManager,
        stop: &tokio_util::sync::CancellationToken,
        lost: &tokio_util::sync::CancellationToken,
        attest_timeout: Duration,
    ) -> Result<(), AnalysisResolutionError> {
        if !self
            .store
            .record_analysis_request_phase(request, "source_probe", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        let file = match self.store.get_file(request.file_id).await {
            Ok(Some(file))
                if file.size == request.source_size && file.mtime == request.source_mtime =>
            {
                file
            }
            Ok(_) => return Err(AnalysisResolutionError::Terminal("source_superseded")),
            Err(_) => {
                return Err(AnalysisResolutionError::Retry {
                    code: "source_catalog_read_failed",
                    charge_attempt: true,
                })
            }
        };
        self.set_analysis_progress_totals(
            &request.request_id,
            &request.target_node_id,
            file.size.max(0) as u64,
            file.duration_ms.unwrap_or_default(),
        );
        if request.component == "subtitle_source" {
            return Err(AnalysisResolutionError::ClaimLost); // Common workers own this component.
        }
        if request.component == "skip_markers" {
            if request.pipeline_version != crate::http::stream::CHAPTER_ANNOTATION_VERSION {
                return Err(AnalysisResolutionError::Terminal(
                    "pipeline_version_unavailable",
                ));
            }
            let stored = self
                .store
                .get_file_probe_chapters_json(file.id)
                .await
                .map_err(|_| AnalysisResolutionError::Retry {
                    code: "source_catalog_read_failed",
                    charge_attempt: true,
                })?;
            let stored_chapters = match stored.as_deref() {
                Some(raw) => Some(
                    crate::http::stream::bounded_chapter_array(raw)
                        .map_err(|_| AnalysisResolutionError::Terminal("stored_probe_invalid"))?,
                ),
                None => None,
            };
            let chapters = match stored_chapters {
                Some(chapters) => chapters,
                None => {
                    let chapters = match crate::http::stream::probe_chapters_until(
                        &file.path,
                        lost,
                        attest_timeout,
                    )
                    .await
                    {
                        Ok(chapters) => chapters,
                        Err(crate::http::stream::ChapterProbeFailure::Cancelled) => {
                            return Err(AnalysisResolutionError::ClaimLost)
                        }
                        Err(crate::http::stream::ChapterProbeFailure::Timeout) => {
                            return Err(AnalysisResolutionError::Retry {
                                code: "source_probe_timeout",
                                charge_attempt: true,
                            })
                        }
                        Err(crate::http::stream::ChapterProbeFailure::Failed) => {
                            return Err(AnalysisResolutionError::Retry {
                                code: "source_unavailable",
                                charge_attempt: true,
                            })
                        }
                    };
                    if let Ok(json) = serde_json::to_string(&chapters) {
                        // Lost work: these discovered chapters are otherwise
                        // discarded when this analysis attempt completes.
                        crate::store_result::observe(
                            crate::store_result::Operation::MergeProbeChapters,
                            crate::store_result::Discard::LostWork,
                            self.store.merge_file_probe_chapters(file.id, &json).await,
                        );
                    }
                    chapters
                }
            };
            if lost.is_cancelled() {
                return Err(AnalysisResolutionError::ClaimLost);
            }
            let duration_ms = file
                .duration_ms
                .filter(|duration| *duration > 0)
                .ok_or(AnalysisResolutionError::Terminal("source_duration_missing"))?;
            let source = crate::http::stream::annotation_source_identity(&file);
            let markers = crate::http::stream::markers_from_chapters(&chapters, Some(duration_ms));
            self.update_analysis_progress(
                &request.request_id,
                &request.target_node_id,
                "persisting",
                file.size.max(0) as u64,
                duration_ms,
                0,
            );
            let mut set = crate::http::stream::annotation_set_from_markers(source, &markers);
            set.generation_id = request.requested_generation.clone();
            if !self
                .store
                .record_analysis_request_phase(request, "publishing", None, clock_ms())
                .await
                .unwrap_or(false)
            {
                return Err(AnalysisResolutionError::ClaimLost);
            }
            // The publish is what takes this request out of `running`, so the
            // heartbeat retires here rather than back in the worker loop.
            stop.cancel();
            let published = self
                .store
                .publish_timeline_annotation_set_for_request(request, duration_ms, &set, clock_ms())
                .await
                .map_err(|_| AnalysisResolutionError::Retry {
                    code: "queue_write_failed",
                    charge_attempt: true,
                })?;
            if !published {
                return Err(AnalysisResolutionError::ClaimLost);
            }
            self.analysis_metrics
                .publication("skip_markers", request.force_rebuild);
            // Best-effort diagnostic: publication is already committed; a
            // missing phase row must not undo the durable result.
            crate::store_result::observe(
                crate::store_result::Operation::RecordPublishedAnalysisPhase,
                crate::store_result::Discard::BestEffort,
                self.store
                    .record_analysis_request_phase(request, "published", None, clock_ms())
                    .await,
            );
            return Ok(());
        }
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err(AnalysisResolutionError::Retry {
                code: "pipeline_version_unavailable",
                charge_attempt: false,
            });
        }
        if request.pipeline_version != engine_sha256 {
            return Err(AnalysisResolutionError::Terminal(
                "pipeline_version_unavailable",
            ));
        }
        let video = match fragment_index_requested_video_options(
            self.store.as_ref(),
            &file,
            have_dovi,
            transcode.dv_convert_enabled().await,
            &request.video_identity,
        )
        .await
        {
            Ok(video) => video,
            // A request that names an identity this node no longer emits is
            // superseded, not retryable: the pipeline or the probe facts
            // moved, and no number of retries brings the old identity back.
            Err(StoreError::Task(_)) => {
                return Err(AnalysisResolutionError::Terminal("pipeline_superseded"))
            }
            Err(_) => {
                return Err(AnalysisResolutionError::Retry {
                    code: "source_catalog_read_failed",
                    charge_attempt: true,
                })
            }
        };
        let object_version = crate::fragment_index_cluster::inspect_copy_source(&file)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_unavailable",
                charge_attempt: true,
            })?;
        let memo = self
            .store
            .fragment_index_source(node_id, file.id, &object_version)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_catalog_read_failed",
                charge_attempt: true,
            })?;
        if !self
            .store
            .record_analysis_request_phase(request, "hashing", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        self.update_analysis_progress(
            &request.request_id,
            &request.target_node_id,
            "verifying",
            0,
            0,
            0,
        );
        // HEVC copy reads every source byte; other codecs retain sampling.
        // Progress and ETA must describe the selected attestation regime.
        self.set_analysis_progress_totals(
            &request.request_id,
            &request.target_node_id,
            crate::fragment_index_cluster::copy_attestation_read_bytes(&file),
            file.duration_ms.unwrap_or_default(),
        );
        // Bound to a local rather than passed inline: the callback outlives
        // the argument expression for as long as the `select!` arm runs.
        let report_progress = |bytes: u64| {
            self.update_analysis_progress(
                &request.request_id,
                &request.target_node_id,
                "verifying",
                bytes,
                0,
                0,
            );
        };
        let attestation_budget = AnalysisAttestationBudget::default();
        attestation_budget.observe_busy(!transcode.pretranscode_worker_idle());
        let attested = tokio::select! {
            result = crate::fragment_index_cluster::attest_copy_source(
                node_id,
                &file,
                memo.as_ref(),
                &report_progress,
            ) => {
                result.map_err(|_| AnalysisResolutionError::Retry {
                    code: "source_attestation_failed",
                    charge_attempt: true,
                })?
            }
            () = self.wait_for_cluster_fragment_index_stop(transcode, Some(request), lost, &attestation_budget) => {
                if lost.is_cancelled() {
                    return Err(AnalysisResolutionError::ClaimLost);
                }
                return Err(AnalysisResolutionError::Retry {
                    code: "foreground_preempted",
                    charge_attempt: false,
                });
            }
            () = wait_analysis_deadline(attest_timeout) => {
                // Playback contention must not turn a formerly deferred request
                // into terminal attempt_limit. Idle-only reads retain the cap.
                return Err(attestation_budget.deadline_failure(!transcode.pretranscode_worker_idle()));
            }
        };
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        if !self
            .analysis_source_may_continue(transcode, Some(request))
            .await
            || !self.cluster_fragment_index_enabled().await
        {
            return Err(AnalysisResolutionError::Retry {
                code: "foreground_preempted",
                charge_attempt: false,
            });
        }
        self.store
            .record_fragment_index_source(&attested.observation)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_record_failed",
                charge_attempt: true,
            })?;
        let pipeline_sha256 =
            crate::fragment_index_cluster::pipeline_digest(&file, engine_sha256, video);
        let logical_cache_key = cluster_fragment_index_key(
            file.id,
            file.size,
            file.mtime,
            &attested.observation.source_sha256,
            &pipeline_sha256,
        )
        .ok_or(AnalysisResolutionError::Terminal("invalid_cache_identity"))?;
        let cache_key = if request.force_rebuild {
            cluster_fragment_index_generation_key(
                file.id,
                file.size,
                file.mtime,
                &attested.observation.source_sha256,
                &pipeline_sha256,
                &request.requested_generation,
            )
            .ok_or(AnalysisResolutionError::Terminal("invalid_cache_identity"))?
        } else {
            self.store
                .cluster_fragment_index_current_generation(&logical_cache_key)
                .await
                .map_err(|_| AnalysisResolutionError::Retry {
                    code: "source_catalog_read_failed",
                    charge_attempt: true,
                })?
                .unwrap_or(logical_cache_key)
        };
        let now = clock_ms();
        let job = NewClusterFragmentIndexJob {
            cache_key: cache_key.clone(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: attested.observation.source_sha256,
            pipeline_sha256,
            priority: request.priority.clone(),
            // Analysis uses `playback` to identify a live requester. Durable
            // artifact admission uses `foreground` for the same request class.
            trigger: if request.trigger == "playback" {
                "foreground".to_owned()
            } else {
                request.trigger.clone()
            },
            target_node_id: request.target_node_id.clone(),
            not_before_ms: now,
            created_at_ms: now,
        };
        // Artifact hydration cannot settle the target-specific structural job
        // under its request fence. Submit one ordinary worker instead of
        // hydrating and then redundantly rebuilding (or, worse, reporting a
        // failed request after valid target coverage was already installed).
        self.update_analysis_progress(
            &request.request_id,
            &request.target_node_id,
            "persisting",
            file.size.max(0) as u64,
            file.duration_ms.unwrap_or_default(),
            0,
        );

        if !self
            .store
            .record_analysis_request_phase(request, "staged", None, clock_ms())
            .await
            .unwrap_or(false)
        {
            return Err(AnalysisResolutionError::ClaimLost);
        }

        // The handoff is what takes this request out of `running`.
        stop.cancel();
        let accepted = self
            .store
            .submit_fragment_index_analysis(request, &job, now)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "queue_write_failed",
                charge_attempt: true,
            })?;
        if !accepted {
            if lost.is_cancelled() {
                return Err(AnalysisResolutionError::ClaimLost);
            }
            // `submit` refuses for two unrelated reasons and this used to map
            // both to a busy queue with an *uncharged* retry — which is an
            // infinite loop, two claims per tick per node, whenever the
            // refusal is the permanent one. A `(cache_key, target)` job that
            // is already terminal will still be terminal on the next tick and
            // on every tick after it: the request cannot make progress and
            // saying so is the only way it stops. The bulk reopen is how an
            // operator revisits that decision.
            if let Ok(Some(existing)) = self
                .store
                .cluster_fragment_index_job(&job.cache_key, &job.target_node_id)
                .await
            {
                if matches!(existing.state.as_str(), "failed" | "cancelled") {
                    return Err(AnalysisResolutionError::Terminal("job_terminal"));
                }
            }
            return Err(AnalysisResolutionError::Retry {
                code: "queue_full_or_busy",
                charge_attempt: false,
            });
        }
        // The source is attested now, so every other identity of the file can
        // be queued beside this one, at no further read.
        self.co_resolve_sibling_identities(
            &job.pipeline_sha256,
            &request.target_node_id,
            &file,
            node_id,
            engine_sha256,
            have_dovi,
            transcode.dv_convert_enabled().await,
            &object_version,
            &job.source_sha256,
            now,
        )
        .await;
        // Best-effort maintenance: unsettled requests remain durable and the
        // next queue pass retries the same bounded settlement.
        crate::store_result::observe(
            crate::store_result::Operation::SettleAnalysisAfterQueueAdmission,
            crate::store_result::Discard::BestEffort,
            self.store.settle_analysis_requests(now).await,
        );
        Ok(())
    }

    /// Queue every other copy identity of a file whose source this node has
    /// just attested (shared-read §5.5, "co-resolve").
    ///
    /// Identities are requested one at a time — discovery files one request
    /// per identity, playback files one for the identity it asked for — and
    /// a sibling's job used to be born only when its own request reached this
    /// stage, which on a busy queue was days later. So the identity Safari
    /// needed and its stripped sibling crossed the queue days apart, and each
    /// cost its own read. Queuing the siblings here, keyed exactly as their
    /// own requests would key them, puts them on the queue in the same second
    /// as the requested identity, where the worker can build them from one
    /// read. A sibling request still queued resolves later through the
    /// attestation memo, with no read, and settles against the job this made.
    ///
    /// Best effort and bounded to the file's own identities: the requested
    /// identity is already submitted, and anything that fails here is still
    /// reached the old way. A sibling that already has an index this node can
    /// serve is skipped; one whose job ended `failed` stays failed —
    /// `enqueue_cluster_fragment_index` does not reopen a tombstone, and
    /// neither does this. Siblings go in at background priority whatever the
    /// requested identity's priority was: a viewer asked for one pipeline.
    #[allow(clippy::too_many_arguments)]
    async fn co_resolve_sibling_identities(
        &self,
        requested_pipeline: &str,
        target_node_id: &str,
        file: &MediaFile,
        node_id: &str,
        engine_sha256: &str,
        have_dovi: bool,
        convert: bool,
        object_version: &str,
        source_sha256: &str,
        now: i64,
    ) {
        let options = match fragment_index_video_identity_options(
            self.store.as_ref(),
            file,
            have_dovi,
            convert,
        )
        .await
        {
            Ok(options) => options,
            Err(error) => {
                tracing::debug!(file_id = file.id, %error, "co-resolve could not read the file's identities");
                return;
            }
        };
        let siblings = options
            .into_iter()
            .map(|(video, _)| {
                crate::fragment_index_cluster::pipeline_digest(file, engine_sha256, video)
            })
            .filter(|pipeline| pipeline != requested_pipeline)
            .collect::<Vec<_>>();
        if siblings.is_empty() {
            return;
        }
        let probes = siblings
            .iter()
            .map(
                |pipeline_sha256| plurx_core::store::ClusterIndexAvailabilityProbe {
                    file_id: file.id,
                    source_size: file.size,
                    source_mtime: file.mtime,
                    object_version: object_version.to_owned(),
                    pipeline_sha256: pipeline_sha256.clone(),
                },
            )
            .collect::<Vec<_>>();
        let present = match self
            .store
            .cluster_fragment_index_availability(node_id, &probes)
            .await
        {
            Ok(present) if present.len() == probes.len() => present,
            _ => return,
        };
        let mut on_queue = 0_usize;
        for (pipeline_sha256, present) in siblings.into_iter().zip(present) {
            if present {
                continue;
            }
            let Some(logical) = cluster_fragment_index_key(
                file.id,
                file.size,
                file.mtime,
                source_sha256,
                &pipeline_sha256,
            ) else {
                continue;
            };
            let Ok(current) = self
                .store
                .cluster_fragment_index_current_generation(&logical)
                .await
            else {
                continue;
            };
            let job = NewClusterFragmentIndexJob {
                cache_key: current.unwrap_or(logical),
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                source_sha256: source_sha256.to_owned(),
                pipeline_sha256,
                priority: "normal".to_owned(),
                trigger: "background".to_owned(),
                target_node_id: target_node_id.to_owned(),
                not_before_ms: now,
                created_at_ms: now,
            };
            if self
                .store
                .enqueue_cluster_fragment_index(&job)
                .await
                .unwrap_or(false)
            {
                on_queue += 1;
            }
        }
        if on_queue > 0 {
            tracing::info!(
                file_id = file.id,
                identities = on_queue,
                "this file's other copy identities are on the queue beside the requested one"
            );
        }
    }

    async fn drain_cluster_fragment_index_slot(
        self: &Arc<Self>,
        transcode: Arc<TranscodeManager>,
    ) -> usize {
        const MAX_JOBS_PER_SLOT: usize = 4;
        const MAX_REFUSALS: usize = 4_096;

        let node_id = self.coordinator.node_id().to_owned();
        let worker = ClusterFragmentIndexWorker {
            engine_sha256: crate::ffmpeg::fragment_index_engine_digest().await,
            cache_root: crate::fragment_index_cluster::cache_root(transcode.runtime_cache_dir()),
            have_dovi: transcode.dv_strippable(),
            convert_dolby_vision: transcode.dv_convert_enabled().await,
            retry_policy: self.analysis_retry_policy().await,
        };
        let mut built = 0_usize;
        for _ in 0..MAX_JOBS_PER_SLOT {
            if !transcode.pretranscode_worker_idle()
                || !self.cluster_fragment_index_enabled().await
                || !crate::ffmpeg::fragment_index_engine_is_current().await
            {
                break;
            }
            let now = clock_ms();
            let excluded = {
                let mut refusals = self.fragment_index_refusals.lock().await;
                refusals.retain(|_, retry_at| *retry_at > now);
                refusals
                    .keys()
                    .take(MAX_REFUSALS)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let (job, active, admission) = match crate::background_jobs::claim_fragment(
                Arc::clone(&self.store),
                Arc::clone(&self.job_authority),
                &transcode,
                &node_id,
                &excluded,
                &crate::background_jobs::FragmentPipeline {
                    engine_sha256: &worker.engine_sha256,
                    have_dovi: worker.have_dovi,
                    convert_dolby_vision: worker.convert_dolby_vision,
                },
            )
            .await
            {
                Ok(Some(claimed)) => claimed,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "claiming durable fragment work");
                    break;
                }
            };
            let admission = Arc::new(admission);
            let keeper: Arc<dyn plurx_core::process::bounded::OriginalWorkKeeper> =
                admission.clone();
            if plurx_core::process::bounded::with_original_work_keeper(
                keeper,
                Arc::clone(self).run_cluster_fragment_index_job(
                    Arc::clone(&transcode),
                    job,
                    worker.clone(),
                    active.fence(),
                    &admission,
                ),
            )
            .await
            {
                built += 1;
            }
            active.finish().await;
            drop(admission);
        }
        built
    }

    async fn cluster_fragment_index_enabled(&self) -> bool {
        self.store
            .get_setting(keys::VOD_INDEX_CLUSTER_CACHE)
            .await
            .ok()
            .is_some_and(setting_enabled)
    }

    async fn analysis_retry_policy(&self) -> AnalysisRetryPolicy {
        let settings = self.store.settings_snapshot().await.unwrap_or_default();
        AnalysisRetryPolicy::from_settings(&settings)
    }

    async fn analysis_source_may_continue(
        &self,
        transcode: &TranscodeManager,
        request: Option<&AnalysisRequest>,
    ) -> bool {
        if transcode.pretranscode_worker_idle() {
            return true;
        }
        let Some(request) = request else {
            return false;
        };
        // Source attestation holds the Store's bounded source-I/O reservation,
        // not an encoder slot. Any live playback waiter on this request permits
        // the read while this node is busy. Expired/departed viewers and failed
        // reads fail closed.
        self.store
            .analysis_preparation_observation(&request.request_id, clock_ms())
            .await
            .ok()
            .flatten()
            .is_some_and(|state| state.has_live_viewer)
    }

    async fn wait_for_cluster_fragment_index_stop(
        &self,
        transcode: &TranscodeManager,
        request: Option<&AnalysisRequest>,
        permit_lost: &tokio_util::sync::CancellationToken,
        attestation_budget: &AnalysisAttestationBudget,
    ) {
        let mut ticks = 0_u8;
        loop {
            attestation_budget.observe_busy(!transcode.pretranscode_worker_idle());
            if permit_lost.is_cancelled()
                || !self.analysis_source_may_continue(transcode, request).await
            {
                return;
            }
            if ticks == 0 && !self.cluster_fragment_index_enabled().await {
                return;
            }
            ticks = (ticks + 1) % 4;
            tokio::select! {
                () = permit_lost.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    async fn wait_for_fragment_worker_stop(
        &self,
        transcode: &TranscodeManager,
        admission: &crate::transcode::FragmentAdmission,
        lost: &tokio_util::sync::CancellationToken,
    ) {
        loop {
            if lost.is_cancelled()
                || !transcode.fragment_worker_idle(admission)
                || !self.cluster_fragment_index_enabled().await
            {
                return;
            }
            tokio::select! {
                () = lost.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    async fn remember_fragment_index_refusal(&self, cache_key: &str, retry_at_ms: i64) {
        const MAX_REFUSALS: usize = 4_096;
        let mut refusals = self.fragment_index_refusals.lock().await;
        refusals.retain(|_, retry_at| *retry_at > clock_ms());
        if refusals.len() >= MAX_REFUSALS {
            if let Some(oldest) = refusals
                .iter()
                .min_by_key(|(_, retry_at)| **retry_at)
                .map(|(key, _)| key.clone())
            {
                refusals.remove(&oldest);
            }
        }
        refusals.insert(cache_key.to_owned(), retry_at_ms);
    }

    fn record_job_outcome(
        &self,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        action: &str,
        code: &str,
        result: Result<bool, StoreError>,
    ) -> bool {
        record_analysis_outcome(&self.analysis_metrics, job, action, code, result)
    }

    async fn fail_fragment_index_job(
        &self,
        fence: &crate::background_jobs::JobFence,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        code: &str,
        retryable: bool,
        retry_at_ms: i64,
    ) -> bool {
        use plurx_core::store::background_jobs::JobSettlement;
        let settlement = if retryable {
            JobSettlement::Retry {
                error_code: code.into(),
                not_before_ms: retry_at_ms,
            }
        } else {
            JobSettlement::Fail {
                error_code: code.into(),
            }
        };
        self.record_job_outcome(job, "fail", code, fence.settle(settlement).await)
    }

    async fn yield_fragment_index_job(
        &self,
        fence: &crate::background_jobs::JobFence,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        code: &str,
        retry_at_ms: i64,
    ) -> bool {
        let result = fence
            .settle(plurx_core::store::background_jobs::JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: retry_at_ms,
            })
            .await;
        self.record_job_outcome(job, "yield", code, result)
    }

    /// Record a cluster worker's refusal in this node's own outcome table too.
    ///
    /// The queue row explains the *job*; this row explains the *file*, which
    /// is what the admin badge and the background pass read. Without it a
    /// clustered node's own indexer would happily spend the same whole-file
    /// read the worker just spent, and the operator surface would still say
    /// "pending" for a title the cluster already gave up on.
    async fn record_local_index_refusal(
        &self,
        file: &plurx_core::domain::MediaFile,
        video: plurx_core::transcode::CopyVideoOptions,
        refusal: plurx_core::segplan::IndexRefusal,
        reason: &str,
    ) {
        let identity = crate::fragindex::identity_for(file, video);
        if let Err(error) = self
            .store
            .record_fragment_index_outcome(file.id, &identity, refusal, reason)
            .await
        {
            tracing::warn!(
                file_id = file.id,
                error = %error,
                "recording a cluster fragment index outcome locally"
            );
        }
    }

    async fn record_local_index_failure(
        &self,
        file: &plurx_core::domain::MediaFile,
        video: plurx_core::transcode::CopyVideoOptions,
        failure: &crate::fragindex::IndexFailure,
        max_attempts: u32,
    ) {
        let identity = crate::fragindex::identity_for(file, video);
        if let Err(error) = self
            .store
            .record_fragment_index_typed_outcome(
                file.id,
                &identity,
                failure.code,
                failure.transient_allowlisted,
                &failure.reason,
                u32::try_from(failure.rows).unwrap_or(u32::MAX),
                &failure.diagnostic,
                max_attempts,
            )
            .await
        {
            tracing::warn!(
                file_id = file.id,
                error = %error,
                "recording a typed cluster fragment-index outcome locally"
            );
        }
    }

    async fn run_cluster_fragment_index_job(
        self: Arc<Self>,
        transcode: Arc<TranscodeManager>,
        job: plurx_core::store::ClusterFragmentIndexJob,
        worker: ClusterFragmentIndexWorker,
        fence: crate::background_jobs::JobFence,
        admission: &crate::transcode::FragmentAdmission,
    ) -> bool {
        const LOCAL_REFUSAL_MS: i64 = 30_000;
        /// Hard background I/O budget. HEVC copy proof hashes the full source;
        /// other consumers still sample. Large/slow files may exhaust this
        /// budget and remain unverified, with bounded charged retries.
        const ATTEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

        let _progress = self.start_analysis_progress(
            (&job.cache_key, &job.target_node_id),
            job.file_id,
            "fragment_index",
            "verifying",
            job.source_size.max(0) as u64,
            0,
        );
        if let Some(token) = fence.snapshot().await {
            _progress.attach_attempt(&token);
        }
        let node_id = self.coordinator.node_id().to_owned();
        let lost = fence.loss_token();
        let retry_identity = format!("{}:{}", job.cache_key, job.target_node_id);
        let retry_ms = worker
            .retry_policy
            .backoff_ms(&retry_identity, job.attempts.max(1));
        let published = self
            .store
            .cluster_fragment_index_artifact(&job.cache_key)
            .await
            .ok()
            .flatten();
        if let Some(artifact) = &published {
            let hydrated = tokio::select! {
                result = crate::fragment_index_cluster::hydrate_for_job(self.store.as_ref(), self.membership.as_ref(),
                    &node_id, &worker.cache_root, artifact) => matches!(result, Ok(Some(_))),
                () = lost.cancelled() => false,
            };
            if hydrated {
                let result = fence.publish_fragment(artifact.clone()).await;
                return self.record_job_outcome(&job, "hydrate", "", result);
            }
        }
        if !job.target_node_id.is_empty() {
            let now = clock_ms();
            // Hydration never silently becomes another full-file build on
            // the receiving node. A source-readable worker owns one canonical
            // repair, while this delivery yields its local/global permits.
            if published.is_some() {
                use plurx_core::store::background_jobs::{
                    EnqueueFragmentJob, EnqueueOutcome, JobState,
                };
                let repair = self
                    .store
                    .enqueue_fragment_job(EnqueueFragmentJob {
                        job: plurx_core::store::NewClusterFragmentIndexJob {
                            cache_key: job.cache_key.clone(),
                            file_id: job.file_id,
                            source_size: job.source_size,
                            source_mtime: job.source_mtime,
                            source_sha256: job.source_sha256.clone(),
                            pipeline_sha256: job.pipeline_sha256.clone(),
                            priority: job.priority.clone(),
                            trigger: "background".into(),
                            target_node_id: String::new(),
                            not_before_ms: now,
                            created_at_ms: now,
                        },
                        analysis_request: None,
                        repair: true,
                        now_ms: now,
                    })
                    .await;
                let waiting = match repair {
                    Ok(
                        EnqueueOutcome::Accepted { .. }
                        | EnqueueOutcome::QueueFull
                        | EnqueueOutcome::JobCancelling { .. },
                    ) => true,
                    Ok(EnqueueOutcome::Existing {
                        job_id,
                        cancelled: false,
                        ..
                    }) => self
                        .store
                        .background_job(&job_id)
                        .await
                        .map(|row| {
                            row.is_some_and(|job| {
                                matches!(job.state, JobState::Queued | JobState::Running)
                            })
                        })
                        .unwrap_or(true),
                    Err(error) => {
                        tracing::debug!(%error, "fragment repair admission unavailable");
                        true
                    }
                    _ => false,
                };
                if waiting {
                    self.yield_fragment_index_job(
                        &fence,
                        &job,
                        "awaiting_artifact_repair",
                        now.saturating_add(30_000),
                    )
                    .await;
                    return false;
                }
            }
            self.fail_fragment_index_job(
                &fence,
                &job,
                "artifact_unavailable",
                true,
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }

        let file = match classify_fragment_source_read(
            self.store.get_file(job.file_id).await,
            job.source_size,
            job.source_mtime,
        ) {
            Ok(file) => file,
            Err(failure) => {
                let now = clock_ms();
                let (code, retryable) = match failure {
                    FragmentSourceReadFailure::Stale => ("source_superseded", false),
                    FragmentSourceReadFailure::Transient => ("source_catalog_read_failed", true),
                };
                self.fail_fragment_index_job(
                    &fence,
                    &job,
                    code,
                    retryable,
                    now.saturating_add(retry_ms),
                )
                .await;
                return false;
            }
        };
        self.set_analysis_progress_totals(
            &job.cache_key,
            &job.target_node_id,
            file.size.max(0) as u64,
            file.duration_ms.unwrap_or_default(),
        );
        let videos = match fragment_index_video_identities(
            self.store.as_ref(),
            &file,
            worker.have_dovi,
            worker.convert_dolby_vision,
        )
        .await
        {
            Ok(videos) => videos,
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "reading probe for claimed fragment index");

                let now = clock_ms();
                self.fail_fragment_index_job(
                    &fence,
                    &job,
                    "source_catalog_read_failed",
                    true,
                    now.saturating_add(retry_ms),
                )
                .await;
                return false;
            }
        };
        let object_version = match crate::fragment_index_cluster::inspect_copy_source(&file).await {
            Ok(version) => version,
            Err(error) => {
                tracing::debug!(file_id = file.id, %error, "claimed index source is not local");

                let now = clock_ms();
                if job.target_node_id.is_empty() {
                    self.remember_fragment_index_refusal(
                        &job.cache_key,
                        now.saturating_add(LOCAL_REFUSAL_MS),
                    )
                    .await;
                    self.yield_fragment_index_job(&fence, &job, "node_local_refusal", now)
                        .await;
                } else {
                    self.fail_fragment_index_job(
                        &fence,
                        &job,
                        "source_unavailable",
                        true,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                }
                return false;
            }
        };
        let memo = self
            .store
            .fragment_index_source(&node_id, file.id, &object_version)
            .await
            .ok()
            .flatten();
        // See the analysis path: the verify stage is denominated in the
        // sample it reads, and the build below restores the file's own size.
        self.set_analysis_progress_totals(
            &job.cache_key,
            &job.target_node_id,
            crate::fragment_index_cluster::copy_attestation_read_bytes(&file),
            file.duration_ms.unwrap_or_default(),
        );
        let report_progress = |bytes: u64| {
            self.update_analysis_progress(
                &job.cache_key,
                &job.target_node_id,
                "verifying",
                bytes,
                0,
                0,
            );
        };
        let attestation = tokio::select! {
            result = crate::fragment_index_cluster::attest_copy_source(
                &node_id,
                &file,
                memo.as_ref(),
                &report_progress,
            ) => Some(result.map_err(AttestationFailure::Refused)),
            () = self.wait_for_fragment_worker_stop(
                transcode.as_ref(), admission,
                &lost,
            ) => None,
            () = tokio::time::sleep(ATTEST_TIMEOUT) => {
                Some(Err(AttestationFailure::TimedOut))
            }
        };
        let Some(attestation) = attestation else {
            let now = clock_ms();
            self.yield_fragment_index_job(
                &fence,
                &job,
                "node_local_refusal",
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        };
        let attested = match attestation {
            Ok(attested) if attested.observation.source_sha256 == job.source_sha256 => attested,
            outcome => {
                // A ten-minute read of a 64 MiB sample is a mount that stopped
                // answering; a digest that does not match is the source
                // changing. The queue shows this code, so it has to be the
                // right one.
                let code = attestation_failure_code(&outcome);
                let timed_out = matches!(outcome, Err(AttestationFailure::TimedOut));
                if let Err(AttestationFailure::Refused(reason)) = &outcome {
                    // The code is the durable answer; this is the sentence
                    // that says which of the identity checks refused, which
                    // the row has nowhere to keep.
                    tracing::debug!(
                        cache_key = %job.cache_key,
                        %reason,
                        "fragment-index source attestation refused"
                    );
                }

                let now = clock_ms();
                if job.target_node_id.is_empty() {
                    // An untargeted job is yielded back to the cluster rather
                    // than failed, so its code has nowhere durable to go. A
                    // refusal earns a bounded local exclusion: this node has
                    // proved it cannot serve that source. A timeout has proved
                    // nothing of the kind — the mount was unreachable for ten
                    // minutes, which is a condition and not a verdict, and
                    // excluding the key for a day would keep the node off its
                    // own files long after the storage recovered.
                    if timed_out {
                        tracing::warn!(
                            cache_key = %job.cache_key,
                            "fragment-index source attestation timed out; yielding without excluding this node"
                        );
                    } else {
                        self.remember_fragment_index_refusal(
                            &job.cache_key,
                            now.saturating_add(LOCAL_REFUSAL_MS),
                        )
                        .await;
                    }
                    self.yield_fragment_index_job(
                        &fence,
                        &job,
                        // A yield writes no durable code, but the lost-write
                        // log is the one place this distinction can still be
                        // read, and it is the distinction the code above
                        // exists to make.
                        code,
                        if timed_out {
                            now.saturating_add(retry_ms)
                        } else {
                            now
                        },
                    )
                    .await;
                } else {
                    self.fail_fragment_index_job(
                        &fence,
                        &job,
                        code,
                        true,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                }
                return false;
            }
        };
        // Lost work: this fresh source attestation is required for another
        // node to use the index being built from it.
        crate::store_result::observe(
            crate::store_result::Operation::RecordFragmentIndexSource,
            crate::store_result::Discard::LostWork,
            self.store
                .record_fragment_index_source(&attested.observation)
                .await,
        );
        // Recheck after source attestation: local capabilities may change
        // after preflight. Another worker may still implement this identity.
        let Some(video) = videos.iter().copied().find(|video| {
            crate::fragment_index_cluster::pipeline_digest(&file, &worker.engine_sha256, *video)
                == job.pipeline_sha256
        }) else {
            let now = clock_ms();
            self.remember_fragment_index_refusal(
                &job.cache_key,
                now.saturating_add(LOCAL_REFUSAL_MS),
            )
            .await;
            self.yield_fragment_index_job(&fence, &job, "node_local_refusal", now)
                .await;
            return false;
        };

        // Shared read (§5.5, co-claim): the file's other identities whose
        // builds are queued for this same attested source are built by this
        // pass, from this one read, under the lead's claim and its single
        // source reservation. Each is claimed and published under a fence of
        // its own only once the lead has settled — a sibling claim beside a
        // running lead would need a second source reservation, which the
        // store keeps for live demand.
        let siblings = self
            .queued_sibling_builds(&job, &file, &videos, &worker, &node_id)
            .await;
        let mut pass_videos = vec![video];
        pass_videos.extend(siblings.iter().map(|(_, video)| *video));
        let reporter = self.index_pass_reporter(&job.cache_key, &job.target_node_id);
        if pass_videos.len() > 1 {
            // The lead's row is the pass's row: it reads once for all of them.
            self.set_analysis_shared_read(&job.cache_key, &job.target_node_id, pass_videos.len());
        }
        let report_progress = move |progress: &crate::fragindex::PassProgress| reporter(progress);
        // Asked per job: the switch, the self-test and the filesystem, now.
        let ride_along = crate::subtitle_ride_along::RideAlongGate::open(
            Arc::clone(&self.store),
            transcode.runtime_cache_dir(),
        )
        .await;
        let cancel = lost.child_token();
        // The lead's last attempt on this node, if it ran out of budget, says
        // how long this node needs for this file (`index_file_budget`).
        let previous = match self
            .store
            .fragment_index_outcome(file.id, &crate::fragindex::identity_for(&file, video))
            .await
        {
            Ok(previous) => previous,
            Err(error) => {
                tracing::debug!(file_id = file.id, %error, "reading the last index outcome");
                None
            }
        };
        let build = crate::fragindex::build_shared_from_attested_file_with_progress(
            &file,
            &attested.handle,
            &attested.observation.object_version,
            &pass_videos,
            transcode.runtime_cache_dir(),
            index_file_budget(file.duration_ms, file.size, previous.as_ref()),
            report_progress,
            ride_along.as_ref(),
            &cancel,
        );
        tokio::pin!(build);
        let (builds, preempted) = tokio::select! {
            built = &mut build => (Some(built), false),
            () = self.wait_for_fragment_worker_stop(transcode.as_ref(), admission, &lost) => {
                cancel.cancel();
                let _ = build.await;
                (None, !lost.is_cancelled())
            }
        };
        if lost.is_cancelled() {
            return false;
        }
        if preempted {
            let now = clock_ms();
            self.yield_fragment_index_job(
                &fence,
                &job,
                "foreground_preempted",
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        let Some(builds) = builds else {
            // The lease was lost mid-build. The row stays `running` until a
            // sweep reclaims it; writing an outcome on a claim we no longer
            // hold is exactly what the fence forbids.
            return false;
        };
        // Every outcome is a statement about the held source, including a
        // failure. If those bytes changed while ffmpeg was running, record
        // source movement instead of caching or retrying a diagnosis for the
        // retired object. The siblings were never claimed; they keep their
        // own turn.
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
        {
            let now = clock_ms();
            self.fail_fragment_index_job(
                &fence,
                &job,
                "source_changed",
                false,
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        let still_current = self
            .store
            .get_file(file.id)
            .await
            .ok()
            .flatten()
            .is_some_and(|current| current.size == file.size && current.mtime == file.mtime);
        if !still_current {
            let now = clock_ms();
            self.fail_fragment_index_job(
                &fence,
                &job,
                "source_superseded",
                false,
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        let mut builds = builds;
        // Both freshness checks passed: the held source is the object the
        // pass read, and the catalog still describes it. The PGS tracks the
        // pass kept are a statement about exactly that source, whatever
        // happens to the index publications below. They travel with the
        // pass's first built output.
        if let Some(pending) = builds.iter_mut().find_map(|build| build.ride_along.take()) {
            // `true`: the `source_still_matches` refusal above has returned
            // already for a source that moved.
            self.settle_ride_along(&file, pending, true).await;
        }
        if let Some(pending) = builds.iter_mut().find_map(|build| build.dv_title.take()) {
            self.settle_dv_title(&file, pending).await;
        }
        let mut builds = builds.into_iter();
        let Some(lead) = builds.next() else {
            return false;
        };
        let built_siblings = siblings
            .into_iter()
            .zip(builds)
            .map(|((pipeline, video), build)| (pipeline, video, build.outcome))
            .collect::<Vec<_>>();
        if !built_siblings.is_empty() {
            self.mark_failed_shared_identity(
                &file,
                &job.source_sha256,
                &job.pipeline_sha256,
                &lead.outcome,
            )
            .await;
            for (pipeline, _, outcome) in &built_siblings {
                self.mark_failed_shared_identity(&file, &job.source_sha256, pipeline, outcome)
                    .await;
            }
        }
        let completed = self
            .settle_cluster_fragment_index_outcome(
                transcode.as_ref(),
                &job,
                &worker,
                &fence,
                admission,
                &file,
                video,
                &attested,
                published,
                retry_ms,
                lead.outcome,
            )
            .await;
        self.publish_sibling_builds(
            transcode.as_ref(),
            &job,
            &worker,
            admission,
            &file,
            &attested,
            &node_id,
            built_siblings,
        )
        .await;
        completed
    }

    /// The queued builds of the lead's other identities for this same attested
    /// source, by pipeline, and the identity each builds (§5.5). Identities
    /// marked by an earlier failed shared read are left to run alone, and a
    /// marked lead shares nothing. Nothing is claimed here.
    async fn queued_sibling_builds(
        &self,
        lead: &plurx_core::store::ClusterFragmentIndexJob,
        file: &MediaFile,
        videos: &[plurx_core::transcode::CopyVideoOptions],
        worker: &ClusterFragmentIndexWorker,
        node_id: &str,
    ) -> Vec<(String, plurx_core::transcode::CopyVideoOptions)> {
        let marked = self.shared_pass_marked(file, &lead.source_sha256).await;
        if marked.contains(&lead.pipeline_sha256) {
            return Vec::new();
        }
        let mut by_pipeline = HashMap::new();
        for video in videos {
            let pipeline =
                crate::fragment_index_cluster::pipeline_digest(file, &worker.engine_sha256, *video);
            if pipeline != lead.pipeline_sha256 && !marked.contains(&pipeline) {
                by_pipeline.insert(pipeline, *video);
            }
        }
        if by_pipeline.is_empty() {
            return Vec::new();
        }
        let pipelines = by_pipeline.keys().cloned().collect::<Vec<_>>();
        let excluded = self.excluded_fragment_keys().await;
        match crate::background_jobs::fragment_siblings(
            Arc::clone(&self.store),
            Arc::clone(&self.job_authority),
            node_id,
            lead,
            &pipelines,
            &excluded,
            &worker.pipeline(),
            false,
        )
        .await
        {
            Ok(queued) => queued
                .into_iter()
                .filter_map(|(job, _)| {
                    by_pipeline
                        .get(&job.pipeline_sha256)
                        .map(|video| (job.pipeline_sha256, *video))
                })
                .collect(),
            Err(error) => {
                tracing::debug!(%error, "looking for sibling fragment builds");
                Vec::new()
            }
        }
    }

    /// Publish the indexes a lead's pass built for its siblings: each sibling
    /// job is claimed now — the lead has settled and released its source
    /// reservation — under a fence of its own, and settled with what the pass
    /// built for it, exactly as a pass of its own would have settled it. A
    /// sibling another worker has taken meanwhile is left to it; one whose
    /// pass failed for reasons of the pass (a process failure or anything
    /// transient) is left queued and uncharged, to build on its own turn.
    #[allow(clippy::too_many_arguments)]
    async fn publish_sibling_builds(
        &self,
        transcode: &TranscodeManager,
        lead: &plurx_core::store::ClusterFragmentIndexJob,
        worker: &ClusterFragmentIndexWorker,
        admission: &crate::transcode::FragmentAdmission,
        file: &MediaFile,
        attested: &crate::fragment_index_cluster::AttestedSource,
        node_id: &str,
        built: Vec<(
            String,
            plurx_core::transcode::CopyVideoOptions,
            crate::fragindex::IndexOutcome,
        )>,
    ) {
        // A foreground viewer arrived: the siblings keep their own turn.
        if !transcode.fragment_worker_idle(admission) {
            return;
        }
        let built = built
            .into_iter()
            .filter(|(_, _, outcome)| !failed_as_a_pass(outcome))
            .collect::<Vec<_>>();
        let excluded = self.excluded_fragment_keys().await;
        let mut published = 0_usize;
        // One at a time: each claim takes the background source slot and its
        // settlement gives it back, so a second claim beside an unsettled
        // first would be refused and its prebuilt index wasted.
        for (pipeline, video, outcome) in built {
            let claimed = match crate::background_jobs::fragment_siblings(
                Arc::clone(&self.store),
                Arc::clone(&self.job_authority),
                node_id,
                lead,
                std::slice::from_ref(&pipeline),
                &excluded,
                &worker.pipeline(),
                true,
            )
            .await
            {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::debug!(%error, "claiming a sibling fragment build for publication");
                    continue;
                }
            };
            let Some((job, Some(active))) = claimed.into_iter().next() else {
                continue;
            };
            let fence = active.fence();
            let retry_ms = worker.retry_policy.backoff_ms(
                &format!("{}:{}", job.cache_key, job.target_node_id),
                job.attempts.max(1),
            );
            // The same two facts the lead's publication rested on, asked
            // again: time has passed since the pass ended.
            let artifact = self
                .store
                .cluster_fragment_index_artifact(&job.cache_key)
                .await
                .ok()
                .flatten();
            let still_current = self
                .store
                .get_file(file.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|current| current.size == file.size && current.mtime == file.mtime);
            if !still_current
                || !crate::fragment_index_cluster::source_still_matches(
                    &attested.handle,
                    &attested.observation,
                )
                .unwrap_or(false)
            {
                self.yield_fragment_index_job(&fence, &job, "source_changed", clock_ms())
                    .await;
            } else if self
                .settle_cluster_fragment_index_outcome(
                    transcode, &job, worker, &fence, admission, file, video, attested, artifact,
                    retry_ms, outcome,
                )
                .await
            {
                published += 1;
            }
            active.finish().await;
        }
        if published > 0 {
            tracing::info!(
                file_id = file.id,
                identities = published,
                "published sibling fragment indexes built from the lead's read"
            );
        }
    }

    async fn excluded_fragment_keys(&self) -> Vec<String> {
        let now = clock_ms();
        let mut refusals = self.fragment_index_refusals.lock().await;
        refusals.retain(|_, retry_at| *retry_at > now);
        refusals.keys().cloned().collect()
    }

    /// The pipelines of this file and source whose shared read failed (§5.6).
    async fn shared_pass_marked(
        &self,
        file: &MediaFile,
        source_sha256: &str,
    ) -> std::collections::HashSet<String> {
        let now = clock_ms();
        let prefix = format!("{}:{}:{}:{source_sha256}:", file.id, file.size, file.mtime);
        let mut marks = self.shared_pass_marks.lock().await;
        marks.retain(|_, until| *until > now);
        marks
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    /// §5.6: an identity whose shared pass ended in a process failure or ran
    /// out of budget builds alone next time. Transient failures and the
    /// identity's own refusals say nothing about sharing, and leave no mark. Keyed by file, source
    /// digest and pipeline, so a replaced source or a new engine starts clean.
    async fn mark_failed_shared_identity(
        &self,
        file: &MediaFile,
        source_sha256: &str,
        pipeline_sha256: &str,
        outcome: &crate::fragindex::IndexOutcome,
    ) {
        const MARK_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
        const MAX_MARKS: usize = 4_096;
        if !failed_as_a_shared_pass(outcome) {
            return;
        }
        let now = clock_ms();
        let mut marks = self.shared_pass_marks.lock().await;
        marks.retain(|_, until| *until > now);
        if marks.len() >= MAX_MARKS {
            if let Some(oldest) = marks
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(key, _)| key.clone())
            {
                marks.remove(&oldest);
            }
        }
        tracing::info!(
            file_id = file.id,
            pipeline = pipeline_sha256,
            "this identity failed a shared read; it builds alone until the mark expires"
        );
        marks.insert(
            format!(
                "{}:{}:{}:{source_sha256}:{pipeline_sha256}",
                file.id, file.size, file.mtime
            ),
            now.saturating_add(MARK_MS),
        );
    }

    /// One identity's index from a cluster pass, settled under that job's
    /// own fence: recorded, encoded, installed and published — or failed,
    /// exactly as a pass of its own would have settled it.
    #[allow(clippy::too_many_arguments)]
    async fn settle_cluster_fragment_index_outcome(
        &self,
        transcode: &TranscodeManager,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        worker: &ClusterFragmentIndexWorker,
        fence: &crate::background_jobs::JobFence,
        admission: &crate::transcode::FragmentAdmission,
        file: &MediaFile,
        video: plurx_core::transcode::CopyVideoOptions,
        attested: &crate::fragment_index_cluster::AttestedSource,
        published: Option<ClusterFragmentIndexArtifact>,
        retry_ms: i64,
        outcome: crate::fragindex::IndexOutcome,
    ) -> bool {
        let lost = fence.loss_token();
        let node_id = self.coordinator.node_id().to_owned();
        let mut index = match outcome {
            crate::fragindex::IndexOutcome::Built(index) => index,
            // The node-local row is recorded here too, so a clustered node's
            // own operator surface and its background pass both know what this
            // worker found out.
            //
            crate::fragindex::IndexOutcome::Unsupported(reason) => {
                tracing::warn!(file_id = file.id, %reason, "cluster fragment index unsupported");
                self.record_local_index_refusal(
                    file,
                    video,
                    plurx_core::segplan::IndexRefusal::Unsupported,
                    &reason,
                )
                .await;

                let now = clock_ms();
                self.fail_fragment_index_job(
                    fence,
                    job,
                    "unsupported",
                    false,
                    now.saturating_add(retry_ms),
                )
                .await;
                return false;
            }
            crate::fragindex::IndexOutcome::Failed(failure) => {
                tracing::warn!(
                    file_id = file.id,
                    code = failure.code.as_str(),
                    rows = failure.rows,
                    reason = %failure.reason,
                    "cluster fragment index failed"
                );
                self.record_local_index_failure(
                    file,
                    video,
                    &failure,
                    worker.retry_policy.max_attempts,
                )
                .await;

                let code = failure.code.as_str().to_owned();
                let written = fence
                    .fail_fragment(
                        failure.code,
                        failure.transient_allowlisted,
                        failure.diagnostic,
                    )
                    .await;
                self.record_job_outcome(job, "fail", &code, written);
                return false;
            }
        };
        self.update_analysis_progress(
            &job.cache_key,
            &job.target_node_id,
            "persisting",
            file.size.max(0) as u64,
            file.duration_ms.unwrap_or_default(),
            index.rows.len(),
        );
        if let Some(proof) = index.promotion.hevc_configuration.as_mut() {
            proof.source_node_id = node_id.clone();
            proof.source_object_version = crate::fragment_index_cluster::local_object_version(
                &attested.observation.object_version,
            )
            .to_owned();
            proof.source_sha256 = Some(attested.observation.source_sha256.clone());
        }
        let blob = match encode_cluster_fragment_index_blob(
            &index,
            &job.source_sha256,
            &job.pipeline_sha256,
        ) {
            Ok(blob) => blob,
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "encoding cluster fragment index");

                let now = clock_ms();
                self.fail_fragment_index_job(
                    fence,
                    job,
                    "encode_failed",
                    false,
                    now.saturating_add(retry_ms),
                )
                .await;
                return false;
            }
        };
        let built_at_ms = clock_ms();
        let artifact = ClusterFragmentIndexArtifact {
            cache_key: job.cache_key.clone(),
            file_id: job.file_id,
            source_size: job.source_size,
            source_mtime: job.source_mtime,
            source_sha256: job.source_sha256.clone(),
            pipeline_sha256: job.pipeline_sha256.clone(),
            blob_sha256: cluster_fragment_index_blob_sha256(&blob),
            bytes: i64::try_from(blob.len()).unwrap_or(i64::MAX),
            built_by_node_id: node_id.clone(),
            built_at_ms,
        };
        // Rebuilding missing bytes preserves the immutable generation's original
        // provenance. A different byte digest remains a publication conflict.
        let artifact = match published {
            Some(original)
                if original.blob_sha256 == artifact.blob_sha256
                    && original.bytes == artifact.bytes =>
            {
                original
            }
            _ => artifact,
        };
        if lost.is_cancelled()
            || !transcode.fragment_worker_idle(admission)
            || !self.cluster_fragment_index_enabled().await
            || !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            let now = clock_ms();
            self.yield_fragment_index_job(
                fence,
                job,
                "foreground_preempted",
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        let Some(index_path) =
            crate::fragment_index_cluster::cache_path(&worker.cache_root, &artifact.cache_key)
        else {
            self.fail_fragment_index_job(fence, job, "invalid_cache_key", false, clock_ms())
                .await;
            return false;
        };
        let _index_publication_pin = match crate::playback_cache::pin(&index_path) {
            Ok(pin) => pin,
            Err(_) => {
                self.yield_fragment_index_job(
                    fence,
                    job,
                    "cache_maintenance",
                    clock_ms().saturating_add(retry_ms),
                )
                .await;
                return false;
            }
        };
        if let Err(error) =
            crate::fragment_index_cluster::install_local_blob(&worker.cache_root, &artifact, &blob)
                .await
        {
            tracing::warn!(file_id = file.id, %error, "publishing local fragment-index blob");

            let now = clock_ms();
            self.fail_fragment_index_job(
                fence,
                job,
                "local_publish_failed",
                true,
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        if lost.is_cancelled()
            || !transcode.fragment_worker_idle(admission)
            || !self.cluster_fragment_index_enabled().await
            || !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            let now = clock_ms();
            self.yield_fragment_index_job(
                fence,
                job,
                "foreground_preempted",
                now.saturating_add(retry_ms),
            )
            .await;
            return false;
        }
        let settled = fence.publish_fragment(artifact).await;
        let completed = self.record_job_outcome(job, "complete", "", settled);
        if completed {
            if let Err(error) = self.store.put_fragment_index(file.id, &index).await {
                tracing::warn!(file_id = file.id, %error, "installing built fragment index");
            }
            self.analysis_metrics
                .publication("fragment_index", job.priority == "forced");
        }
        completed
    }

    /// The preparation lanes the settings open: the speculative discovery
    /// schedule and the `vod.output_preparation` Developer switch.
    pub(crate) async fn preparation_lanes(&self) -> crate::background_jobs::PreparationLanes {
        self.read_preparation_lanes().await.0
    }

    /// [`Self::preparation_lanes`], and whether the output switch was actually
    /// read. A store error is not "off": the output lane stays closed for this
    /// tick (nothing is claimed under a switch nobody read) and the caller
    /// must not drain on it, because draining cancels queued work the
    /// operator may well have switched on.
    async fn read_preparation_lanes(&self) -> (crate::background_jobs::PreparationLanes, bool) {
        /// One warning per run of failures, debug for the rest of the run.
        static OUTPUT_SWITCH_UNREADABLE: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let speculative = self.job_interval(keys::JOB_CACHE_PRODUCE_MINS).await > 0;
        let (output, read) = match self.store.get_setting(keys::VOD_OUTPUT_PREPARATION).await {
            Ok(value) => {
                OUTPUT_SWITCH_UNREADABLE.store(false, Ordering::Relaxed);
                (
                    crate::vodserve::OutputPreparation::parse(value.as_deref()),
                    true,
                )
            }
            Err(error) => {
                if OUTPUT_SWITCH_UNREADABLE.swap(true, Ordering::Relaxed) {
                    tracing::debug!(%error, "output preparation switch still unreadable; lane and drain skipped");
                } else {
                    tracing::warn!(%error, "could not read the output preparation switch; its lane and drain are skipped until it reads");
                }
                (crate::vodserve::OutputPreparation::Off, false)
            }
        };
        (
            crate::background_jobs::PreparationLanes {
                speculative,
                output,
            },
            read,
        )
    }

    /// `lanes` narrowed to what this node's job authority lets it execute.
    async fn executable_lanes(
        &self,
        lanes: crate::background_jobs::PreparationLanes,
    ) -> crate::background_jobs::PreparationLanes {
        use plurx_core::store::background_jobs::JobKind;
        let speculative = lanes.speculative
            && self
                .job_authority
                .may_execute_job(JobKind::TranscodePrepare)
                .await;
        let output = match lanes.output {
            crate::vodserve::OutputPreparation::Off => crate::vodserve::OutputPreparation::Off,
            mode => {
                let copy = self
                    .job_authority
                    .may_execute_job(JobKind::CopyOutputPrepare)
                    .await;
                let encoded = mode == crate::vodserve::OutputPreparation::CopyAndEncoded
                    && self
                        .job_authority
                        .may_execute_job(JobKind::EncodedOutputPrepare)
                        .await;
                match (copy, encoded) {
                    (true, true) => crate::vodserve::OutputPreparation::CopyAndEncoded,
                    (true, false) => crate::vodserve::OutputPreparation::Copy,
                    // An encoded-only authority is not a mode the switch has;
                    // claim nothing rather than invent one.
                    _ => crate::vodserve::OutputPreparation::Off,
                }
            }
        };
        crate::background_jobs::PreparationLanes {
            speculative,
            output,
        }
    }

    /// Cancel queued output-preparation rows the switch no longer admits.
    ///
    /// By cancelling, never by claiming: a claim needs target-node
    /// eligibility, scratch, and an admission permit, and a row the operator
    /// disabled may never satisfy them. `cancel_job` is one guarded update that
    /// takes a `queued` row straight to `cancelled`, so this runs on every
    /// node whatever the row's target, with nothing reserved. A row claimed
    /// concurrently goes to `cancelling` instead; its executor's heartbeat
    /// settles it with the cancel disposition, and if that executor is gone,
    /// store upkeep settles it at lease expiry. Bounded: 32 rows per kind per
    /// tick.
    pub(crate) async fn drain_disabled_output_preparation(
        &self,
        mode: crate::vodserve::OutputPreparation,
    ) -> usize {
        self.drain_disabled_output_preparation_with(mode, |_| std::future::ready(()))
            .await
    }

    /// [`Self::drain_disabled_output_preparation`] with a hook that runs
    /// between listing a queued row and cancelling it, so a test can claim
    /// the row in exactly that window. Production passes a no-op.
    pub(crate) async fn drain_disabled_output_preparation_with<F, Fut>(
        &self,
        mode: crate::vodserve::OutputPreparation,
        before_cancel: F,
    ) -> usize
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use plurx_core::store::background_jobs::{CancelJob, JobKind, JobQuery, JobState};
        const DRAIN_PER_KIND: usize = 32;
        let mut drained = 0;
        for kind in [JobKind::CopyOutputPrepare, JobKind::EncodedOutputPrepare] {
            if mode.job_kinds().contains(&kind) {
                continue;
            }
            let page = match self
                .store
                .list_jobs(JobQuery {
                    node_id: None,
                    state: Some(JobState::Queued),
                    kind: Some(kind),
                    after_id: None,
                    limit: DRAIN_PER_KIND,
                })
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    tracing::debug!(%error, ?kind, "could not list disabled output preparation");
                    continue;
                }
            };
            for job in page.jobs {
                before_cancel(job.id.clone()).await;
                match self
                    .store
                    .cancel_job(CancelJob {
                        job_id: job.id.clone(),
                        now_ms: clock_ms(),
                    })
                    .await
                {
                    Ok(Some(cancelled)) => {
                        drained += 1;
                        crate::telemetry::record_output_preparation_drained(kind);
                        // `cancelled` when it was still queued; `cancelling`
                        // when a node claimed it after the list, in which case
                        // its executor (or store upkeep at lease expiry)
                        // settles it.
                        tracing::info!(
                            job = %job.id,
                            ?kind,
                            state = ?cancelled.state,
                            reason = "output_preparation_disabled",
                            "cancelled queued output preparation the Developer switch no longer admits"
                        );
                    }
                    // Another node cancelled it first, or it settled.
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(job = %job.id, %error, "output preparation drain failed")
                    }
                }
            }
        }
        drained
    }

    async fn work_pretranscode_queue(
        self: Arc<Self>,
        transcode: Arc<TranscodeManager>,
        lanes: crate::background_jobs::PreparationLanes,
    ) {
        let Some((root, node)) = transcode.cache_location() else {
            return;
        };
        if self.producing.swap(true, Ordering::Relaxed) {
            return;
        }
        let _running = ProducingGuard(Arc::clone(&self));
        let mut lanes = lanes;
        const LOCAL_SWEEP_INTERVAL_MS: i64 = 15 * 60 * 1_000;
        let sweep_now = clock_ms();
        let previous = self
            .last_pretranscode_cache_sweep_ms
            .load(Ordering::Acquire);
        // The pre-transcode cache's own upkeep belongs to the speculative
        // lane: a node running only output preparation has no business
        // sweeping that cache.
        if lanes.speculative
            && sweep_now.saturating_sub(previous) >= LOCAL_SWEEP_INTERVAL_MS
            && self
                .last_pretranscode_cache_sweep_ms
                .compare_exchange(previous, sweep_now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // This runs even when the queue is empty. Every producer-enabled
            // node therefore maintains its own disk without multiplying work
            // on each scheduler tick.
            let sweep_started = std::time::Instant::now();
            tracing::info!(node, "pretranscode cachekeep sweep started");
            crate::cachekeep::sweep_with_readers(
                &self.store,
                root,
                node,
                transcode.cache_readers(),
                now(),
            )
            .await;
            tracing::info!(
                node,
                elapsed_ms = sweep_started.elapsed().as_millis() as u64,
                "pretranscode cachekeep sweep finished"
            );
        }
        // One read: the speculative lane's ceiling and the output lanes'
        // retained budget are the same key under the same unset rule.
        let stored_budget = match self.store.get_setting(keys::CACHE_MAX_GB).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "preparation queue cannot read its cache budget");
                return;
            }
        };
        let budget = crate::cachekeep::cache_budget(stored_budget.as_deref());
        let cache_ceiling = budget.map_or(0, |bytes| i64::try_from(bytes).unwrap_or(i64::MAX));
        let output_budget = budget.unwrap_or(0);
        if budget.is_none() {
            return;
        }

        let deadline = std::time::Instant::now() + PRODUCE_WINDOW;
        let mut produced = 0_u64;
        let mut skipped = 0_u64;
        let mut reasons = std::collections::BTreeMap::<&'static str, u64>::new();
        self.pretranscode_refusals
            .lock()
            .await
            .retain(|_, retry_at| *retry_at > clock_ms());
        for index in 0..PRODUCE_MAX_PER_PASS {
            if self.stop_producing.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline
            {
                break;
            }
            if !transcode.pretranscode_worker_idle() {
                *reasons.entry("foreground_or_offline_busy").or_default() += 1;
                break;
            }
            // `statvfs` is cheap enough to refresh after every title. Full
            // cache reconciliation stays on the cleanup schedule: doing its
            // database inventory and filesystem walk here would make an empty
            // queue tax every media-serving node once per scheduler tick.
            let global_remaining = match crate::playback_cache::lookup(root) {
                Some(budget) => match budget.available_bytes(root).await {
                    Ok(bytes) => i64::try_from(bytes).unwrap_or(i64::MAX),
                    Err(error) => {
                        tracing::warn!(%error, "playback cache capacity unavailable before queue claim");
                        *reasons.entry("cache_usage_unavailable").or_default() += 1;
                        break;
                    }
                },
                None => i64::MAX,
            };
            let remaining_budget = if lanes.speculative {
                let used_bytes = match self.store.cache_bytes(node).await {
                    Ok(bytes) => bytes.max(0),
                    Err(error) => {
                        tracing::warn!(%error, "could not read durable cache usage before queue claim");
                        *reasons.entry("cache_usage_unavailable").or_default() += 1;
                        break;
                    }
                };
                cache_ceiling
                    .saturating_sub(used_bytes)
                    .min(global_remaining)
            } else {
                0
            };
            if lanes.speculative && remaining_budget <= 0 {
                tracing::info!(
                    remaining_budget,
                    cache_ceiling,
                    "speculative worker stopped at cache budget"
                );
                *reasons.entry("cache_budget_full").or_default() += 1;
                // Output preparation also draws from global_remaining below;
                // no lane can advertise the same bytes independently.
                lanes.speculative = false;
                if lanes.is_empty() {
                    break;
                }
            }
            let output_remaining =
                i64::try_from(transcode.retained_output_remaining(output_budget))
                    .unwrap_or(i64::MAX)
                    .min(global_remaining);
            let capabilities = transcode.pretranscode_capabilities();
            // Requirements carry the estimated complete artifact plus 64 MiB
            // generation headroom. Each lane advertises only the lesser of
            // physical scratch and its own durable remainder, so a claim whose
            // expected publication is already known not to fit never starts.
            if capabilities.scratch_bytes == 0 {
                tracing::warn!(
                    decoders = ?capabilities.decoders,
                    scratch_bytes = capabilities.scratch_bytes,
                    "speculative worker has no proved decoder inventory or cache scratch"
                );
                *reasons.entry("unproved_capability_or_scratch").or_default() += 1;
                break;
            }
            let now_unix_ms = clock_ms();
            let excluded_job_ids = {
                let mut refusals = self.pretranscode_refusals.lock().await;
                refusals.retain(|_, retry_at| *retry_at > now_unix_ms);
                refusals
                    .keys()
                    .take(MAX_PRETRANSCODE_REFUSALS)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let allowed = lanes.kinds();
            let claim = match crate::background_jobs::claim_pretranscode(
                Arc::clone(&self.store),
                Arc::clone(&self.job_authority),
                &transcode,
                node,
                &allowed,
                &capabilities,
                remaining_budget,
                output_remaining,
                &excluded_job_ids,
            )
            .await
            {
                Ok(Some(claim)) => claim,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "could not claim speculative-transcode work");
                    *reasons.entry("claim_failed").or_default() += 1;
                    break;
                }
            };
            let (job, active, fence) = match claim {
                crate::background_jobs::PreparationClaim::Encoded(job, active, _admission) => {
                    let controller =
                        crate::background_jobs::BackgroundJobController::new(job, active);
                    let job = controller.job();
                    let fence = controller.fence();
                    let observation = transcode.copy_preparation_attachment_observation();
                    let result = {
                        let operation = async {
                            let plurx_core::store::background_jobs::JobPayload::EncodedOutputPrepare {
                                file_id, source_size, source_mtime, reason, intent, ..
                            } = job.supported_payload().map_err(|_| {
                                crate::background_jobs::PreparationError::Fail(
                                    "encoded_payload_unsupported",
                                )
                            })?
                            else {
                                return Err(crate::background_jobs::PreparationError::Fail(
                                    "encoded_payload_unsupported",
                                ));
                            };
                            let Some(file) =
                                crate::transcode::TranscodeManager::claimed_preparation_file(
                                    self.store.as_ref(),
                                    file_id,
                                    source_size,
                                    source_mtime,
                                    &fence,
                                )
                                .await?
                            else {
                                return Ok(false);
                            };
                            let item = self
                                .store
                                .get_item(file.item_id)
                                .await
                                .map_err(|error| error.to_string())?;
                            self.publish_preparation(&file, item.as_ref(), &reason, index)
                                .await;
                            let roots = match item {
                                Some(item) => self
                                    .store
                                    .get_library(item.library_id)
                                    .await
                                    .map_err(|error| error.to_string())?
                                    .map(|library| library.paths)
                                    .unwrap_or_default(),
                                None => Vec::new(),
                            };
                            #[cfg(target_os = "linux")]
                            let selected_static =
                                transcode.uses_selected_static_intent(&file, &intent);
                            #[cfg(not(target_os = "linux"))]
                            let selected_static = {
                                let _ = &intent;
                                false
                            };
                            if !selected_static {
                                let original =
                                    job.token.as_ref().ok_or("encoded claim token missing")?;
                                if fence.snapshot_for_claim(original).await.is_none() {
                                    return Err(crate::background_jobs::PreparationError::Stop(
                                        "ownership_lost",
                                    ));
                                }
                                crate::transcode::pretranscode_source_snapshot(&file, &roots)
                                    .await
                                    .ok_or("encoded source unavailable on owner")?;
                            }
                            transcode
                                .produce_encoded_output_job(
                                    &file,
                                    job,
                                    fence.clone(),
                                    deadline,
                                    observation,
                                )
                                .await
                        };
                        // Activity's Stop reaches a running preparation here:
                        // the watcher polls this every PRODUCER_POLL and
                        // abandons the operation, and the settle below
                        // cancels the job instead of yielding it.
                        crate::background_jobs::watch_copy_preparation(
                            &fence,
                            deadline.into(),
                            || {
                                !self.stop_producing.load(Ordering::Relaxed)
                                    && transcode.encoded_preparation_still_idle(observation)
                            },
                            operation,
                        )
                        .await
                    };
                    let reason = self
                        .settle_output_preparation(
                            job,
                            &fence,
                            "encoded_output",
                            result,
                            fence.loss_token().is_cancelled()
                                || std::time::Instant::now() >= deadline
                                || !transcode.encoded_preparation_still_idle(observation),
                            self.stop_producing.load(Ordering::Relaxed),
                        )
                        .await;
                    match reason {
                        None => produced += 1,
                        Some(reason) => {
                            skipped += 1;
                            *reasons.entry(reason).or_default() += 1;
                        }
                    }
                    drop(fence);
                    let _ = controller.finish().await;
                    continue;
                }
                crate::background_jobs::PreparationClaim::Transcode(job, active, fence) => {
                    (job, active, fence)
                }
                crate::background_jobs::PreparationClaim::Copy(job, active, admission) => {
                    let fence = active.fence();
                    let observation = transcode.copy_preparation_attachment_observation();
                    let result = {
                        let operation = async {
                            let payload = job.supported_payload().map_err(|_| {
                                crate::background_jobs::PreparationError::Fail(
                                    "copy_payload_unsupported",
                                )
                            })?;
                            let plurx_core::store::background_jobs::JobPayload::CopyOutputPrepare {
                                file_id,
                                source_size,
                                source_mtime,
                                reason,
                                ..
                            } = payload
                            else {
                                return Err(crate::background_jobs::PreparationError::Fail(
                                    "copy_payload_unsupported",
                                ));
                            };
                            let Some(file) =
                                crate::transcode::TranscodeManager::claimed_preparation_file(
                                    self.store.as_ref(),
                                    file_id,
                                    source_size,
                                    source_mtime,
                                    &fence,
                                )
                                .await?
                            else {
                                return Ok(false);
                            };
                            let item = self
                                .store
                                .get_item(file.item_id)
                                .await
                                .map_err(|error| error.to_string())?;
                            self.publish_preparation(&file, item.as_ref(), &reason, index)
                                .await;
                            let roots = match item {
                                Some(item) => self
                                    .store
                                    .get_library(item.library_id)
                                    .await
                                    .map_err(|error| error.to_string())?
                                    .map(|library| library.paths)
                                    .unwrap_or_default(),
                                None => Vec::new(),
                            };
                            // Preserve the existing worker's configured-library
                            // boundary before the independently held source open.
                            let original_claim = job.token.as_ref().ok_or(
                                crate::background_jobs::PreparationError::Fail(
                                    "copy_claim_token_missing",
                                ),
                            )?;
                            if fence.snapshot_for_claim(original_claim).await.is_none() {
                                return Err(crate::background_jobs::PreparationError::Yield(
                                    "lease_lost",
                                ));
                            }
                            crate::transcode::pretranscode_source_snapshot(&file, &roots)
                                .await
                                .ok_or("copy source unavailable on owner")?;
                            transcode
                                .produce_copy_output_job(
                                    &file,
                                    &job,
                                    fence.clone(),
                                    &admission,
                                    deadline,
                                )
                                .await
                        };
                        crate::background_jobs::watch_copy_preparation(
                            &fence,
                            deadline.into(),
                            || {
                                !self.stop_producing.load(Ordering::Relaxed)
                                    && transcode
                                        .copy_preparation_still_idle(&admission, observation)
                            },
                            operation,
                        )
                        .await
                    };
                    let reason = self
                        .settle_output_preparation(
                            &job,
                            &fence,
                            "copy_output",
                            result,
                            fence.loss_token().is_cancelled()
                                || std::time::Instant::now() >= deadline
                                || !transcode.copy_preparation_still_idle(&admission, observation),
                            self.stop_producing.load(Ordering::Relaxed),
                        )
                        .await;
                    match reason {
                        None => produced += 1,
                        Some(reason) => {
                            skipped += 1;
                            *reasons.entry(reason).or_default() += 1;
                        }
                    }
                    active.finish().await;
                    continue;
                }
            };
            let lost = active.fence().loss_token();
            let file = match self.store.get_file(job.file_id).await {
                Ok(Some(file))
                    if file.size == job.source_size && file.mtime == job.source_mtime =>
                {
                    file
                }
                Ok(Some(_)) | Ok(None) => {
                    crate::store_result::observe(
                        crate::store_result::Operation::CancelTranscodeSourceChanged,
                        crate::store_result::Discard::Cancelled,
                        fence
                            .cancel_job(self.store.as_ref(), "source_changed", clock_ms())
                            .await,
                    );
                    active.finish().await;
                    skipped += 1;
                    *reasons.entry("source_changed").or_default() += 1;
                    continue;
                }
                Err(error) => {
                    let now_unix_ms = clock_ms();
                    tracing::warn!(job = job.id, %error, "source row unavailable for speculative transcode");
                    let _ = fence
                        .yield_job(
                            "store_unavailable",
                            self.store.as_ref(),
                            now_unix_ms,
                            now_unix_ms.saturating_add(5_000),
                        )
                        .await;
                    active.finish().await;
                    skipped += 1;
                    *reasons.entry("store_unavailable").or_default() += 1;
                    continue;
                }
            };
            let trusted_roots = match self.store.get_item(file.item_id).await {
                Ok(Some(item)) => self
                    .store
                    .get_library(item.library_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|library| library.paths)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            if fence.snapshot().await.is_none() {
                active.finish().await;
                skipped += 1;
                *reasons.entry("lease_lost").or_default() += 1;
                continue;
            }
            let Some(source_snapshot) =
                crate::transcode::pretranscode_source_snapshot(&file, &trusted_roots).await
            else {
                let now_unix_ms = clock_ms();
                {
                    let mut refusals = self.pretranscode_refusals.lock().await;
                    refusals.retain(|_, retry_at| *retry_at > now_unix_ms);
                    if refusals.len() >= MAX_PRETRANSCODE_REFUSALS {
                        if let Some(oldest) = refusals
                            .iter()
                            .min_by_key(|(_, retry_at)| **retry_at)
                            .map(|(id, _)| id.clone())
                        {
                            refusals.remove(&oldest);
                        }
                    }
                    refusals.insert(
                        job.id.clone(),
                        now_unix_ms.saturating_add(PRETRANSCODE_REFUSAL_TTL_MS),
                    );
                }
                let _ = fence
                    .yield_job(
                        "source_unreadable_on_node",
                        self.store.as_ref(),
                        now_unix_ms,
                        now_unix_ms,
                    )
                    .await;
                active.finish().await;
                skipped += 1;
                *reasons.entry("source_unreadable_on_node").or_default() += 1;
                tracing::debug!(
                    job = job.id,
                    path = %file.path.display(),
                    "speculative transcode refused on a node without the source snapshot"
                );
                continue;
            };
            let title = self
                .store
                .get_item(file.item_id)
                .await
                .ok()
                .flatten()
                .map_or_else(|| file.path.display().to_string(), |item| item.title);
            self.set_producing(Some(ProducingNow {
                title: title.clone(),
                reason: job.reason.clone(),
                index: index + 1,
                total: PRODUCE_MAX_PER_PASS,
            }))
            .await;
            let result = transcode
                .produce_pretranscode_job(
                    &file,
                    job.target_height,
                    deadline,
                    &lost,
                    source_snapshot,
                    fence.clone(),
                )
                .await;
            match result {
                Ok(PretranscodeProduceOutcome::Ready(made)) => {
                    produced += 1;
                    tracing::info!(
                        job = job.id,
                        recipe = %made.recipe,
                        title = %title,
                        reason = job.reason,
                        height = job.target_height,
                        segments = made.segments,
                        mb = made.bytes / 1_048_576,
                        parts = made.parts,
                        "distributed speculative transcode ready"
                    );
                }
                Ok(PretranscodeProduceOutcome::Yielded(_)) if lost.is_cancelled() => {
                    skipped += 1;
                    *reasons.entry("lease_lost").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::Yielded(reason)) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .yield_job(
                            reason,
                            self.store.as_ref(),
                            now_unix_ms,
                            now_unix_ms.saturating_add(1_000),
                        )
                        .await;
                    skipped += 1;
                    *reasons.entry("yielded").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::StoreUnavailable) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .yield_job(
                            "store_unavailable",
                            self.store.as_ref(),
                            now_unix_ms,
                            now_unix_ms.saturating_add(5_000),
                        )
                        .await;
                    skipped += 1;
                    *reasons.entry("store_unavailable").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::PolicyChanged) => {
                    let now_unix_ms = clock_ms();
                    crate::store_result::observe(
                        crate::store_result::Operation::CancelTranscodePolicyChanged,
                        crate::store_result::Discard::Cancelled,
                        fence
                            .cancel_job(self.store.as_ref(), "policy_changed", now_unix_ms)
                            .await,
                    );
                    skipped += 1;
                    *reasons.entry("policy_changed").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::SourceChanged) => {
                    let now_unix_ms = clock_ms();
                    crate::store_result::observe(
                        crate::store_result::Operation::CancelTranscodeSourceChanged,
                        crate::store_result::Discard::Cancelled,
                        fence
                            .cancel_job(self.store.as_ref(), "source_changed", now_unix_ms)
                            .await,
                    );
                    skipped += 1;
                    *reasons.entry("source_changed").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::HealthRefused) => {
                    // Cancelled, not yielded, and deliberately not with a
                    // re-enqueueable error code. The same plan on the same
                    // source reaches the same decoder and settles the same
                    // refused receipt, so a job that retried this would
                    // re-encode the title on every discovery pass forever.
                    let now_unix_ms = clock_ms();
                    crate::store_result::observe(
                        crate::store_result::Operation::CancelTranscodeHealthRefused,
                        crate::store_result::Discard::Cancelled,
                        fence
                            .cancel_job(self.store.as_ref(), "health_refused", now_unix_ms)
                            .await,
                    );
                    skipped += 1;
                    *reasons.entry("health_refused").or_default() += 1;
                }
                Err(error) if crate::transcode::is_retryable_capacity_error(&error) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .yield_job(
                            "capacity",
                            self.store.as_ref(),
                            now_unix_ms,
                            now_unix_ms.saturating_add(5_000),
                        )
                        .await;
                    skipped += 1;
                    *reasons.entry("capacity").or_default() += 1;
                }
                Err(error) => {
                    let now_unix_ms = clock_ms();
                    let backoff_ms = crate::background_jobs::retry_delay_ms(&job.id, job.attempts);
                    let _ = fence
                        .fail_job(
                            self.store.as_ref(),
                            "encode_failed",
                            now_unix_ms,
                            now_unix_ms.saturating_add(backoff_ms),
                        )
                        .await;
                    skipped += 1;
                    *reasons.entry("failed").or_default() += 1;
                    tracing::warn!(job = job.id, title = %title, %error, "speculative transcode failed");
                }
            }
            active.finish().await;
            self.set_producing(None).await;
        }
        self.emit_producer_pass(produced, skipped, serde_json::json!(reasons));
    }

    /// Settle one copy/encoded output preparation by outcome class. Returns
    /// `None` when the output was published, else the pass-telemetry reason.
    /// `Ok(false)` means the callee either settled the row itself (a stop the
    /// fence then refuses to overwrite) or the staged body lost its exposure
    /// race; a yield is correct for the latter and harmless for the former.
    async fn settle_output_preparation(
        &self,
        job: &plurx_core::store::background_jobs::BackgroundJob,
        fence: &crate::background_jobs::JobFence,
        kind: &'static str,
        result: Result<bool, crate::background_jobs::PreparationError>,
        preempted: bool,
        operator_stopped: bool,
    ) -> Option<&'static str> {
        let error = match result {
            Ok(true) => return None,
            Ok(false) => crate::background_jobs::PreparationError::Yield("output_not_published"),
            Err(error) => error,
        };
        if operator_stopped {
            // Activity's Stop on a preparation cancels it (ruling 3 of the
            // main-merge defects build): this node only, and the title's next
            // play may queue it again.
            tracing::info!(job = %job.id, kind, %error, "output preparation stopped by an operator");
            if let Err(store_error) = fence
                .settle(plurx_core::store::background_jobs::JobSettlement::Cancel)
                .await
            {
                tracing::warn!(job = %job.id, kind, %store_error, "output preparation cancel failed");
            }
            return Some("operator_stopped");
        }
        let retry_code = if kind == "encoded_output" {
            "encoded_output_failed"
        } else {
            "copy_output_failed"
        };
        let (settlement, reason) = error.settlement(job, retry_code, preempted, clock_ms());
        let disposition = match &settlement {
            plurx_core::store::background_jobs::JobSettlement::Yield { .. } => "yield",
            plurx_core::store::background_jobs::JobSettlement::Retry { .. } => "retry",
            plurx_core::store::background_jobs::JobSettlement::Fail { .. } => "fail",
            plurx_core::store::background_jobs::JobSettlement::Stop { .. } => "stop",
            plurx_core::store::background_jobs::JobSettlement::Cancel => "cancel",
        };
        if disposition == "yield" {
            tracing::debug!(job = %job.id, kind, %error, disposition, "output preparation settled");
        } else {
            tracing::warn!(
                job = %job.id,
                kind,
                %error,
                disposition,
                failed_attempts = job.failed_attempts,
                attempt_limit = job.attempt_limit,
                "output preparation did not publish"
            );
        }
        if let Err(store_error) = fence.settle(settlement).await {
            tracing::warn!(job = %job.id, kind, %store_error, "output preparation settlement failed");
        }
        Some(reason)
    }

    /// Publish the output preparation this pass is running, so Activity's
    /// producer banner and its Stop cover it like a speculative title.
    async fn publish_preparation(
        &self,
        file: &plurx_core::domain::MediaFile,
        item: Option<&plurx_core::domain::Item>,
        reason: &str,
        index: usize,
    ) {
        self.set_producing(Some(ProducingNow {
            title: item.map_or_else(
                || file.path.display().to_string(),
                |item| item.title.clone(),
            ),
            reason: reason.to_owned(),
            index: index + 1,
            total: PRODUCE_MAX_PER_PASS,
        }))
        .await;
    }

    fn emit_producer_pass(&self, produced: u64, skipped: u64, reasons: serde_json::Value) {
        crate::telemetry::emit(
            Arc::clone(&self.store),
            PlaybackEvent {
                at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0),
                event: "producer_pass".to_owned(),
                extra: Some(
                    serde_json::json!({
                        "produced": produced,
                        "skipped": skipped,
                        "reasons": reasons,
                    })
                    .to_string(),
                ),
                ..PlaybackEvent::default()
            },
        );
    }

    /// Give every enriched item that still has incomplete artwork another go.
    ///
    /// The self-healing half of the artwork fix: §2 records *that* a download
    /// failed, this is what comes back for it. Forced, because these items
    /// already carry `metadata_at` and the ordinary queue would skip them
    /// forever — which is precisely how they got here.
    ///
    /// Grouped by library because that is the unit that knows which provider
    /// to ask. The per-item backoff, not this interval, is what stops a
    /// permanently art-less item from being re-fetched every half hour.
    #[cfg(test)]
    pub async fn sweep_artwork(&self) -> Result<usize, plurx_core::error::StoreError> {
        Ok(self
            .sweep_artwork_with_backoff(keys::ARTWORK_RETRY_BACKOFF_SECS)
            .await?
            .repaired)
    }

    /// The retry pass with an injectable backoff for fairness tests.
    #[cfg(test)]
    async fn sweep_artwork_with_backoff(
        &self,
        retry_after_secs: i64,
    ) -> Result<ArtworkSweepResult, plurx_core::error::StoreError> {
        let Some(lease) = self.acquire_job("provider:artwork".to_owned()).await? else {
            return Ok(ArtworkSweepResult::default());
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let result = tokio::select! {
            result = self.sweep_artwork_with_publication(retry_after_secs, &publisher) => result,
            () = lost.cancelled() => Err(StoreError::Task(
                "cluster artwork lease was lost".to_owned(),
            )),
        };
        drop(publisher);
        let _ = lease.release().await;
        result
    }

    async fn sweep_artwork_with_publication(
        &self,
        retry_after_secs: i64,
        publisher: &PublicationStore<'_>,
    ) -> Result<ArtworkSweepResult, plurx_core::error::StoreError> {
        let mut items = self
            .store
            .items_missing_artwork(None, retry_after_secs, ARTWORK_RETRY_BATCH)
            .await?;
        if items.is_empty() {
            return Ok(ArtworkSweepResult::default());
        }

        // Never spend the tail of a batch retrying an old row while a never-
        // attempted row is still entering the queue. SQL sorts NULL stamps
        // first; if both classes fit in this result, take only the fresh
        // prefix. The next pass finishes the initial backlog before normal
        // retry rotation begins, even when tests/operators use zero backoff.
        if items[0].artwork_attempted_at.is_none() {
            if let Some(first_retry) = items
                .iter()
                .position(|item| item.artwork_attempted_at.is_some())
            {
                items.truncate(first_retry);
            }
        }
        let attempted = items.len();
        #[cfg(test)]
        let claimed_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        // Stable library order makes partial progress and logs reproducible if
        // one library disappears or SQLite rejects one of the later reads.
        let mut by_library: BTreeMap<i64, Vec<Item>> = BTreeMap::new();
        for item in items {
            by_library.entry(item.library_id).or_default().push(item);
        }
        let mut repaired = 0usize;
        let mut still_missing = 0usize;
        let mut provider_errors = 0usize;
        for (library_id, candidates) in by_library {
            let library = match self.store.get_library(library_id).await {
                Ok(Some(library)) => library,
                Ok(None) => {
                    tracing::warn!(
                        library = library_id,
                        candidates = candidates.len(),
                        "artwork retry library disappeared"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        library = library_id,
                        candidates = candidates.len(),
                        error = %e,
                        "artwork retry could not read library"
                    );
                    continue;
                }
            };
            // Metadata providers enter a TV tree through the show, never
            // through a season/episode row. Carry each missing child and all
            // of its ancestors so `enrich_library` selects the show while
            // `enrich_episodes` remains narrowed to exactly the affected
            // seasons and episodes.
            let ids: Vec<i64> = candidates.iter().map(|item| item.id).collect();
            let targets = self.enrich_targets(&ids).await;
            let outcome = self
                .enrich(publisher, &library, true, Some(&targets), Some(&ids), None)
                .await;
            provider_errors += outcome.enrich.as_ref().map_or(0, |r| r.errors);

            // Count from persisted state, not provider-level matches: a show
            // can be a route for twenty candidate episodes without itself
            // being one repaired row. Attempt stamps are written only inside
            // provider paths that know an image was requested or the provider
            // successfully answered that no matching image/episode exists.
            // Errors and a missing API key deliberately remain unstamped so
            // the next scheduled pass can retry them in thirty minutes.
            for before in candidates {
                let after = match self.store.get_item(before.id).await {
                    Ok(Some(after)) => after,
                    Ok(None) => {
                        tracing::warn!(
                            item_id = before.id,
                            "artwork retry candidate disappeared after enrichment"
                        );
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(
                            item_id = before.id,
                            error = %e,
                            "artwork retry could not read candidate after enrichment"
                        );
                        continue;
                    }
                };
                if !needs_artwork_retry(&after) {
                    repaired += 1;
                } else {
                    still_missing += 1;
                }
            }
        }
        tracing::info!(
            attempted,
            repaired,
            still_missing,
            provider_errors,
            "artwork retry sweep finished"
        );
        Ok(ArtworkSweepResult {
            repaired,
            #[cfg(test)]
            claimed_ids,
        })
    }

    /// A minutes-interval setting; absent, blank or unparseable reads as off.
    /// Deliberately not an error: a hand-edited settings row should stop one
    /// job, not the server.
    async fn job_interval(&self, key: &str) -> i64 {
        self.job_interval_or(key, 0).await
    }

    /// [`job_interval`](Self::job_interval) with a different reading of
    /// "absent". A stored `0` still means off — an admin who turned a job off
    /// must not have it turned back on by a default.
    async fn job_interval_or(&self, key: &str, default_value: i64) -> i64 {
        self.store
            .get_setting(key)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default_value)
            .max(0)
    }

    async fn job_stamp(&self, key: &str) -> Option<i64> {
        self.store
            .get_setting(key)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i64>().ok())
    }

    async fn stamp(&self, key: &str, publisher: &PublicationStore<'_>) {
        if let Err(e) = publisher.put_setting(key, &now().to_string()).await {
            tracing::warn!(error = %e, key, "recording a job run time failed");
        }
    }

    fn local_job_key(&self, key: &str) -> String {
        format!("{key}.node.{}", self.coordinator.node_id())
    }

    async fn stamp_local(&self, key: &str) {
        let key = self.local_job_key(key);
        if let Err(e) = self.store.put_setting(&key, &now().to_string()).await {
            tracing::warn!(error = %e, key, "recording a node-local job run time failed");
        }
    }

    async fn finish(&self, library_id: i64, mut status: ScanStatus) {
        status.running = false;
        status.phase = None;
        status.progress = None;
        if status.finished_at.is_none() {
            status.finished_at = Some(now());
        }
        self.live.lock().await.remove(&library_id);
        self.statuses.lock().await.insert(library_id, status);
    }
}

fn error_status(message: &str) -> ScanStatus {
    ScanStatus {
        running: false,
        finished_at: Some(now()),
        error: Some(message.to_owned()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
