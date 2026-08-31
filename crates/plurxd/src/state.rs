//! Shared application state and the background job manager.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use plurx_core::cluster::coordination::LeaseClaim;
use plurx_core::cluster::coordination::{ClusterJobAuthority, StoreCoordinator};
#[cfg(test)]
use plurx_core::domain::ArtworkAttempt;
use plurx_core::domain::{
    BookMetadataPatch, BookMetadataSource, Item, ItemKind, Library, LibraryKind, MediaFile,
    MetadataPatch, NewPretranscodeJob, OfflinePackageStats, PlaybackEvent, PretranscodeJob,
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
    ArtworkRepairFence, CatalogueReader, ClusterFragmentIndexArtifact,
    ClusterFragmentIndexLocation, NewAnalysisRequest, NewClusterFragmentIndexJob,
    PrometheusStoreSnapshot, PublicationStore, Store,
};
use plurx_core::transcode::EncoderCaps;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::job_lease::{acquire_cluster_job, ActiveJobLease};
use crate::logbuf::{LogBuffer, LogBuffers};
use crate::offline::OfflineManager;
use crate::schedule::{due_jobs, DueJob, GlobalSchedule};
use crate::trakt::TraktManager;
use crate::transcode::{PretranscodeFence, PretranscodeProduceOutcome, TranscodeManager};

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
    /// PLURX_HWACCEL preference, or "auto".
    pub hwaccel_pref: String,
    pub encoders: EncoderCaps,
    /// Portable video decoders reported by this exact ffmpeg at boot.
    pub decoders: Vec<String>,
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
    /// Whether this build converts Dolby Vision Profile 7 to Profile 8.1 on
    /// the way through a copy (PLAYBACK-CAPS-V2-PLAN §4.8).
    ///
    /// Not a probe: the conversion is plurx's own code, so the answer is
    /// "this binary has it" — which is always true — narrowed by the
    /// `playback.dv_convert` setting an operator can turn off. It sits with
    /// the probes because a client asking why a title played as HDR10 rather
    /// than Dolby Vision needs all four answers in one place.
    pub dolby_vision_convert: bool,
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
    analysis_queue_depth: [AtomicI64; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
    analysis_queue_oldest_age_seconds: [AtomicI64; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
    analysis_lifecycle_counts: [AtomicI64; plurx_core::store::ANALYSIS_LIFECYCLE_METRIC_SLOTS],
    analysis_marker_counts: [AtomicI64; plurx_core::store::ANALYSIS_MARKER_METRIC_SLOTS],
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
            analysis_queue_depth: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_queue_oldest_age_seconds: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_lifecycle_counts: std::array::from_fn(|_| AtomicI64::new(0)),
            analysis_marker_counts: std::array::from_fn(|_| AtomicI64::new(0)),
        }
    }
}

/// Lock-free view consumed by the Prometheus handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreMetricsView {
    pub sample: Option<PrometheusStoreSnapshot>,
    pub age_seconds: Option<u64>,
    pub valid: bool,
    pub errors: u64,
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
                },
            };
            let after = self.inner.sequence.load(Ordering::Acquire);
            if before == after {
                let age_seconds = published.then(|| elapsed.saturating_sub(sampled_elapsed));
                return StoreMetricsView {
                    sample: published.then_some(sample),
                    age_seconds,
                    valid: age_seconds.is_some_and(|age| age <= STORE_METRICS_FRESHNESS_SECS),
                    errors,
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
    pub store: Arc<dyn Store>,
    /// Named Authority/BoundedReplica boundary for eligible catalogue reads.
    pub catalogue: CatalogueReader,
    /// Read-only projection of the selected backend's watch-state convergence.
    pub replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    /// Monotonic, Store-free authority for mutable media and readiness.
    pub(crate) serving: crate::serving_fence::ServingFence,
    /// Join/add/remove lifecycle and privacy-safe per-node health.
    pub membership: plurx_core::cluster::membership::MembershipManager,
    /// Bounded authenticated client for node-local activity snapshots.
    #[allow(dead_code)] // aggregation child #326 is the first consumer
    pub peer_activity: crate::http::internal_activity::PeerActivityClient,
    /// Fresh, authenticated media-capability snapshots and diagnostics-only
    /// placement offers. P4 observes candidates; it never starts a session.
    pub(crate) media_pool: Arc<crate::media_pool::MediaPool>,
    /// Authenticated remote start/abort and streaming HLS relay transport.
    pub(crate) media_sessions: Arc<crate::media_sessions::MediaSessionCoordinator>,
    pub server_name: String,
    /// Stable identity of the node that owns local transcode/offline bytes.
    pub node_id: String,
    /// Whether LAN discovery must distinguish this node from cluster peers.
    pub cluster_advertisement: bool,
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
    /// Where extracted subtitles are kept, keyed by file identity and source
    /// fingerprint — see `http::stream::subtitles_vtt`.
    pub subs_dir: PathBuf,
    /// Staged rollout gate for the authenticated `pgs-v1` overlay API. The
    /// daemon only advertises the capability when the same process will serve
    /// it. Default-off until physical-client acceptance is complete.
    pub pgs_overlay_enabled: bool,
    pub jobs: Arc<JobManager>,
    pub transcode: Arc<TranscodeManager>,
    pub offline: Arc<OfflineManager>,
    /// Short-lived, revision-bound EPUB resource capabilities. Publication
    /// markup receives one of these, never the user's reusable API token.
    pub publications: Arc<crate::http::publication::PublicationSessions>,
    pub trakt: Arc<TraktManager>,
    pub system: Arc<SystemInfo>,
    pub logs: Arc<LogBuffer>,
    /// Replication/membership detail kept out of the general diagnostics ring.
    pub cluster_logs: Arc<LogBuffer>,
    /// The coming-soon rail's cached answer from monarr (plan §11.2).
    pub coming_soon: Arc<crate::http::ComingSoonCache>,
    /// Pushes watch state to monarr when enabled (plan §11.1).
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
    /// Keeps the click path off the NAS, and announces a start once playback
    /// is real rather than once a decision has been made.
    pub availability: Arc<crate::playstart::AvailabilityCache>,
    pub starts: Arc<crate::playstart::StartNotifier>,
    /// Live telemetry for progressive `/stream.mp4` remuxes, which are not
    /// transcode sessions and so have nowhere else to report from.
    pub streams: Arc<crate::progressive::Streams>,
    /// Deliveries that keep no record of themselves — direct play. The other
    /// three routes are listed from the machinery that already owns their
    /// lifetimes, so this holds only what would otherwise be invisible; see
    /// [`crate::delivery`].
    pub direct_plays: Arc<crate::delivery::DirectPlays>,
    /// Store-backed gauges sampled away from the Prometheus request path.
    pub store_metrics: StoreMetricsCache,
    /// Application-initiated graceful drain. Signals still use the process
    /// watcher in `main`; the cluster leave endpoint cancels this only after
    /// its own voter removal has committed.
    pub shutdown: tokio_util::sync::CancellationToken,
    pub started_at: Instant,
}

impl AppState {
    /// `node_id` is this server's stable id — the `node_id` a cache location
    /// is recorded against, so a cluster can tell whose copy is whose.
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
        let catalogue = CatalogueReader::authority(Arc::clone(&store));
        Self::new_configured(
            AppConfig {
                server_name,
                node_id,
                cluster_advertisement: false,
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
            },
            store,
            dirs,
            encoder_caps,
            system,
            LogBuffers {
                general: logs,
                cluster: Arc::new(LogBuffer::default()),
            },
        )
    }

    pub fn new_configured(
        config: AppConfig,
        store: Arc<dyn Store>,
        dirs: Dirs,
        encoder_caps: EncoderCaps,
        system: SystemInfo,
        logs: LogBuffers,
    ) -> Self {
        let AppConfig {
            server_name,
            node_id,
            cluster_advertisement,
            scan_prune_percent,
            credential_key,
            replication,
            membership,
            cluster_id,
            shared_cache_dir,
            shared_cache_id,
            catalogue,
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
        let jobs = Arc::new(
            JobManager::new_with_scan_prune_percent(
                Arc::clone(&store),
                artwork_dir.clone(),
                scan_prune_percent,
                node_id.clone(),
                Arc::new(membership.clone()),
            )
            .with_membership(membership.clone()),
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
            .with_decoders(system.decoders.clone())
            .with_dv_strippable(system.dovi_rpu)
            .with_dovi_reshape(system.dovi_reshape)
            .with_dovi_passthrough(system.dovi_passthrough)
            .with_dovi_passthrough_qsv(system.dovi_passthrough_qsv)
            .with_hdr10_passthrough(system.hdr10_passthrough)
            .with_hdr10_passthrough_qsv(system.hdr10_passthrough_qsv)
            .with_cache_layout(
                cache_dir.clone(),
                runtime_cache.clone(),
                subs_dir.clone(),
                renditions,
                system.ffmpeg_version.clone().unwrap_or_default(),
                node_id.clone(),
                Some(membership.clone()),
            )
            .with_serving_authority(serving.authority())
            .with_shared_cache(Arc::clone(&shared_cache)),
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
        AppState {
            store,
            catalogue,
            replication,
            peer_activity: crate::http::internal_activity::PeerActivityClient::new(
                membership.clone(),
            ),
            serving,
            membership,
            media_pool,
            media_sessions,
            server_name,
            node_id,
            cluster_advertisement,
            artwork_dir,
            artwork_fetch: crate::http::images::ArtworkCoordinator::new(),
            cache_dir,
            runtime_cache_dir: runtime_cache,
            shared_cache,
            subs_dir,
            pgs_overlay_enabled: std::env::var("PLURX_PGS_OVERLAY").is_ok_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            }),
            jobs,
            transcode,
            offline,
            publications: crate::http::publication::PublicationSessions::new(),
            trakt,
            system: Arc::new(system),
            logs: logs.general,
            cluster_logs: logs.cluster,
            coming_soon,
            watched,
            progress,
            storage: Arc::new(tokio::sync::RwLock::new(Default::default())),
            availability: Arc::new(crate::playstart::AvailabilityCache::new()),
            starts: Arc::new(crate::playstart::StartNotifier::new()),
            streams: crate::progressive::Streams::new(),
            direct_plays: crate::delivery::DirectPlays::new(),
            store_metrics: StoreMetricsCache::default(),
            shutdown: tokio_util::sync::CancellationToken::new(),
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

    /// Keep the scrape snapshot fresh without coupling availability to a
    /// Store or leader round trip. The first sample starts immediately.
    pub async fn store_metrics_loop(self) {
        let stagger = self.node_id.bytes().fold(0_u64, |hash, byte| {
            hash.wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(byte))
        }) % 15;
        let base_interval = Duration::from_secs(30 + stagger);
        let mut consecutive_errors = 0_u32;
        loop {
            if let Err(error) = self.refresh_store_metrics().await {
                consecutive_errors = consecutive_errors.saturating_add(1);
                tracing::debug!(%error, "refreshing Store-backed metrics snapshot failed");
            } else {
                consecutive_errors = 0;
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

pub struct AppConfig {
    pub server_name: String,
    pub node_id: String,
    pub cluster_advertisement: bool,
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
}

/// Status of the most recent (or in-flight) scan for one library.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanStatus {
    pub running: bool,
    /// What the job is doing right now: "scanning" or "enriching".
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

pub struct JobManager {
    store: Arc<dyn Store>,
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
    /// Targeted scans waiting for a library's running scan to finish,
    /// per library. See [`JobManager::request_scan`].
    pending: Mutex<HashMap<i64, Vec<ScanRequest>>>,
    /// At most one delayed remote-lease retry per library. Without this,
    /// several integration requests arriving during the same remote scan
    /// would each perpetuate its own two-second retry task.
    pending_retries: Mutex<HashSet<i64>>,
    /// Recent targeted-scan requests and their outcomes, newest last.
    requests: Mutex<VecDeque<ScanRequestRecord>>,
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
    /// Throttle bounded, replicated analysis-history pruning so an idle queue
    /// does not produce a Raft write on every scheduler tick.
    last_analysis_prune_ms: AtomicI64,
    /// High-frequency worker progress is deliberately node-local. Replicating
    /// every fragment would turn one long media read into sustained Raft load;
    /// the bounded peer Activity snapshot carries these rows to an ingress.
    analysis_progress: std::sync::Mutex<HashMap<(String, String), AnalysisProgress>>,
    analysis_progress_epoch: AtomicU64,
    analysis_metrics: Arc<AnalysisRuntimeMetrics>,
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
    /// Per-node maintenance cadence for node-local cache bytes. Candidate
    /// generation is cluster-singleton and cannot maintain every worker disk.
    last_pretranscode_cache_sweep_ms: AtomicI64,
    /// Stable local cursor for the content-addressed index cache. Without a
    /// cursor each bounded pass would revisit the same legitimate head page.
    fragment_index_sweep_cursor: Mutex<Option<String>>,
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
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub elapsed_ms: i64,
    pub throughput_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<i64>,
    #[serde(skip)]
    registry_epoch: u64,
}

#[cfg(test)]
impl AnalysisProgress {
    pub(crate) fn test_row(job_id: &str, title: &str) -> Self {
        Self {
            job_id: job_id.to_owned(),
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
            started_at_ms: 1,
            updated_at_ms: 1,
            elapsed_ms: 1,
            throughput_bps: 1,
            eta_ms: Some(1),
            registry_epoch: 0,
        }
    }
}

const ANALYSIS_STAGES: [&str; 6] = [
    "probing",
    "fragment_index",
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

#[derive(Default)]
pub(crate) struct AnalysisRuntimeMetrics {
    current_by_stage: [AtomicU64; ANALYSIS_STAGES.len()],
    bytes: AnalysisHistogram,
    media_seconds: AnalysisHistogram,
    wall_seconds: AnalysisHistogram,
    throughput: AnalysisHistogram,
    publications: [[AtomicU64; 2]; 2],
    correlations: [AtomicU64; 3],
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
        out
    }
}

struct AnalysisProgressGuard {
    jobs: Arc<JobManager>,
    job_id: String,
    target_node_id: String,
    registry_epoch: u64,
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
/// 14x Dolby Vision copy on nynuc needs about nine minutes for a two-hour film.
const INDEX_FILE_BUDGET_FLOOR_SECS: u64 = 90;
const INDEX_FILE_BUDGET_CEILING_SECS: u64 = 30 * 60;
const INDEX_EXPECTED_MIN_SPEED: u64 = 8;
const INDEX_FILE_HEADROOM_SECS: u64 = 30;

fn index_file_budget(duration_ms: Option<i64>) -> Duration {
    let film_secs = duration_ms
        .filter(|milliseconds| *milliseconds > 0)
        .map_or(0, |milliseconds| (milliseconds as u64).div_ceil(1_000));
    let estimated = film_secs
        .div_ceil(INDEX_EXPECTED_MIN_SPEED)
        .saturating_add(INDEX_FILE_HEADROOM_SECS);
    Duration::from_secs(
        estimated.clamp(INDEX_FILE_BUDGET_FLOOR_SECS, INDEX_FILE_BUDGET_CEILING_SECS),
    )
}

/// Stable, wrapping order for one bounded pass. The cursor is the last file a
/// prior pass examined, successful or not; starting after it prevents a few
/// permanently slow/unsupported rows from starving every later title.
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
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnalysisRetryPolicy {
    lease_ms: i64,
    backoff_base_ms: i64,
    backoff_max_ms: i64,
}

#[derive(Clone)]
struct ClusterFragmentIndexWorker {
    engine_sha256: String,
    cache_root: PathBuf,
    have_dovi: bool,
    retry_policy: AnalysisRetryPolicy,
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
        Self {
            lease_ms,
            backoff_base_ms,
            backoff_max_ms,
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

struct ClusterIndexWorkingGuard(Arc<JobManager>);

impl Drop for ClusterIndexWorkingGuard {
    fn drop(&mut self) {
        self.0.cluster_index_working.store(false, Ordering::Relaxed);
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

/// How many rows to take off each rail, per user. A rail is a prediction and
/// the tail of one is a weak prediction; the head is where the value is.
const PRODUCE_RAIL: i64 = 5;

/// Ceiling on what one pass will attempt, across every user and rail.
const PRODUCE_MAX_PER_PASS: usize = 12;
/// Hard fan-out bounds for one singleton discovery pass. The rotating user
/// cursor gives every account a turn without an all-users allocation.
const PRODUCE_USER_PAGE: i64 = 64;
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

/// Ids the caller already knows, so plurx does not have to guess.
///
/// Without these, matching is title+year parsed off a filename — the step
/// that puts the wrong poster on a remake. The caller grabbed a specific
/// TMDB id; telling plurx costs nothing and ends the ambiguity.
#[derive(Clone, Debug, Default)]
pub struct IdHints {
    pub tmdb: Option<i64>,
    pub imdb: Option<String>,
    /// For an episode, the SHOW's id — an episode's own id is not what
    /// identifies the series it belongs to.
    pub series_tmdb: Option<i64>,
    /// The ids belong to an ancestor (a show), not to the placed item.
    pub episodeish: bool,
}

/// Validated Curator facts carried by one targeted book scan.
#[derive(Clone, Debug)]
pub struct BookHints {
    pub title: Option<String>,
    pub author: Option<String>,
    pub medium: ItemKind,
    pub work_id: String,
    pub edition_id: String,
    pub cover_url: Option<String>,
}

fn curator_pairing_can_advance(
    current_edition: Option<&str>,
    current_provider_cover: bool,
    requested_edition: &str,
    replacement_cover_ready: bool,
) -> bool {
    !current_provider_cover || current_edition == Some(requested_edition) || replacement_cover_ready
}

impl IdHints {
    fn is_empty(&self) -> bool {
        self.tmdb.is_none() && self.imdb.is_none() && self.series_tmdb.is_none()
    }
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

/// How many request records are kept. A debugging surface — "what happened
/// last night" — not an audit log, and deliberately in memory: persisting it
/// would mean a schema, a retention policy and a growth problem, for data
/// whose value expires in hours.
const MAX_REQUESTS: usize = 256;
/// One library can attract a burst of integration callbacks while another
/// node owns its scan. Bound retained waiter state independently of the
/// request-history ring; overflow is terminal and visible to the caller.
const MAX_PENDING_PER_LIBRARY: usize = 256;

const PRETRANSCODE_LEASE_TTL: std::time::Duration = std::time::Duration::from_secs(30);
const PRETRANSCODE_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(10);
const PRETRANSCODE_REFUSAL_TTL_MS: i64 = 10 * 60 * 1_000;
// The store admits at most 4,096 active queue rows. Retaining that entire
// bounded universe avoids rotating one unreadable high-priority row back into
// eligibility before a lower-priority row on this node's mount can be found.
const MAX_PRETRANSCODE_REFUSALS: usize = 4_096 + PRODUCE_MAX_PER_PASS;

fn lease_time_remaining(expires_at_unix_ms: i64) -> std::time::Duration {
    let now_unix_ms = clock_ms();
    std::time::Duration::from_millis(expires_at_unix_ms.saturating_sub(now_unix_ms).max(0) as u64)
}

pub(crate) fn clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(i64::MAX)
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
) -> Result<Vec<plurx_core::transcode::CopyVideoOptions>, StoreError> {
    let probe_json = store.get_file_probe_json(file.id).await?;
    Ok(crate::fragindex::video_identities(
        file,
        probe_json.as_deref(),
        have_dovi,
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
async fn fragment_index_requested_video_options(
    store: &dyn Store,
    file: &MediaFile,
    have_dovi: bool,
) -> Result<plurx_core::transcode::CopyVideoOptions, StoreError> {
    let probe_json = store.get_file_probe_json(file.id).await?;
    let videos = crate::fragindex::video_identities(file, probe_json.as_deref(), have_dovi);
    for video in &videos {
        let identity = crate::fragindex::identity_for(file, *video);
        if store.fragment_index(file.id, &identity).await?.is_none() {
            return Ok(*video);
        }
    }
    Ok(videos
        .first()
        .copied()
        .unwrap_or_else(|| plurx_core::transcode::CopyVideoOptions::new(have_dovi, false)))
}

/// Heartbeat and self-fence for one distributed queue row.
struct ActivePretranscodeJob {
    fence: PretranscodeFence,
    cancel: tokio_util::sync::CancellationToken,
    lost: tokio_util::sync::CancellationToken,
    heartbeat: Option<tokio::task::JoinHandle<Result<(), StoreError>>>,
}

impl ActivePretranscodeJob {
    fn start(store: Arc<dyn Store>, job: PretranscodeJob) -> Self {
        let fence = PretranscodeFence::new(job);
        let heartbeat_fence = fence.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_cancel = cancel.clone();
        let lost = tokio_util::sync::CancellationToken::new();
        let heartbeat_lost = lost.clone();
        let heartbeat = tokio::spawn(async move {
            let mut heartbeat_error = None;
            let mut ticker = tokio::time::interval(PRETRANSCODE_HEARTBEAT);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = heartbeat_cancel.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                let Some(current) = heartbeat_fence.snapshot().await else {
                    heartbeat_lost.cancel();
                    break;
                };
                let now_unix_ms = clock_ms();
                if now_unix_ms >= current.lease_expires_ms {
                    let _ = heartbeat_fence.invalidate(&current).await;
                    heartbeat_lost.cancel();
                    tracing::warn!(
                        job = current.id,
                        fence = current.fence,
                        "pre-transcode queue heartbeat missed its lease deadline"
                    );
                    break;
                }
                let expires_at =
                    now_unix_ms.saturating_add(
                        PRETRANSCODE_LEASE_TTL.as_millis().min(i64::MAX as u128) as i64,
                    );
                let renewal = heartbeat_fence.renew(store.as_ref(), now_unix_ms, expires_at);
                tokio::pin!(renewal);
                let expiry = tokio::time::sleep(lease_time_remaining(current.lease_expires_ms));
                tokio::pin!(expiry);
                let mut stop_after_renewal = false;
                let renewed = tokio::select! {
                    _ = heartbeat_cancel.cancelled() => {
                        // A renewal may already have crossed the backend
                        // boundary. Drain it before retirement so an
                        // acknowledged replacement cannot be stranded.
                        stop_after_renewal = true;
                        renewal.await
                    }
                    _ = &mut expiry => {
                        stop_after_renewal = true;
                        heartbeat_fence.revoke();
                        heartbeat_lost.cancel();
                        let result = renewal.await;
                        tracing::warn!(
                            job = current.id,
                            fence = current.fence,
                            "pre-transcode queue renewal exceeded its lease deadline and self-fenced"
                        );
                        result
                    }
                    result = &mut renewal => result,
                };
                match renewed {
                    Ok(true) => {}
                    Ok(false) => {
                        heartbeat_lost.cancel();
                        tracing::warn!(
                            job = current.id,
                            fence = current.fence,
                            "pre-transcode queue job lost its fence"
                        );
                        break;
                    }
                    Err(error) => {
                        heartbeat_lost.cancel();
                        tracing::warn!(
                            job = current.id,
                            fence = current.fence,
                            %error,
                            "pre-transcode queue renewal failed and self-fenced"
                        );
                        heartbeat_error = Some(error);
                        break;
                    }
                }
                if stop_after_renewal {
                    break;
                }
            }
            heartbeat_fence.revoke();
            if let Err(error) = heartbeat_fence.retire(store.as_ref()).await {
                tracing::warn!(%error, "pre-transcode queue retirement was ambiguous");
                if heartbeat_error.is_none() {
                    heartbeat_error = Some(error);
                }
            }
            match heartbeat_error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        });
        Self {
            fence,
            cancel,
            lost,
            heartbeat: Some(heartbeat),
        }
    }

    fn fence(&self) -> PretranscodeFence {
        self.fence.clone()
    }

    fn loss_token(&self) -> tokio_util::sync::CancellationToken {
        self.lost.clone()
    }

    async fn finish(mut self) {
        self.fence.revoke();
        self.lost.cancel();
        self.cancel.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            match heartbeat.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "pre-transcode cleanup was ambiguous after settlement");
                }
                Err(error) => {
                    tracing::warn!(%error, "pre-transcode heartbeat task failed during settlement");
                }
            }
        }
    }
}

impl Drop for ActivePretranscodeJob {
    fn drop(&mut self) {
        self.fence.revoke();
        self.cancel.cancel();
        self.lost.cancel();
        // Dropping a JoinHandle detaches the task. Cancellation makes it
        // drain any dispatched renewal and retire the final acknowledged
        // token; aborting here would recreate the late-write race.
        let _ = self.heartbeat.take();
    }
}

impl JobManager {
    #[cfg(test)]
    fn new(store: Arc<dyn Store>, artwork_dir: PathBuf) -> Self {
        Self::new_with_scan_prune_percent(
            store,
            artwork_dir,
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "test-node".to_owned(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        )
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
            coordinator,
            job_authority,
            membership: None,
            artwork_dir,
            scan_prune_percent,
            #[cfg(test)]
            tmdb_base: None,
            statuses: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            pending_retries: Mutex::new(HashSet::new()),
            requests: Mutex::new(VecDeque::new()),
            metrics: Arc::new(IntegrationMetrics::default()),
            producing: std::sync::atomic::AtomicBool::new(false),
            indexing: std::sync::atomic::AtomicBool::new(false),
            cluster_index_working: std::sync::atomic::AtomicBool::new(false),
            last_analysis_prune_ms: AtomicI64::new(0),
            analysis_progress: std::sync::Mutex::new(HashMap::new()),
            analysis_progress_epoch: AtomicU64::new(0),
            analysis_metrics: Arc::new(AnalysisRuntimeMetrics::default()),
            now_producing: Mutex::new(None),
            stop_producing: std::sync::atomic::AtomicBool::new(false),
            pretranscode_refusals: Mutex::new(HashMap::new()),
            fragment_index_refusals: Mutex::new(HashMap::new()),
            last_pretranscode_cache_sweep_ms: AtomicI64::new(0),
            fragment_index_sweep_cursor: Mutex::new(None),
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

    async fn acquire_job(&self, resource: String) -> Result<Option<ActiveJobLease>, StoreError> {
        acquire_cluster_job(&self.coordinator, self.job_authority.as_ref(), resource).await
    }

    /// Whether this node may run cluster-wide scheduled work right now.
    pub(crate) async fn may_run_cluster_jobs(&self) -> bool {
        self.job_authority.may_run_cluster_jobs().await
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
        if !matches!(component, "fragment_index" | "skip_markers") {
            return Err(StoreError::Task(
                "unsupported analysis component".to_owned(),
            ));
        }
        if !matches!(trigger, "admin" | "background") {
            return Err(StoreError::Task("unsupported analysis trigger".to_owned()));
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
        let requested_generation = if force_rebuild {
            uuid::Uuid::new_v4().to_string()
        } else {
            plurx_core::segplan::argv_fingerprint(&[
                "analysis-request".to_owned(),
                file.id.to_string(),
                file.size.to_string(),
                file.mtime.to_string(),
                component.to_owned(),
                pipeline_version.clone(),
            ])
        };
        let request = self
            .store
            .enqueue_analysis_request(&NewAnalysisRequest {
                request_id: request_id.clone(),
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                component: component.to_owned(),
                pipeline_version,
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
                started_at_ms: now,
                updated_at_ms: now,
                elapsed_ms: 0,
                throughput_bps: 0,
                eta_ms: None,
                registry_epoch,
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
            value.throughput_bps = if value.elapsed_ms > 0 {
                value
                    .bytes_read
                    .saturating_mul(1_000)
                    .saturating_div(u64::try_from(value.elapsed_ms).unwrap_or(u64::MAX))
            } else {
                0
            };
            value.eta_ms = if value.bytes_read > 0
                && value.total_bytes > value.bytes_read
                && value.throughput_bps > 0
            {
                Some(
                    value
                        .total_bytes
                        .saturating_sub(value.bytes_read)
                        .saturating_mul(1_000)
                        .saturating_div(value.throughput_bps)
                        .min(i64::MAX as u64) as i64,
                )
            } else {
                None
            };
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
        let live = self.live.lock().await;
        for (id, progress) in live.iter() {
            if let Some(status) = map.get_mut(id) {
                if status.running {
                    status.progress = Some(ProgressSnapshot::sample(progress));
                }
            }
        }
        map
    }

    /// Kick off a scan for `library_id` unless one is already running. Returns
    /// `true` if a scan was started, `false` if one was already in flight.
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

    async fn trigger(
        self: &Arc<Self>,
        library_id: i64,
        force_metadata: bool,
        why: ScanTrigger,
    ) -> bool {
        let resource = format!("scan:library:{library_id}");
        let lease = match self.acquire_job(resource).await {
            Ok(Some(lease)) => lease,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(
                    library = library_id,
                    stage = "acquire_lease",
                    error = %error,
                    "acquiring scan lease failed"
                );
                return false;
            }
        };
        {
            let mut statuses = self.statuses.lock().await;
            let entry = statuses.entry(library_id).or_default();
            if entry.running {
                drop(statuses);
                let _ = lease.release().await;
                return false;
            }
            self.metrics.count_scan(why);
            *entry = ScanStatus {
                running: true,
                phase: Some("scanning".to_owned()),
                started_at: Some(now()),
                ..Default::default()
            };
        }
        let progress = Arc::new(ScanProgress::default());
        self.live
            .lock()
            .await
            .insert(library_id, Arc::clone(&progress));

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let lost = lease.loss_token();
            tokio::select! {
                () = manager.run_scan(library_id, progress, force_metadata, &lease) => {}
                () = lost.cancelled() => {
                    tracing::warn!(
                        library = library_id,
                        stage = "lease",
                        "library scan failed because its cluster lease was lost"
                    );
                    let mut status = manager
                        .statuses
                        .lock()
                        .await
                        .get(&library_id)
                        .cloned()
                        .unwrap_or_default();
                    status.error = Some("cluster scan lease was lost".to_owned());
                    manager.finish(library_id, status).await;
                }
            }
            let _ = lease.release().await;
            // Whatever queued up while this ran is work someone was
            // promised. A full scan covers the same files a targeted one
            // would have, but the CALLER is still owed its answer — the
            // item ids it asked for — so the queue drains rather than
            // being discarded as redundant.
            manager.drain_pending(library_id).await;
        });
        true
    }

    /// Ask for a targeted scan of one path.
    ///
    /// Returns `Ok(Some(scan))` when it ran now, or `Ok(None)` when the
    /// library was already scanning and the request was queued — the caller
    /// polls `scan_request` for the outcome.
    ///
    /// **Requests are queued, never dropped.** `trigger` returns false
    /// while a scan runs, which is right for "the user pressed Scan twice"
    /// and wrong here: importing a season fires one request per episode
    /// within seconds, and dropping N−1 of them would leave most of the
    /// season unindexed with nothing anywhere saying so. Duplicates by path
    /// collapse (scanning the same folder twice is the same work), and the
    /// rest are drained when the running scan finishes.
    pub async fn request_scan(
        self: &Arc<Self>,
        req: ScanRequest,
    ) -> Result<Option<TargetedScan>, TargetError> {
        self.record_request(&req, "running", None, None).await;

        let busy = {
            let statuses = self.statuses.lock().await;
            statuses.get(&req.library_id).is_some_and(|s| s.running)
        };
        if busy {
            if let Err(error) = self.queue_targeted(req.clone()).await {
                self.record_request(&req, "failed", None, Some(error.to_string()))
                    .await;
                return Err(error);
            }
            self.set_request_status(&req.id, "queued").await;
            return Ok(None);
        }

        let lease = match self
            .acquire_job(format!("scan:library:{}", req.library_id))
            .await
            .map_err(TargetError::Store)?
        {
            Some(lease) => lease,
            None => {
                if let Err(error) = self.queue_targeted(req.clone()).await {
                    self.record_request(&req, "failed", None, Some(error.to_string()))
                        .await;
                    return Err(error);
                }
                self.set_request_status(&req.id, "queued").await;
                self.schedule_pending_retry(req.library_id).await;
                return Ok(None);
            }
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let out = tokio::select! {
            out = self.run_targeted(std::slice::from_ref(&req), &publisher) => out,
            () = lost.cancelled() => Err(TargetError::Store(StoreError::Task(
                "cluster scan lease was lost".to_owned(),
            ))),
        };
        let _ = lease.release().await;
        match &out {
            Ok(scan) => {
                self.record_request(&req, "done", Some(scan), None).await;
            }
            Err(e) => {
                self.record_request(&req, "failed", None, Some(e.to_string()))
                    .await;
            }
        }
        out.map(Some)
    }

    async fn run_targeted(
        &self,
        requests: &[ScanRequest],
        publisher: &PublicationStore<'_>,
    ) -> Result<TargetedScan, TargetError> {
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
        let out = scan::scan_path_with_publication(publisher, &library, &req.path).await?;
        for request in requests {
            self.apply_ids(request, &out.items, publisher).await;
        }

        // The rows exist; without this they would have no artwork until
        // somebody pressed Scan. Bounded to what this request placed (and the
        // seasons/shows/folders above it) because monarr is holding the HTTP
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
        for request in requests {
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
            let client = AniListClient::new();
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
                    let tmdb = self.tmdb_client(key);
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
    ) {
        let Some(ids) = req.ids.as_ref().filter(|i| !i.is_empty()) else {
            return;
        };
        for placed in items {
            let target = if ids.episodeish {
                match self.show_root(placed.item_id).await {
                    Some(id) => id,
                    None => continue,
                }
            } else {
                placed.item_id
            };
            let patch = MetadataPatch {
                tmdb_id: ids.series_tmdb.or(ids.tmdb),
                imdb_id: if ids.episodeish {
                    None
                } else {
                    ids.imdb.clone()
                },
                ..Default::default()
            };
            match publisher.apply_metadata(target, &patch).await {
                Ok(_) => tracing::info!(
                    target: "plurxd::integrate",
                    item = target,
                    tmdb = patch.tmdb_id.unwrap_or(0),
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "applied caller-supplied ids"
                ),
                Err(e) => tracing::warn!(
                    target: "plurxd::integrate",
                    item = target, error = %e,
                    correlation_id = req.correlation_id.as_deref().unwrap_or("-"),
                    "could not apply caller-supplied ids; falling back to title matching"
                ),
            }
        }
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

    async fn queue_targeted(&self, req: ScanRequest) -> Result<(), TargetError> {
        let mut pending = self.pending.lock().await;
        // The path is only the physical scan target. Request ids, caller ids,
        // curator book hints, correlation ids, and terminal status are all
        // per waiter; discarding a same-path request would strand its record
        // in `queued` forever and silently lose its metadata payload.
        let queue = pending.entry(req.library_id).or_default();
        if queue.len() >= MAX_PENDING_PER_LIBRARY {
            return Err(TargetError::Store(StoreError::Task(format!(
                "targeted scan queue for library {} is full ({MAX_PENDING_PER_LIBRARY} waiters)",
                req.library_id
            ))));
        }
        queue.push(req);
        Ok(())
    }

    async fn schedule_pending_retry(self: &Arc<Self>, library_id: i64) {
        if !self.pending_retries.lock().await.insert(library_id) {
            return;
        }
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if manager.drain_pending_once(library_id).await {
                    continue;
                }
                // Close the empty-observation/removal race with request_scan:
                // it queues while holding `pending`, then tries
                // `pending_retries`. Holding the first lock while removing
                // from the second makes an enqueue land wholly before this
                // check (and loop again) or wholly after removal (and install
                // a new retry task).
                let pending = manager.pending.lock().await;
                if pending
                    .get(&library_id)
                    .is_some_and(|queue| !queue.is_empty())
                {
                    continue;
                }
                manager.pending_retries.lock().await.remove(&library_id);
                break;
            }
        });
    }

    /// Run whatever queued up while a library was scanning. Called once a
    /// scan finishes; the work a caller was promised must not be forgotten
    /// just because it arrived at a busy moment.
    async fn drain_pending(self: &Arc<Self>, library_id: i64) {
        if self.drain_pending_once(library_id).await {
            self.schedule_pending_retry(library_id).await;
        }
    }

    /// Attempt one drain. `true` asks the caller's single-flight retry loop to
    /// try again after the remote owner has had time to make progress.
    async fn drain_pending_once(self: &Arc<Self>, library_id: i64) -> bool {
        let queued = {
            let mut pending = self.pending.lock().await;
            pending.remove(&library_id).unwrap_or_default()
        };
        if queued.is_empty() {
            return false;
        }
        let lease = match self.acquire_job(format!("scan:library:{library_id}")).await {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                for req in queued {
                    if let Err(error) = self.queue_targeted(req.clone()).await {
                        self.record_request(&req, "failed", None, Some(error.to_string()))
                            .await;
                    }
                }
                return true;
            }
            Err(error) => {
                tracing::warn!(library = library_id, error = %error, "targeted scan lease retry failed");
                for req in queued {
                    if let Err(error) = self.queue_targeted(req.clone()).await {
                        self.record_request(&req, "failed", None, Some(error.to_string()))
                            .await;
                    }
                }
                return true;
            }
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let mut groups: BTreeMap<PathBuf, Vec<ScanRequest>> = BTreeMap::new();
        for req in queued {
            let normalized = req.path.canonicalize().unwrap_or_else(|_| req.path.clone());
            groups.entry(normalized).or_default().push(req);
        }
        for requests in groups.into_values() {
            let out = tokio::select! {
                out = self.run_targeted(&requests, &publisher) => out,
                () = lost.cancelled() => Err(TargetError::Store(StoreError::Task(
                    "cluster scan lease was lost".to_owned(),
                ))),
            };
            for req in requests {
                match &out {
                    Ok(scan) => self.record_request(&req, "done", Some(scan), None).await,
                    Err(e) => {
                        self.record_request(&req, "failed", None, Some(e.to_string()))
                            .await
                    }
                }
            }
        }
        drop(publisher);
        let _ = lease.release().await;
        false
    }

    /// The recent request ring, newest last.
    pub async fn scan_requests(&self) -> Vec<ScanRequestRecord> {
        self.requests.lock().await.iter().cloned().collect()
    }

    pub async fn scan_request(&self, id: &str) -> Option<ScanRequestRecord> {
        self.requests
            .lock()
            .await
            .iter()
            .find(|r| r.request_id == id)
            .cloned()
    }

    async fn record_request(
        &self,
        req: &ScanRequest,
        status: &str,
        scan: Option<&TargetedScan>,
        error: Option<String>,
    ) {
        let mut ring = self.requests.lock().await;
        if let Some(existing) = ring.iter_mut().find(|r| r.request_id == req.id) {
            existing.status = status.to_owned();
            existing.report = scan.map(|s| s.report.clone());
            existing.items = scan.map(|s| s.items.clone());
            existing.error = error;
            return;
        }
        if ring.len() == MAX_REQUESTS {
            ring.pop_front();
        }
        ring.push_back(ScanRequestRecord {
            request_id: req.id.clone(),
            at: now(),
            library_id: req.library_id,
            path: req.path.display().to_string(),
            correlation_id: req.correlation_id.clone(),
            source: req.source.clone(),
            status: status.to_owned(),
            report: scan.map(|s| s.report.clone()),
            items: scan.map(|s| s.items.clone()),
            error,
        });
    }

    async fn set_request_status(&self, id: &str, status: &str) {
        if let Some(r) = self
            .requests
            .lock()
            .await
            .iter_mut()
            .find(|r| r.request_id == id)
        {
            r.status = status.to_owned();
        }
    }

    async fn run_scan(
        &self,
        library_id: i64,
        progress: Arc<ScanProgress>,
        force_metadata: bool,
        lease: &ActiveJobLease,
    ) {
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
                return;
            }
            Err(e) => {
                tracing::warn!(
                    library = library_id,
                    stage = "load_library",
                    error = %e,
                    "library scan failed"
                );
                self.finish(library_id, error_status(&e.to_string())).await;
                return;
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
            Ok(report) => status.last_scan = Some(report),
            Err(e) => {
                tracing::warn!(
                    library = library_id,
                    stage = "catalogue_scan",
                    error = %e,
                    "library scan failed"
                );
                self.finish(library_id, error_status(&e.to_string())).await;
                return;
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
        }
        self.finish(library_id, status).await;
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

    async fn run_due_jobs(
        self: &Arc<Self>,
        transcode: &Arc<TranscodeManager>,
    ) -> Result<(), plurx_core::error::StoreError> {
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
                        tracing::info!(library = id, "scheduled scan started");
                    }
                }
                DueJob::Refresh(id) => {
                    if self.trigger_refresh_as(id, ScanTrigger::Scheduled).await {
                        tracing::info!(library = id, "scheduled metadata refresh started");
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
                    let removed = transcode.sweep_orphan_dirs().await;
                    if removed > 0 {
                        tracing::info!(removed, "swept orphaned transcode directories");
                    }
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

        // Execution has no interval of its own. Every node asks once per
        // scheduler tick while speculative production is enabled, and the
        // process-local guard keeps a long title from stacking worker loops.
        if global.cache_produce_mins > 0 {
            let state = Arc::clone(self);
            let transcode = Arc::clone(transcode);
            tokio::spawn(async move { state.work_pretranscode_queue(transcode).await });
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
            Ok(Some(key)) if !key.is_empty() => Some(TmdbClient::new(key)),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, "genre backfill: reading TMDB key");
                None
            }
        };
        let anilist = AniListClient::new();
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
        let user_cursor = publisher
            .get_setting(keys::CACHE_PRETRANSCODE_USER_CURSOR)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
            .unwrap_or(0);
        let mut users = match self
            .store
            .list_users_page(user_cursor, PRODUCE_USER_PAGE)
            .await
        {
            Ok(users) => users,
            Err(e) => {
                tracing::warn!(error = %e, "candidate generator cannot page users");
                drop(publisher);
                let _ = lease.release().await;
                return;
            }
        };
        if users.is_empty() && user_cursor > 0 {
            users = self
                .store
                .list_users_page(0, PRODUCE_USER_PAGE)
                .await
                .unwrap_or_default();
        }
        let next_user_cursor = if users.len() < PRODUCE_USER_PAGE as usize {
            0
        } else {
            users.last().map_or(0, |user| user.id)
        };
        if let Err(error) = publisher
            .put_setting(
                keys::CACHE_PRETRANSCODE_USER_CURSOR,
                &next_user_cursor.to_string(),
            )
            .await
        {
            tracing::warn!(%error, "candidate generator could not advance its user cursor");
        }
        let mut rails: Vec<Vec<produce::DiscoveryCandidate>> = Vec::new();
        let mut in_progress = Vec::new();
        let mut next_up = Vec::new();
        for user in &users {
            if let Ok(rows) = self.store.continue_watching(user.id, PRODUCE_RAIL).await {
                in_progress.extend(rows.into_iter().map(|r| (r.item.id, r.item.title)));
            }
            if let Ok(rows) = self.store.next_up(user.id, PRODUCE_RAIL).await {
                next_up.extend(rows.into_iter().map(|r| (r.item.id, r.item.title)));
            }
        }
        // The fallback rail, and the only one a brand-new server has: nobody
        // has watch history on day one, but a 4K film that landed yesterday is
        // still the most likely thing to be played tonight.
        let recent: Vec<(i64, String)> = self
            .store
            .recently_added(None, PRODUCE_RAIL)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.item.id, r.item.title))
            .collect();

        for (reason, items) in [
            (produce::REASON_IN_PROGRESS, in_progress),
            (produce::REASON_NEXT_UP, next_up),
            (produce::REASON_RECENT, recent),
        ] {
            let mut rail = Vec::new();
            for (item_id, title) in items {
                rail.push(produce::DiscoveryCandidate {
                    item_id,
                    title,
                    reason,
                });
            }
            rails.push(rail);
        }

        // Dedupe/rank cheap item ids first, then perform at most one bounded
        // file lookup per globally selected item. This is O(1) store fan-out
        // as users/history grow: 64 users, 129 rail reads, and 64 file reads.
        let discoveries = produce::rank_discovery(&rails, PRODUCE_DISCOVERY_ITEMS);
        let mut candidates = Vec::with_capacity(PRODUCE_MAX_PER_PASS);
        for discovery in discoveries {
            let Ok(files) = self.store.files_for_item(discovery.item_id).await else {
                continue;
            };
            let Some(file) = files.into_iter().next() else {
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
            match publisher.enqueue_pretranscode_job(&job).await {
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

        let cluster_cache_enabled =
            match self.store.get_setting(keys::VOD_INDEX_CLUSTER_CACHE).await {
                Ok(value) => setting_enabled(value),
                Err(error) => {
                    tracing::warn!(%error, "could not read the cluster fragment-index gate");
                    false
                }
            };
        if cluster_cache_enabled {
            if self.may_run_cluster_jobs().await {
                self.discover_cluster_fragment_indexes(transcode).await;
            }
            return;
        }

        let deadline = std::time::Instant::now() + INDEX_WINDOW;
        let have_dovi = transcode.dv_strippable();
        let runtime_cache = transcode.runtime_cache_dir().to_path_buf();
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
        let paths = ordered_index_paths(paths, cursor);

        let mut built = 0usize;
        let mut built_file_ids = Vec::new();
        let mut attempted = 0usize;
        let mut last_examined = None;
        for (examined, (file_id, _path)) in paths.into_iter().enumerate() {
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
            if !transcode.pretranscode_worker_idle() {
                return;
            }
            last_examined = Some(file_id);
            let Ok(Some(file)) = self.store.get_file(file_id).await else {
                continue;
            };
            if !crate::copyseg::supports(file.video_codec.as_deref()) {
                continue;
            }
            let videos = match fragment_index_video_identities(
                self.store.as_ref(),
                &file,
                have_dovi,
            )
            .await
            {
                Ok(videos) => videos,
                Err(error) => {
                    tracing::warn!(file_id, %error, "reading probe for fragment index");
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
            for video in videos {
                if !transcode.pretranscode_worker_idle() {
                    // Deliberately a return: it skips the cursor stamp, so a
                    // preempted pass leaves the file due rather than half done.
                    return;
                }
                let identity = crate::fragindex::identity_for(&file, video);
                match self.store.fragment_index(file_id, &identity).await {
                    // Already current for this file and this pipeline.
                    Ok(Some(_)) => continue,
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
                attempted += 1;
                match crate::fragindex::build(
                    &file,
                    video,
                    &runtime_cache,
                    index_file_budget(file.duration_ms),
                )
                .await
                {
                    crate::fragindex::IndexOutcome::Built(index) => {
                        if let Err(error) = self.store.put_fragment_index(file_id, &index).await {
                            tracing::warn!(file_id, error = %error, "storing a fragment index");
                        } else {
                            built += 1;
                            if built_file_ids.last() != Some(&file_id) {
                                built_file_ids.push(file_id);
                            }
                        }
                    }
                    // These now make an HLS title unavailable, so keep the
                    // reason in the ordinary operator log and move the cursor
                    // forward.
                    crate::fragindex::IndexOutcome::Truncated { reason, rows } => {
                        tracing::warn!(file_id, rows, "fragment index incomplete: {reason}");
                    }
                    crate::fragindex::IndexOutcome::Unsupported(reason) => {
                        tracing::warn!(file_id, "file cannot be indexed: {reason}");
                    }
                }
            }
        }
        // The scheduler can tick before a first-run library has been created
        // or before its initial scan has published any files. Stamping that
        // empty pass would make a default-on VOD server refuse every new title
        // for a full cadence. A pass that examined media is complete even when
        // everything was already current or unsupported; an empty or
        // preempted pass remains due for the next minute tick.
        if let Some(file_id) = last_examined {
            if let Err(error) = self
                .store
                .put_setting(&cursor_key, &file_id.to_string())
                .await
            {
                tracing::warn!(error = %error, key = cursor_key, "recording VOD index cursor failed");
            }
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
        const RETAIN_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let prune_before = clock_ms().saturating_sub(RETAIN_MS);
        match self
            .store
            .prune_cluster_fragment_indexes(prune_before, 128)
            .await
        {
            Ok(cache_keys) => {
                for cache_key in cache_keys {
                    crate::fragment_index_cluster::remove_local_blob(&cache_root, &cache_key).await;
                }
            }
            Err(error) => tracing::warn!(%error, "pruning fragment-index catalog generations"),
        }
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
            match self
                .request_file_analysis(file_id, false, "fragment_index", "background")
                .await
            {
                Ok((_, false)) => enqueued += 1,
                Ok((_, true)) => {}
                Err(error) => {
                    tracing::warn!(file_id, %error, "queueing background fragment analysis");
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
        const GLOBAL_SLOTS: usize = 2;

        if self.cluster_index_working.swap(true, Ordering::Relaxed) {
            return;
        }
        let _guard = ClusterIndexWorkingGuard(Arc::clone(&self));
        if !self.may_run_cluster_jobs().await {
            return;
        }
        let fragment_engine_current = crate::ffmpeg::fragment_index_engine_is_current().await;

        let now = clock_ms();
        if let Err(error) = self.store.settle_analysis_requests(now).await {
            tracing::warn!(%error, "settling analysis requests");
        }
        const ANALYSIS_PRUNE_INTERVAL_MS: i64 = 60 * 60 * 1_000;
        const ANALYSIS_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let last_prune = self.last_analysis_prune_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last_prune) >= ANALYSIS_PRUNE_INTERVAL_MS
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
        self.resolve_analysis_requests(Arc::clone(&transcode)).await;

        if !fragment_engine_current {
            return;
        }

        let mut slots = tokio::task::JoinSet::new();
        for slot in 0..GLOBAL_SLOTS {
            let lease = match self
                .acquire_job(format!("media:fragment-index:{slot}"))
                .await
            {
                Ok(Some(lease)) => lease,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(slot, %error, "acquiring cluster fragment-index media slot");
                    continue;
                }
            };
            let state = Arc::clone(&self);
            let transcode = Arc::clone(&transcode);
            slots.spawn(async move {
                let permit_lost = lease.loss_token();
                let built = state
                    .drain_cluster_fragment_index_slot(transcode, permit_lost)
                    .await;
                if let Err(error) = lease.release().await {
                    tracing::warn!(slot, %error, "releasing cluster fragment-index media slot");
                }
                built
            });
        }
        let mut built = 0_usize;
        while let Some(result) = slots.join_next().await {
            match result {
                Ok(count) => built += count,
                Err(error) => tracing::warn!(%error, "cluster fragment-index slot panicked"),
            }
        }
        if built > 0 {
            tracing::info!(built, "cluster fragment-index queue pass finished");
        }
    }

    async fn resolve_analysis_requests(self: &Arc<Self>, transcode: Arc<TranscodeManager>) {
        const MAX_REQUESTS_PER_PASS: usize = 2;
        const ATTEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

        if !self.cluster_fragment_index_enabled().await || !transcode.pretranscode_worker_idle() {
            return;
        }
        let node_id = self.coordinator.node_id().to_owned();
        let retry_policy = self.analysis_retry_policy().await;
        let engine_sha256 = crate::ffmpeg::fragment_index_engine_digest().await;
        let have_dovi = transcode.dv_strippable();
        for _ in 0..MAX_REQUESTS_PER_PASS {
            if !self.cluster_fragment_index_enabled().await || !transcode.pretranscode_worker_idle()
            {
                break;
            }
            let now = clock_ms();
            let request = match self
                .store
                .claim_analysis_request(&node_id, now, now.saturating_add(retry_policy.lease_ms))
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
                let stop = stop.clone();
                let lost = lost.clone();
                let fence = request.fence;
                let renew_every = retry_policy.renew_every();
                let lease_ms = retry_policy.lease_ms;
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(renew_every);
                    loop {
                        tokio::select! {
                            () = stop.cancelled() => break,
                            _ = interval.tick() => {
                                let now = clock_ms();
                                match store.renew_analysis_request(
                                    &request_id,
                                    &node_id,
                                    fence,
                                    now,
                                    now.saturating_add(lease_ms),
                                ).await {
                                    Ok(true) => {}
                                    Ok(false) | Err(_) => {
                                        lost.cancel();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                })
            };

            let outcome = self
                .resolve_analysis_request(
                    &request,
                    &node_id,
                    &engine_sha256,
                    have_dovi,
                    transcode.as_ref(),
                    &lost,
                    ATTEST_TIMEOUT,
                )
                .await;
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
                                let _ = self
                                    .store
                                    .record_analysis_request_phase(
                                        &request,
                                        "retry_wait",
                                        Some(code),
                                        now,
                                    )
                                    .await;
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
                                let _ = self
                                    .store
                                    .record_analysis_request_phase(
                                        &request,
                                        "failed",
                                        Some(code),
                                        now,
                                    )
                                    .await;
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
            stop.cancel();
            let _ = heartbeat.await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_analysis_request(
        &self,
        request: &AnalysisRequest,
        node_id: &str,
        engine_sha256: &str,
        have_dovi: bool,
        transcode: &TranscodeManager,
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
                        let _ = self.store.merge_file_probe_chapters(file.id, &json).await;
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
            let _ = self
                .store
                .record_analysis_request_phase(request, "published", None, clock_ms())
                .await;
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
        let video = fragment_index_requested_video_options(self.store.as_ref(), &file, have_dovi)
            .await
            .map_err(|_| AnalysisResolutionError::Retry {
                code: "source_catalog_read_failed",
                charge_attempt: true,
            })?;
        let object_version = crate::fragment_index_cluster::inspect_source(&file)
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
        let attested = tokio::select! {
            result = crate::fragment_index_cluster::attest_source(node_id, &file, memo.as_ref()) => {
                result.map_err(|_| AnalysisResolutionError::Retry {
                    code: "source_attestation_failed",
                    charge_attempt: true,
                })?
            }
            () = self.wait_for_cluster_fragment_index_stop(transcode, lost) => {
                if lost.is_cancelled() {
                    return Err(AnalysisResolutionError::ClaimLost);
                }
                return Err(AnalysisResolutionError::Retry {
                    code: "foreground_preempted",
                    charge_attempt: false,
                });
            }
            () = wait_analysis_deadline(attest_timeout) => {
                return Err(AnalysisResolutionError::Retry {
                    code: "source_attestation_timeout",
                    charge_attempt: true,
                });
            }
        };
        if lost.is_cancelled() {
            return Err(AnalysisResolutionError::ClaimLost);
        }
        if !transcode.pretranscode_worker_idle() || !self.cluster_fragment_index_enabled().await {
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
            trigger: request.trigger.clone(),
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
            return Err(AnalysisResolutionError::Retry {
                code: "queue_full_or_busy",
                charge_attempt: false,
            });
        }
        let _ = self.store.settle_analysis_requests(now).await;
        Ok(())
    }

    async fn drain_cluster_fragment_index_slot(
        self: &Arc<Self>,
        transcode: Arc<TranscodeManager>,
        permit_lost: tokio_util::sync::CancellationToken,
    ) -> usize {
        const MAX_JOBS_PER_SLOT: usize = 4;
        const MAX_REFUSALS: usize = 4_096;

        let node_id = self.coordinator.node_id().to_owned();
        let worker = ClusterFragmentIndexWorker {
            engine_sha256: crate::ffmpeg::fragment_index_engine_digest().await,
            cache_root: crate::fragment_index_cluster::cache_root(transcode.runtime_cache_dir()),
            have_dovi: transcode.dv_strippable(),
            retry_policy: self.analysis_retry_policy().await,
        };
        let mut built = 0_usize;
        for _ in 0..MAX_JOBS_PER_SLOT {
            if permit_lost.is_cancelled()
                || !transcode.pretranscode_worker_idle()
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
            let job = match self
                .store
                .claim_cluster_fragment_index(
                    &node_id,
                    &excluded,
                    now,
                    now.saturating_add(worker.retry_policy.lease_ms),
                )
                .await
            {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "claiming a cluster fragment-index job");
                    break;
                }
            };
            if Arc::clone(self)
                .run_cluster_fragment_index_job(
                    Arc::clone(&transcode),
                    job,
                    worker.clone(),
                    permit_lost.clone(),
                )
                .await
            {
                built += 1;
            }
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

    async fn wait_for_cluster_fragment_index_stop(
        &self,
        transcode: &TranscodeManager,
        permit_lost: &tokio_util::sync::CancellationToken,
    ) {
        let mut ticks = 0_u8;
        loop {
            if permit_lost.is_cancelled() || !transcode.pretranscode_worker_idle() {
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

    async fn run_cluster_fragment_index_job(
        self: Arc<Self>,
        transcode: Arc<TranscodeManager>,
        job: plurx_core::store::ClusterFragmentIndexJob,
        worker: ClusterFragmentIndexWorker,
        permit_lost: tokio_util::sync::CancellationToken,
    ) -> bool {
        // Long enough for a node to walk the entire enforced 4,096-job active
        // queue at eight refusals per minute. Discovery removes an exclusion
        // immediately when the exact source becomes readable again.
        const LOCAL_REFUSAL_MS: i64 = 24 * 60 * 60_000;
        const ATTEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

        let _progress = self.start_analysis_progress(
            (&job.cache_key, &job.target_node_id),
            job.file_id,
            "fragment_index",
            "verifying",
            job.source_size.max(0) as u64,
            0,
        );
        let node_id = self.coordinator.node_id().to_owned();
        let stop = tokio_util::sync::CancellationToken::new();
        let lost = tokio_util::sync::CancellationToken::new();
        let retry_identity = format!("{}:{}", job.cache_key, job.target_node_id);
        let retry_ms = worker
            .retry_policy
            .backoff_ms(&retry_identity, job.attempts.max(1));
        let heartbeat = {
            let store = Arc::clone(&self.store);
            let cache_key = job.cache_key.clone();
            let target_node_id = job.target_node_id.clone();
            let node_id = node_id.clone();
            let stop = stop.clone();
            let lost = lost.clone();
            let fence = job.fence;
            let renew_every = worker.retry_policy.renew_every();
            let lease_ms = worker.retry_policy.lease_ms;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(renew_every);
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = interval.tick() => {
                            let now = clock_ms();
                            match store.renew_cluster_fragment_index(
                                &cache_key,
                                &target_node_id,
                                &node_id,
                                fence,
                                now,
                                now.saturating_add(lease_ms),
                            ).await {
                                Ok(true) => {}
                                Ok(false) | Err(_) => {
                                    lost.cancel();
                                    break;
                                }
                            }
                        }
                    }
                }
            })
        };

        let finish_heartbeat =
            |stop: tokio_util::sync::CancellationToken, heartbeat: tokio::task::JoinHandle<()>| async move {
                stop.cancel();
                let _ = heartbeat.await;
            };
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
                let _ = self
                    .store
                    .fail_cluster_fragment_index(
                        &job.cache_key,
                        &job.target_node_id,
                        &node_id,
                        job.fence,
                        code,
                        retryable,
                        now,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                finish_heartbeat(stop, heartbeat).await;
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
        )
        .await
        {
            Ok(videos) => videos,
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "reading probe for claimed fragment index");
                let now = clock_ms();
                let _ = self
                    .store
                    .fail_cluster_fragment_index(
                        &job.cache_key,
                        &job.target_node_id,
                        &node_id,
                        job.fence,
                        "source_catalog_read_failed",
                        true,
                        now,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                finish_heartbeat(stop, heartbeat).await;
                return false;
            }
        };
        let object_version = match crate::fragment_index_cluster::inspect_source(&file).await {
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
                    let _ = self
                        .store
                        .yield_cluster_fragment_index(
                            &job.cache_key,
                            &job.target_node_id,
                            &node_id,
                            job.fence,
                            now,
                            now,
                        )
                        .await;
                } else {
                    let _ = self
                        .store
                        .fail_cluster_fragment_index(
                            &job.cache_key,
                            &job.target_node_id,
                            &node_id,
                            job.fence,
                            "source_unavailable",
                            true,
                            now,
                            now.saturating_add(retry_ms),
                        )
                        .await;
                }
                finish_heartbeat(stop, heartbeat).await;
                return false;
            }
        };
        let memo = self
            .store
            .fragment_index_source(&node_id, file.id, &object_version)
            .await
            .ok()
            .flatten();
        let attestation = tokio::select! {
            result = crate::fragment_index_cluster::attest_source(
                &node_id,
                &file,
                memo.as_ref(),
            ) => Some(result),
            () = self.wait_for_cluster_fragment_index_stop(
                transcode.as_ref(),
                &permit_lost,
            ) => None,
            () = tokio::time::sleep(ATTEST_TIMEOUT) => {
                Some(Err("source attestation timed out".to_owned()))
            }
        };
        let Some(attestation) = attestation else {
            let now = clock_ms();
            let _ = self
                .store
                .yield_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
        };
        let attested = match attestation {
            Ok(attested) if attested.observation.source_sha256 == job.source_sha256 => attested,
            Ok(_) | Err(_) => {
                let now = clock_ms();
                if job.target_node_id.is_empty() {
                    self.remember_fragment_index_refusal(
                        &job.cache_key,
                        now.saturating_add(LOCAL_REFUSAL_MS),
                    )
                    .await;
                    let _ = self
                        .store
                        .yield_cluster_fragment_index(
                            &job.cache_key,
                            &job.target_node_id,
                            &node_id,
                            job.fence,
                            now,
                            now,
                        )
                        .await;
                } else {
                    let _ = self
                        .store
                        .fail_cluster_fragment_index(
                            &job.cache_key,
                            &job.target_node_id,
                            &node_id,
                            job.fence,
                            "source_attestation_failed",
                            true,
                            now,
                            now.saturating_add(retry_ms),
                        )
                        .await;
                }
                finish_heartbeat(stop, heartbeat).await;
                return false;
            }
        };
        let _ = self
            .store
            .record_fragment_index_source(&attested.observation)
            .await;
        // The job names one pipeline by digest; this node offers one identity
        // per copy pipeline the file can be asked for. Claim the job only if
        // one of them still produces the bytes the job was queued for — a job
        // whose pipeline this build no longer emits is superseded exactly as
        // it was when there was only ever one identity to compare.
        let Some(video) = videos.into_iter().find(|video| {
            crate::fragment_index_cluster::pipeline_digest(&file, &worker.engine_sha256, *video)
                == job.pipeline_sha256
        }) else {
            let now = clock_ms();
            let _ = self
                .store
                .fail_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    "pipeline_superseded",
                    false,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
        };

        let progress_jobs = Arc::clone(&self);
        let progress_key = job.cache_key.clone();
        let progress_target = job.target_node_id.clone();
        let (outcome, preempted) = tokio::select! {
            outcome = crate::fragindex::build_from_attested_file_with_progress(
                &file,
                &attested.handle,
                video,
                transcode.runtime_cache_dir(),
                index_file_budget(file.duration_ms),
                move |bytes_read, media_ms, fragments| {
                    progress_jobs.update_analysis_progress(
                        &progress_key,
                        &progress_target,
                        "fragment_index",
                        bytes_read,
                        media_ms,
                        fragments,
                    );
                },
            ) => (Some(outcome), false),
            () = lost.cancelled() => (None, false),
            () = self.wait_for_cluster_fragment_index_stop(
                transcode.as_ref(),
                &permit_lost,
            ) => (None, true),
        };
        if preempted {
            let now = clock_ms();
            let _ = self
                .store
                .yield_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
        }
        let Some(outcome) = outcome else {
            finish_heartbeat(stop, heartbeat).await;
            return false;
        };
        let index = match outcome {
            crate::fragindex::IndexOutcome::Built(index) => index,
            crate::fragindex::IndexOutcome::Truncated { reason, .. } => {
                tracing::warn!(file_id = file.id, %reason, "cluster fragment index incomplete");
                let now = clock_ms();
                let _ = self
                    .store
                    .fail_cluster_fragment_index(
                        &job.cache_key,
                        &job.target_node_id,
                        &node_id,
                        job.fence,
                        "truncated",
                        false,
                        now,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                finish_heartbeat(stop, heartbeat).await;
                return false;
            }
            crate::fragindex::IndexOutcome::Unsupported(reason) => {
                tracing::warn!(file_id = file.id, %reason, "cluster fragment index unsupported");
                let now = clock_ms();
                let _ = self
                    .store
                    .fail_cluster_fragment_index(
                        &job.cache_key,
                        &job.target_node_id,
                        &node_id,
                        job.fence,
                        "unsupported",
                        false,
                        now,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                finish_heartbeat(stop, heartbeat).await;
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
        if !crate::fragment_index_cluster::source_still_matches(
            &attested.handle,
            &attested.observation,
        )
        .unwrap_or(false)
        {
            let now = clock_ms();
            let _ = self
                .store
                .fail_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    "source_changed",
                    false,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
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
            let _ = self
                .store
                .fail_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    "source_superseded",
                    false,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
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
                let _ = self
                    .store
                    .fail_cluster_fragment_index(
                        &job.cache_key,
                        &job.target_node_id,
                        &node_id,
                        job.fence,
                        "encode_failed",
                        false,
                        now,
                        now.saturating_add(retry_ms),
                    )
                    .await;
                finish_heartbeat(stop, heartbeat).await;
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
        if lost.is_cancelled()
            || permit_lost.is_cancelled()
            || !transcode.pretranscode_worker_idle()
            || !self.cluster_fragment_index_enabled().await
            || !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            let now = clock_ms();
            let _ = self
                .store
                .yield_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
        }
        if let Err(error) =
            crate::fragment_index_cluster::install_local_blob(&worker.cache_root, &artifact, &blob)
                .await
        {
            tracing::warn!(file_id = file.id, %error, "publishing local fragment-index blob");
            let now = clock_ms();
            let _ = self
                .store
                .fail_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    "local_publish_failed",
                    true,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
        }
        if lost.is_cancelled()
            || permit_lost.is_cancelled()
            || !transcode.pretranscode_worker_idle()
            || !self.cluster_fragment_index_enabled().await
            || !crate::ffmpeg::fragment_index_engine_is_current().await
        {
            let now = clock_ms();
            let _ = self
                .store
                .yield_cluster_fragment_index(
                    &job.cache_key,
                    &job.target_node_id,
                    &node_id,
                    job.fence,
                    now,
                    now.saturating_add(retry_ms),
                )
                .await;
            finish_heartbeat(stop, heartbeat).await;
            return false;
        }
        let completed_at_ms = clock_ms();
        let location = ClusterFragmentIndexLocation {
            cache_key: job.cache_key.clone(),
            node_id: node_id.clone(),
            bytes: artifact.bytes,
            verified_at_ms: completed_at_ms,
            last_seen_at_ms: completed_at_ms,
        };
        let completed = match self
            .store
            .complete_cluster_fragment_index(&job, &artifact, &location, completed_at_ms)
            .await
        {
            Ok(true) => {
                if let Err(error) = self.store.put_fragment_index(file.id, &index).await {
                    tracing::warn!(file_id = file.id, %error, "installing built fragment index");
                }
                self.analysis_metrics
                    .publication("fragment_index", job.priority == "forced");
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "settling cluster fragment index");
                false
            }
        };
        finish_heartbeat(stop, heartbeat).await;
        completed
    }

    async fn work_pretranscode_queue(self: Arc<Self>, transcode: Arc<TranscodeManager>) {
        let Some((root, node)) = transcode.cache_location() else {
            return;
        };
        if self.producing.swap(true, Ordering::Relaxed) {
            return;
        }
        let _running = ProducingGuard(Arc::clone(&self));
        const LOCAL_SWEEP_INTERVAL_MS: i64 = 15 * 60 * 1_000;
        let sweep_now = clock_ms();
        let previous = self
            .last_pretranscode_cache_sweep_ms
            .load(Ordering::Acquire);
        if sweep_now.saturating_sub(previous) >= LOCAL_SWEEP_INTERVAL_MS
            && self
                .last_pretranscode_cache_sweep_ms
                .compare_exchange(previous, sweep_now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // This runs even when the queue is empty. Every producer-enabled
            // node therefore maintains its own disk without multiplying work
            // on each scheduler tick.
            crate::cachekeep::sweep_with_readers(
                &self.store,
                root,
                node,
                transcode.cache_readers(),
                now(),
            )
            .await;
        }
        let cache_ceiling = match crate::cachekeep::budget_bytes_fallible(&self.store).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "speculative queue cannot read its cache budget");
                return;
            }
        };

        let deadline = std::time::Instant::now() + PRODUCE_WINDOW;
        let mut produced = 0_u64;
        let mut skipped = 0_u64;
        let mut reasons = std::collections::BTreeMap::<&'static str, u64>::new();
        let has_refusals = !self.pretranscode_refusals.lock().await.is_empty();
        if has_refusals {
            let active_job_ids = match self.store.active_pretranscode_job_ids().await {
                Ok(ids) if ids.len() <= 4_096 => ids.into_iter().collect::<HashSet<_>>(),
                Ok(_) => {
                    tracing::error!("active speculative queue exceeded its hard row bound");
                    *reasons.entry("active_queue_bound_exceeded").or_default() += 1;
                    self.emit_producer_pass(produced, skipped, serde_json::json!(reasons));
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "could not prune stale node-local queue refusals");
                    *reasons.entry("refusal_prune_failed").or_default() += 1;
                    self.emit_producer_pass(produced, skipped, serde_json::json!(reasons));
                    return;
                }
            };
            let now_unix_ms = clock_ms();
            self.pretranscode_refusals
                .lock()
                .await
                .retain(|id, retry_at| *retry_at > now_unix_ms && active_job_ids.contains(id));
        }
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
            let used_bytes = match self.store.cache_bytes(node).await {
                Ok(bytes) => bytes.max(0),
                Err(error) => {
                    tracing::warn!(%error, "could not read durable cache usage before queue claim");
                    *reasons.entry("cache_usage_unavailable").or_default() += 1;
                    break;
                }
            };
            let remaining_budget = cache_ceiling.saturating_sub(used_bytes);
            if remaining_budget <= 0 {
                tracing::info!(
                    used_bytes,
                    cache_ceiling,
                    "speculative worker stopped at cache budget"
                );
                *reasons.entry("cache_budget_full").or_default() += 1;
                break;
            }
            let mut capabilities = transcode.pretranscode_capabilities();
            // Requirements carry the estimated complete artifact plus 64 MiB
            // generation headroom. Advertising only the lesser of physical
            // scratch and durable budget remainder prevents a claim whose
            // expected publication is already known not to fit.
            capabilities.scratch_bytes = capabilities.scratch_bytes.min(remaining_budget);
            if !capabilities.validate() || capabilities.scratch_bytes == 0 {
                tracing::warn!(
                    decoders = ?capabilities.decoders,
                    scratch_bytes = capabilities.scratch_bytes,
                    "speculative worker has no proved decoder inventory or cache scratch"
                );
                *reasons.entry("unproved_capability_or_scratch").or_default() += 1;
                break;
            }
            let now_unix_ms = clock_ms();
            let expires_at = now_unix_ms
                .saturating_add(PRETRANSCODE_LEASE_TTL.as_millis().min(i64::MAX as u128) as i64);
            let excluded_job_ids = {
                let mut refusals = self.pretranscode_refusals.lock().await;
                refusals.retain(|_, retry_at| *retry_at > now_unix_ms);
                refusals
                    .keys()
                    .take(MAX_PRETRANSCODE_REFUSALS)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let job = match self
                .store
                .claim_pretranscode_job(
                    node,
                    &capabilities,
                    &excluded_job_ids,
                    now_unix_ms,
                    expires_at,
                )
                .await
            {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "could not claim speculative-transcode work");
                    *reasons.entry("claim_failed").or_default() += 1;
                    break;
                }
            };
            let active = ActivePretranscodeJob::start(Arc::clone(&self.store), job.clone());
            let fence = active.fence();
            let lost = active.loss_token();
            let file = match self.store.get_file(job.file_id).await {
                Ok(Some(file))
                    if file.size == job.source_size && file.mtime == job.source_mtime =>
                {
                    file
                }
                Ok(Some(_)) | Ok(None) => {
                    let _ = fence
                        .cancel_job(self.store.as_ref(), "source_changed", clock_ms())
                        .await;
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
                    .yield_job(self.store.as_ref(), now_unix_ms, now_unix_ms)
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
                Ok(PretranscodeProduceOutcome::Yielded) if lost.is_cancelled() => {
                    skipped += 1;
                    *reasons.entry("lease_lost").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::Yielded) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .yield_job(
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
                    let _ = fence
                        .cancel_job(self.store.as_ref(), "policy_changed", now_unix_ms)
                        .await;
                    skipped += 1;
                    *reasons.entry("policy_changed").or_default() += 1;
                }
                Ok(PretranscodeProduceOutcome::SourceChanged) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .cancel_job(self.store.as_ref(), "source_changed", now_unix_ms)
                        .await;
                    skipped += 1;
                    *reasons.entry("source_changed").or_default() += 1;
                }
                Err(error) if crate::transcode::is_retryable_capacity_error(&error) => {
                    let now_unix_ms = clock_ms();
                    let _ = fence
                        .yield_job(
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
                    let exponent = u32::try_from(job.attempts.clamp(0, 7)).unwrap_or(0);
                    let backoff_ms = 30_000_i64
                        .saturating_mul(1_i64 << exponent)
                        .min(60 * 60 * 1_000);
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
mod tests {
    use super::*;

    #[test]
    fn transient_fragment_source_reads_are_retryable_not_stale() {
        assert!(matches!(
            classify_fragment_source_read(
                Err(StoreError::Database("temporary authority read".to_owned())),
                10,
                20,
            ),
            Err(FragmentSourceReadFailure::Transient)
        ));
        assert!(matches!(
            classify_fragment_source_read(Ok(None), 10, 20),
            Err(FragmentSourceReadFailure::Stale)
        ));
    }
    use plurx_core::domain::{ItemKind, NewItem, NewLibrary, PlaybackEventQuery};
    use plurx_core::store::{
        LibraryStore, MediaStore, PlaybackTelemetryStore, SettingsStore, SqliteStore,
    };
    use plurx_core::transcode::Pipeline;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    fn metrics_sample(value: i64) -> PrometheusStoreSnapshot {
        PrometheusStoreSnapshot {
            libraries: value,
            users: value,
            offline: OfflinePackageStats {
                queued: value,
                preparing: value,
                ready: value,
                failed: value,
                queued_bytes: value,
                preparing_bytes: value,
                ready_bytes: value,
                failed_bytes: value,
                active_leases: value,
                pinned_bytes: value,
            },
            watched_outbox: (value, value, value),
            analysis: plurx_core::store::AnalysisStoreMetrics {
                queue_depth: [value; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
                queue_oldest_age_seconds: [value; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
                lifecycle_counts: [value; plurx_core::store::ANALYSIS_LIFECYCLE_METRIC_SLOTS],
                marker_counts: [value; plurx_core::store::ANALYSIS_MARKER_METRIC_SLOTS],
            },
        }
    }

    #[test]
    fn store_metrics_cache_distinguishes_absent_stale_and_failed_samples() {
        let cache = StoreMetricsCache::default();
        assert_eq!(
            cache.snapshot_at(0),
            StoreMetricsView {
                sample: None,
                age_seconds: None,
                valid: false,
                errors: 0,
            }
        );

        cache.record_error();
        assert_eq!(cache.snapshot_at(30).sample, None);
        assert_eq!(cache.snapshot_at(30).errors, 1);

        let complete = metrics_sample(7);
        cache.publish_at(complete, 40);
        assert_eq!(
            cache.snapshot_at(41),
            StoreMetricsView {
                sample: Some(complete),
                age_seconds: Some(1),
                valid: true,
                errors: 1,
            }
        );

        cache.record_error();
        let stale = cache.snapshot_at(40 + STORE_METRICS_FRESHNESS_SECS + 1);
        assert_eq!(stale.sample, Some(complete));
        assert_eq!(stale.age_seconds, Some(STORE_METRICS_FRESHNESS_SECS + 1));
        assert!(!stale.valid);
        assert_eq!(stale.errors, 2);
    }

    #[test]
    fn store_metrics_cache_never_exposes_a_torn_complete_sample() {
        let cache = StoreMetricsCache::default();
        cache.publish_at(metrics_sample(1), 0);
        let writer = cache.clone();
        let handle = std::thread::spawn(move || {
            for ordinal in 0..10_000 {
                writer.publish_at(metrics_sample(if ordinal % 2 == 0 { 2 } else { 3 }), 0);
            }
        });
        for _ in 0..10_000 {
            let sample = cache
                .snapshot_at(0)
                .sample
                .expect("the initial complete sample remains present");
            let expected = sample.libraries;
            assert_eq!(sample, metrics_sample(expected));
        }
        handle.join().expect("join metrics publisher");
    }

    #[test]
    fn re_pairing_provider_art_waits_for_its_replacement_cover() {
        assert!(
            curator_pairing_can_advance(None, false, "edition:new", false),
            "an initial optional or failed cover may retain embedded EPUB art"
        );
        assert!(
            curator_pairing_can_advance(Some("edition:old"), false, "edition:new", false),
            "EPUB-backed Curator facts may advance without provider art"
        );
        assert!(
            !curator_pairing_can_advance(Some("edition:old"), true, "edition:new", false),
            "fetch failure or publication-slot contention must retain the coherent old pair"
        );
        assert!(curator_pairing_can_advance(
            Some("edition:old"),
            true,
            "edition:new",
            true
        ));
        assert!(
            curator_pairing_can_advance(Some("edition:same"), true, "edition:same", false),
            "refreshing facts for the same edition may retain its provider cover"
        );
    }

    fn manager(store: Arc<dyn Store>, artwork: &std::path::Path) -> Arc<JobManager> {
        Arc::new(JobManager::new(store, artwork.to_path_buf()))
    }

    #[test]
    fn analysis_backoff_is_stable_exponential_jitter_with_a_cap() {
        let policy = AnalysisRetryPolicy::from_settings(&BTreeMap::new());
        assert_eq!(
            policy.lease_ms,
            plurx_core::store::DEFAULT_ANALYSIS_LEASE_SECS * 1_000
        );
        let first = policy.backoff_ms("request-a", 1);
        assert_eq!(first, policy.backoff_ms("request-a", 1));
        assert!((3_750..=6_250).contains(&first));
        let later = policy.backoff_ms("request-a", 6);
        assert!(later >= first);
        assert!(later <= policy.backoff_max_ms);
        assert_eq!(
            policy.backoff_ms("request-a", 60),
            policy.backoff_max_ms,
            "a large attempt cannot overflow or exceed the configured ceiling"
        );
    }

    #[test]
    fn analysis_progress_registry_is_bounded_and_removes_completed_work() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(store, artwork.path());
        let mut guards = Vec::new();
        for index in 0..(MAX_ANALYSIS_PROGRESS + 8) {
            guards.push(jobs.start_analysis_progress(
                (&format!("job-{index:03}"), ""),
                index as i64,
                "fragment_index",
                "probing",
                1_000,
                2_000,
            ));
        }
        assert_eq!(
            jobs.analysis_progress_snapshot().len(),
            MAX_ANALYSIS_PROGRESS
        );
        let newest = format!("job-{:03}", MAX_ANALYSIS_PROGRESS + 7);
        jobs.update_analysis_progress(&newest, "", "fragment_index", 500, 1_000, 12);
        let row = jobs
            .analysis_progress_snapshot()
            .into_iter()
            .find(|row| row.job_id == newest)
            .expect("newest progress retained");
        assert_eq!(row.stage, "fragment_index");
        assert_eq!(row.fragments_indexed, 12);
        drop(guards);
        assert!(jobs.analysis_progress_snapshot().is_empty());
        assert!(jobs
            .analysis_metrics
            .current_by_stage
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 0));

        let old =
            jobs.start_analysis_progress(("same-job", ""), 1, "skip_markers", "probing", 1, 1);
        let current =
            jobs.start_analysis_progress(("same-job", ""), 1, "skip_markers", "persisting", 1, 1);
        drop(old);
        assert_eq!(jobs.analysis_progress_snapshot()[0].stage, "persisting");
        drop(current);
        assert!(jobs.analysis_progress_snapshot().is_empty());
        assert!(jobs
            .analysis_metrics
            .current_by_stage
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 0));

        let generic = jobs.start_analysis_progress(
            ("shared-cache", ""),
            1,
            "fragment_index",
            "verifying",
            1,
            1,
        );
        let targeted = jobs.start_analysis_progress(
            ("shared-cache", "node-a"),
            1,
            "fragment_index",
            "fragment_index",
            1,
            1,
        );
        let snapshot = jobs.analysis_progress_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.iter().any(|row| row.stage == "verifying"));
        assert!(snapshot.iter().any(|row| row.stage == "fragment_index"));
        drop(generic);
        assert_eq!(jobs.analysis_progress_snapshot().len(), 1);
        assert_eq!(jobs.analysis_progress_snapshot()[0].stage, "fragment_index");
        drop(targeted);
        assert!(jobs.analysis_progress_snapshot().is_empty());
        assert!(jobs
            .analysis_metrics
            .current_by_stage
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 0));
    }

    fn manager_with_tmdb(
        store: Arc<dyn Store>,
        artwork: &std::path::Path,
        base: &str,
    ) -> Arc<JobManager> {
        let mut manager = JobManager::new(store, artwork.to_path_buf());
        manager.tmdb_base = Some((base.to_owned(), base.to_owned()));
        Arc::new(manager)
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn three_cluster_ticks_run_one_provider_pass() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("cluster tick store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let transcode_dir = crate::test_tempdir().expect("transcode");
        seeded_episode_backlog(&store, 1).await;
        let hits = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(blocking_season_tmdb(
            Arc::clone(&hits),
            Arc::clone(&entered),
            Arc::clone(&release),
        ))
        .await;
        let manager = |node: &str| {
            let store: Arc<dyn Store> = store.clone();
            let mut manager = JobManager::new_with_scan_prune_percent(
                store,
                artwork.path().to_path_buf(),
                plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
                node.to_owned(),
                Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
            );
            manager.tmdb_base = Some((base.clone(), base.clone()));
            Arc::new(manager)
        };
        let a = manager("tick-a");
        let b = manager("tick-b");
        let c = manager("tick-c");
        let transcode_store: Arc<dyn Store> = store.clone();
        let transcode = Arc::new(TranscodeManager::new(
            transcode_store,
            transcode_dir.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let (a_tick, b_tick, c_tick) = tokio::join!(
            a.run_due_jobs(&transcode),
            b.run_due_jobs(&transcode),
            c.run_due_jobs(&transcode),
        );
        a_tick.expect("tick a");
        b_tick.expect("tick b");
        c_tick.expect("tick c");
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("winning provider pass started");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "three real scheduler ticks must dispatch one provider pass"
        );
        release.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while a.retrying_artwork.load(Ordering::Relaxed)
                || b.retrying_artwork.load(Ordering::Relaxed)
                || c.retrying_artwork.load(Ordering::Relaxed)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider owner released its lease");
    }

    #[tokio::test]
    async fn aborted_job_owner_stops_heartbeat_and_allows_ttl_takeover() {
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("aborted owner store"));
        let first =
            StoreCoordinator::new(Arc::clone(&store), "aborted-owner").expect("first coordinator");
        let successor =
            StoreCoordinator::new(Arc::clone(&store), "successor").expect("successor coordinator");
        let lease = match first
            .acquire("test:aborted-owner", std::time::Duration::from_secs(1))
            .await
            .expect("first acquire")
        {
            LeaseClaim::Acquired(lease) => lease,
            held => panic!("first owner must acquire, got {held:?}"),
        };
        let active = ActiveJobLease::start_with_policy(
            first,
            lease,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(50),
        )
        .expect("valid test lease policy");
        let owner = tokio::spawn(async move {
            let active = active;
            std::future::pending::<()>().await;
            drop(active);
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        owner.abort();
        assert!(owner
            .await
            .expect_err("owner task was aborted")
            .is_cancelled());

        tokio::time::sleep(std::time::Duration::from_millis(1_150)).await;
        assert!(matches!(
            successor
                .acquire("test:aborted-owner", std::time::Duration::from_secs(1))
                .await
                .expect("takeover acquire"),
            LeaseClaim::Acquired(_)
        ));
    }

    #[tokio::test]
    async fn empty_pretranscode_queue_rate_limits_local_cache_maintenance() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("empty queue store"));
        store
            .put_setting(keys::CACHE_MAX_GB, "50")
            .await
            .expect("enable cache");
        let artwork = crate::test_tempdir().expect("artwork");
        let cache = crate::test_tempdir().expect("cache");
        let cache_root = cache.path().join("transcode");
        let recipe = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let orphan = cache_root
            .join("dd")
            .join(format!("{recipe}-j00000000-0000-4000-8000-000000000303-f1"));
        tokio::fs::create_dir_all(&orphan)
            .await
            .expect("queue-shaped orphan");
        tokio::fs::write(orphan.join("index.m3u8"), b"unbound")
            .await
            .expect("orphan bytes");

        let shared: Arc<dyn Store> = store;
        let jobs = manager(Arc::clone(&shared), artwork.path());
        let transcode = Arc::new(
            TranscodeManager::new(
                shared,
                cache.path().join("work"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_decoders(vec!["h264".to_owned()])
            .with_cache(cache_root, "test-ffmpeg".to_owned(), "test-node".to_owned()),
        );

        Arc::clone(&jobs).work_pretranscode_queue(transcode).await;
        assert!(
            !orphan.exists(),
            "a producer-enabled node did not maintain its local cache on an empty queue"
        );
    }

    fn targeted_show_tmdb(
        show_hits: Arc<AtomicUsize>,
        season_one_hits: Arc<AtomicUsize>,
        season_one_poster: Option<&'static str>,
    ) -> axum::Router {
        use axum::routing::get;
        use axum::Json;

        axum::Router::new()
            .route(
                "/tv/42",
                get(move || {
                    let hits = Arc::clone(&show_hits);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "id": 42,
                            "name": "Severance",
                            "first_air_date": "2022-02-18",
                            "poster_path": "/show.jpg",
                            "backdrop_path": "/backdrop.jpg"
                        }))
                    }
                }),
            )
            .route(
                "/tv/42/season/1",
                get(move || {
                    let hits = Arc::clone(&season_one_hits);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "poster_path": season_one_poster,
                            "episodes": [{
                                "episode_number": 1,
                                "name": "Good News About Hell",
                                "still_path": "/episode-1.jpg"
                            }]
                        }))
                    }
                }),
            )
            .route(
                "/tv/42/season/2",
                get(|| async {
                    Json(json!({
                        "poster_path": "/season-2.jpg",
                        "episodes": [{
                            "episode_number": 1,
                            "name": "Hello, Ms. Cobel",
                            "still_path": "/episode-2.jpg"
                        }]
                    }))
                }),
            )
            // Image paths are served by the same base in this test.
            .fallback(get(|| async { vec![0_u8, 1, 2, 3] }))
    }

    fn empty_season_tmdb(season_hits: Arc<AtomicUsize>) -> axum::Router {
        use axum::routing::get;
        use axum::Json;

        axum::Router::new().route(
            "/tv/42/season/1",
            get(move || {
                let hits = Arc::clone(&season_hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "poster_path": "/season-1.jpg",
                        "episodes": []
                    }))
                }
            }),
        )
    }

    fn blocking_season_tmdb(
        season_hits: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> axum::Router {
        use axum::routing::get;
        use axum::Json;

        axum::Router::new().route(
            "/tv/42/season/1",
            get(move || {
                let hits = Arc::clone(&season_hits);
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    Json(json!({
                        "poster_path": "/season-1.jpg",
                        "episodes": []
                    }))
                }
            }),
        )
    }

    async fn seeded_enriched_show(store: &SqliteStore) -> (i64, i64, i64) {
        let lib = store
            .create_library(&NewLibrary {
                name: "TV".into(),
                kind: LibraryKind::Shows,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("lib");
        store
            .put_setting(keys::TMDB_API_KEY, "test-key")
            .await
            .expect("key");
        let show = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Show,
                parent_id: None,
                title: "Severance".into(),
                year: Some(2022),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("show");
        let season = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Season,
                parent_id: Some(show),
                title: "Season 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: None,
            })
            .await
            .expect("season");
        store
            .apply_metadata(
                show,
                &MetadataPatch {
                    tmdb_id: Some(42),
                    poster_path: Some("existing-show.jpg".into()),
                    backdrop_path: Some("existing-backdrop.jpg".into()),
                    enriched: true,
                    artwork: Some(ArtworkAttempt::Stored),
                    ..Default::default()
                },
            )
            .await
            .expect("enrich show");
        (lib.id, show, season)
    }

    async fn seeded_episode_backlog(store: &SqliteStore, count: usize) -> Vec<i64> {
        let (library_id, _, season) = seeded_enriched_show(store).await;
        store
            .apply_metadata(
                season,
                &MetadataPatch {
                    poster_path: Some("existing-season.jpg".into()),
                    artwork: Some(ArtworkAttempt::Stored),
                    ..Default::default()
                },
            )
            .await
            .expect("healthy season");
        let mut episodes = Vec::with_capacity(count);
        for number in 1..=count {
            episodes.push(
                store
                    .insert_item(&NewItem {
                        library_id,
                        kind: ItemKind::Episode,
                        parent_id: Some(season),
                        title: format!("Episode {number}"),
                        year: None,
                        season_number: Some(1),
                        episode_number: Some(number as i32),
                    })
                    .await
                    .expect("episode"),
            );
        }
        episodes
    }

    fn scan_request_fixture(id: impl Into<String>, library_id: i64) -> ScanRequest {
        ScanRequest {
            id: id.into(),
            library_id,
            path: PathBuf::from("/missing/target"),
            ids: None,
            book: None,
            correlation_id: Some("correlation".into()),
            source: Some("test".into()),
        }
    }

    #[tokio::test]
    async fn job_lifecycle_helpers_publish_progress_and_clear_every_guard() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(Arc::clone(&store), artwork.path());

        assert!(
            !jobs.stop_producing(),
            "an idle producer has nothing to stop"
        );
        jobs.producing.store(true, Ordering::Relaxed);
        jobs.set_producing(Some(ProducingNow {
            title: "Heat".into(),
            reason: crate::produce::REASON_RECENT.to_owned(),
            index: 1,
            total: 2,
        }))
        .await;
        assert!(jobs.stop_producing());
        assert_eq!(jobs.producing_now().await.expect("activity").title, "Heat");
        drop(ProducingGuard(Arc::clone(&jobs)));
        assert!(!jobs.producing.load(Ordering::Relaxed));
        assert!(!jobs.stop_producing.load(Ordering::Relaxed));
        assert!(jobs.producing_now().await.is_none());

        jobs.backfilling_genres.store(true, Ordering::Relaxed);
        drop(GenreBackfillGuard(Arc::clone(&jobs)));
        assert!(!jobs.backfilling_genres.load(Ordering::Relaxed));
        jobs.retrying_artwork.store(true, Ordering::Relaxed);
        drop(ArtworkRetryGuard(Arc::clone(&jobs)));
        assert!(!jobs.retrying_artwork.load(Ordering::Relaxed));

        let progress = Arc::new(ScanProgress::default());
        progress.found.store(5, Ordering::Relaxed);
        progress.processed.store(3, Ordering::Relaxed);
        progress.changed.store(2, Ordering::Relaxed);
        jobs.statuses.lock().await.insert(
            7,
            ScanStatus {
                running: true,
                ..Default::default()
            },
        );
        jobs.live.lock().await.insert(7, Arc::clone(&progress));
        let status = jobs.all_statuses().await.remove(&7).expect("live status");
        let snapshot = status.progress.expect("sampled progress");
        assert_eq!(
            (snapshot.found, snapshot.processed, snapshot.changed),
            (5, 3, 2)
        );

        jobs.finish(7, ScanStatus::default()).await;
        let finished = jobs
            .all_statuses()
            .await
            .remove(&7)
            .expect("finished status");
        assert!(!finished.running);
        assert!(finished.finished_at.is_some());
        assert!(finished.progress.is_none());
        let failed = error_status("disk unavailable");
        assert_eq!(failed.error.as_deref(), Some("disk unavailable"));
        assert!(failed.finished_at.is_some());
    }

    #[tokio::test]
    async fn job_settings_are_bounded_and_stamps_round_trip() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(Arc::clone(&store), artwork.path());

        assert_eq!(jobs.job_interval("missing").await, 0);
        assert_eq!(jobs.job_interval_or("missing", 17).await, 17);
        store
            .put_setting("job.test", " -3 ")
            .await
            .expect("negative");
        assert_eq!(jobs.job_interval("job.test").await, 0);
        store
            .put_setting("job.test", " 42 ")
            .await
            .expect("interval");
        assert_eq!(jobs.job_interval("job.test").await, 42);
        assert_eq!(jobs.job_stamp("job.test").await, Some(42));
        store
            .put_setting("job.test", "invalid")
            .await
            .expect("invalid");
        assert_eq!(jobs.job_stamp("job.test").await, None);

        let lease = jobs
            .acquire_job("test:stamp".to_owned())
            .await
            .expect("stamp lease")
            .expect("stamp owner");
        let publisher = lease.publisher(store.as_ref());
        jobs.stamp("job.stamped", &publisher).await;
        drop(publisher);
        let _ = lease.release().await;
        let stamped = jobs.job_stamp("job.stamped").await.expect("stamp");
        assert!((now() - stamped).abs() <= 1);
    }

    /// A job authority whose answer can be moved while the manager holding it
    /// keeps running, which is the whole point: committed membership moves
    /// under a live daemon and the gate has to follow it without a restart.
    struct MovableJobAuthority(std::sync::atomic::AtomicBool);

    impl MovableJobAuthority {
        fn learner() -> Arc<Self> {
            Arc::new(Self(std::sync::atomic::AtomicBool::new(false)))
        }

        fn promote(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[plurx_core::cluster::coordination::cluster_job_async_trait]
    impl ClusterJobAuthority for MovableJobAuthority {
        async fn may_run_cluster_jobs(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Every cluster-wide singleton resource, named individually.
    ///
    /// Spelled out rather than derived, so adding a sixth job without deciding
    /// whether a node with no vote may run it fails here.
    const CLUSTER_SINGLETON_RESOURCES: &[&str] = &[
        "provider:artwork",
        "provider:genres",
        "scan:library:1",
        "repair:probe",
        "candidate:pretranscode",
    ];

    /// Leases that are singletons but not *cluster* singletons, named so this
    /// enumeration is honest about what it does and does not cover.
    ///
    /// `shared-cache-gc:{storage_id}` is taken directly on the store by
    /// `shared_cache::gc_once_inner` rather than through `acquire_cluster_job`,
    /// and that is deliberate: it is a per-shared-volume singleton owned by
    /// whichever node has the volume mounted. Its work is the same work
    /// whoever runs it, it is fenced against a concurrent successor, and it
    /// has to keep happening on a node with no vote — a learner that mounts
    /// the volume is exactly as responsible for it as a voter is.
    ///
    /// Listing it here rather than leaving the gap unstated is the point: an
    /// enumeration that silently omits a resource cannot fail.
    const SHARED_STORAGE_SINGLETON_RESOURCES: &[&str] = &["shared-cache-gc:{storage_id}"];

    /// A node with no vote acquires none of the cluster's singleton leases —
    /// and the store it is talking to is perfectly writable, so the refusal is
    /// the gate rather than an incidental failure.
    #[tokio::test]
    async fn a_node_without_a_vote_acquires_no_cluster_job() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = tempfile::tempdir().expect("artwork");
        let authority = MovableJobAuthority::learner();
        let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
            Arc::clone(&store),
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "learner-node".to_owned(),
            authority.clone(),
        ));

        assert!(!jobs.may_run_cluster_jobs().await);
        for resource in CLUSTER_SINGLETON_RESOURCES {
            let claimed = jobs
                .acquire_job((*resource).to_owned())
                .await
                .expect("lease acquisition must not error");
            assert!(
                claimed.is_none(),
                "a node with no vote acquired the {resource} lease"
            );
        }

        // Nothing took the leases, so the store still hands every one of them
        // to a node that is allowed to ask.
        let voter = Arc::new(JobManager::new_with_scan_prune_percent(
            Arc::clone(&store),
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "voter-node".to_owned(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        ));
        for resource in CLUSTER_SINGLETON_RESOURCES {
            let claimed = voter
                .acquire_job((*resource).to_owned())
                .await
                .expect("lease acquisition")
                .unwrap_or_else(|| panic!("an eligible node must acquire {resource}"));
            let _ = claimed.release().await;
        }
    }

    /// The lease that bypasses the membership gate is named, not denied.
    ///
    /// `acquire_cluster_job` claimed to be "the only gate the whole
    /// singleton-job surface passes through". It was not: the shared-cache GC
    /// takes `shared-cache-gc:{storage_id}` directly on the store. Nothing is
    /// corrupted by that — the GC is fenced and is a per-shared-volume
    /// singleton any mounting node may own, including a learner — but while
    /// the claim stood, the enumeration beside it could not fail, because a
    /// resource nobody had listed could not be missing.
    #[test]
    fn the_lease_that_bypasses_the_membership_gate_is_named_rather_than_denied() {
        // Built at runtime so this test's own source does not count as a call
        // site of what it is looking for.
        let needle = format!(".{}(", "acquire_lease");

        let gate = include_str!("job_lease.rs");
        assert!(
            !gate.contains("the only gate the whole singleton-job surface passes through"),
            "the false claim must not come back"
        );
        for resource in SHARED_STORAGE_SINGLETON_RESOURCES {
            assert!(
                gate.contains(resource),
                "the gate's contract must name the exception {resource}"
            );
        }

        // And the exception is exactly one call site, still outside the gate.
        let shared_cache = include_str!("shared_cache.rs");
        assert_eq!(
            shared_cache.matches(needle.as_str()).count(),
            SHARED_STORAGE_SINGLETON_RESOURCES.len(),
            "a second direct lease means the enumeration above is stale again"
        );
        assert!(shared_cache.contains("shared-cache-gc:"));
        assert!(
            !shared_cache.contains("acquire_cluster_job"),
            "routing it through the gate is a fine choice, but then it is no longer an exception \
             and both enumerations have to say so"
        );
    }

    /// The scheduler tick itself is gated, not only the leases it dispatches.
    ///
    /// Defence in depth, and it was untested: deleting the gate left every
    /// plurxd test green, because the individual leases refuse a learner
    /// anyway. The tick still matters — it is what keeps a node with no vote
    /// from doing the scheduler's reads and writing its log lines every minute
    /// — and the answer has to move the moment committed membership does,
    /// without a restart.
    #[tokio::test]
    async fn a_node_without_a_vote_dispatches_no_scheduler_tick() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = tempfile::tempdir().expect("artwork");
        let work = tempfile::tempdir().expect("transcode work");
        let authority = MovableJobAuthority::learner();
        let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
            Arc::clone(&store),
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "learner-node".to_owned(),
            authority.clone(),
        ));
        let transcode = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            work.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        assert!(
            !jobs.schedule_tick(&transcode).await,
            "a node with no vote must not dispatch a scheduler tick"
        );

        authority.promote();
        assert!(
            jobs.schedule_tick(&transcode).await,
            "and the same manager, never restarted, dispatches once it has a vote"
        );
    }

    /// The live property. The same manager, never restarted, starts acquiring
    /// leases the moment its committed role changes — which is what makes this
    /// a membership check rather than a boot-time flag.
    #[tokio::test]
    async fn a_promoted_node_starts_acquiring_without_a_restart() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = tempfile::tempdir().expect("artwork");
        let authority = MovableJobAuthority::learner();
        let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
            Arc::clone(&store),
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "promoted-node".to_owned(),
            authority.clone(),
        ));

        assert!(jobs
            .acquire_job("provider:artwork".to_owned())
            .await
            .expect("lease acquisition")
            .is_none());

        authority.promote();

        assert!(jobs.may_run_cluster_jobs().await);
        let lease = jobs
            .acquire_job("provider:artwork".to_owned())
            .await
            .expect("lease acquisition")
            .expect("a promoted node acquires the lease it was refused a moment ago");
        let _ = lease.release().await;
    }

    /// The scheduler's startup arm asks the same question, and asks it after
    /// its settle delay rather than before, so the answer is the one that
    /// holds at the moment work would actually begin.
    #[tokio::test(start_paused = true)]
    async fn the_startup_scan_does_not_run_without_a_vote() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = tempfile::tempdir().expect("media");
        let artwork = tempfile::tempdir().expect("artwork");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        store
            .put_setting(keys::JOB_SCAN_ON_STARTUP, "1")
            .await
            .expect("enable startup scan");
        let authority = MovableJobAuthority::learner();
        let store_handle: Arc<dyn Store> = store.clone();
        let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
            store_handle,
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "learner-node".to_owned(),
            authority.clone(),
        ));

        let settle = |jobs: Arc<JobManager>| async move {
            let startup = tokio::spawn(async move { jobs.scan_on_startup().await });
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_secs(30)).await;
            startup.await.expect("startup task");
        };

        settle(Arc::clone(&jobs)).await;
        assert!(
            jobs.all_statuses().await.is_empty(),
            "a node with no vote must not start the boot scan"
        );

        authority.promote();
        settle(Arc::clone(&jobs)).await;
        assert!(
            jobs.all_statuses().await.contains_key(&library.id),
            "and must start it once it carries one"
        );
    }

    #[tokio::test]
    async fn an_empty_vod_pass_does_not_delay_the_first_useful_index() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = tempfile::tempdir().expect("artwork");
        let transcode_dir = tempfile::tempdir().expect("transcode");
        let jobs = manager(store.clone(), artwork.path());
        let transcode = Arc::new(TranscodeManager::new(
            store.clone(),
            transcode_dir.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        Arc::clone(&jobs).build_fragment_indexes(transcode).await;

        let stamp = jobs.local_job_key(keys::JOB_LAST_VOD_INDEX);
        assert_eq!(
            store.get_setting(&stamp).await.expect("read VOD stamp"),
            None,
            "a boot tick before library creation must stay due for the first scan"
        );
        assert!(!jobs.indexing.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn analysis_hash_stop_signal_observes_foreground_playback() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        store
            .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
            .await
            .expect("enable analysis");
        let artwork = tempfile::tempdir().expect("artwork");
        let transcode_dir = tempfile::tempdir().expect("transcode");
        let jobs = manager(store.clone(), artwork.path());
        let transcode = Arc::new(TranscodeManager::new(
            store,
            transcode_dir.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let lost = tokio_util::sync::CancellationToken::new();
        let waiter = {
            let jobs = Arc::clone(&jobs);
            let transcode = Arc::clone(&transcode);
            let lost = lost.clone();
            tokio::spawn(async move {
                jobs.wait_for_cluster_fragment_index_stop(&transcode, &lost)
                    .await;
                lost.is_cancelled()
            })
        };
        tokio::task::yield_now().await;
        let _waiting_viewer = transcode.test_mark_live_waiting();
        let claim_was_lost = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("foreground demand cancels the hash selector")
            .expect("wait task");
        assert!(
            !claim_was_lost,
            "foreground cancellation is a no-charge retry, not claim loss"
        );
    }

    #[test]
    fn the_vod_index_cursor_wraps_past_failed_low_ids() {
        let paths = [1, 9, 17, 120, 5910]
            .into_iter()
            .map(|file_id| (file_id, PathBuf::from(format!("/{file_id}.mkv"))))
            .collect();
        let ordered: Vec<i64> = ordered_index_paths(paths, Some(17))
            .into_iter()
            .map(|(file_id, _)| file_id)
            .collect();
        assert_eq!(ordered, [120, 5910, 1, 9, 17]);
    }

    #[test]
    fn long_remuxes_receive_a_complete_pass_budget() {
        assert_eq!(
            index_file_budget(None),
            Duration::from_secs(INDEX_FILE_BUDGET_FLOOR_SECS)
        );
        let steel = index_file_budget(Some(7_095_005));
        assert!(steel >= Duration::from_secs(15 * 60));
        assert!(steel <= Duration::from_secs(INDEX_FILE_BUDGET_CEILING_SECS));
    }

    #[tokio::test]
    async fn targeted_request_failures_queue_every_waiter_and_stay_bounded() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(Arc::clone(&store), artwork.path());

        let missing = scan_request_fixture("missing", 404);
        assert!(jobs.request_scan(missing).await.is_err());
        let failed = jobs.scan_request("missing").await.expect("failed record");
        assert_eq!(failed.status, "failed");
        assert!(failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("not under any root")));

        jobs.statuses.lock().await.insert(
            405,
            ScanStatus {
                running: true,
                ..Default::default()
            },
        );
        assert!(jobs
            .request_scan(scan_request_fixture("queued-1", 405))
            .await
            .expect("queue first")
            .is_none());
        assert!(jobs
            .request_scan(scan_request_fixture("queued-2", 405))
            .await
            .expect("queue duplicate path waiter")
            .is_none());
        assert_eq!(
            jobs.pending.lock().await.get(&405).expect("pending").len(),
            2
        );
        assert_eq!(
            jobs.scan_request("queued-1")
                .await
                .expect("queued record")
                .status,
            "queued"
        );
        assert_eq!(
            jobs.scan_request("queued-2")
                .await
                .expect("coalesced record")
                .status,
            "queued"
        );
        jobs.drain_pending(405).await;
        assert_eq!(
            jobs.scan_request("queued-1")
                .await
                .expect("drained record")
                .status,
            "failed"
        );
        assert_eq!(
            jobs.scan_request("queued-2")
                .await
                .expect("second drained record")
                .status,
            "failed"
        );

        for index in 0..=MAX_REQUESTS {
            let request = scan_request_fixture(format!("ring-{index}"), 1);
            jobs.record_request(&request, "queued", None, None).await;
        }
        let records = jobs.scan_requests().await;
        assert_eq!(records.len(), MAX_REQUESTS);
        assert!(
            jobs.scan_request("ring-0").await.is_none(),
            "oldest record was evicted"
        );
        let newest = scan_request_fixture(format!("ring-{MAX_REQUESTS}"), 1);
        jobs.record_request(&newest, "done", None, None).await;
        assert_eq!(
            jobs.scan_request(&format!("ring-{MAX_REQUESTS}"))
                .await
                .expect("updated newest")
                .status,
            "done"
        );
    }

    #[tokio::test]
    async fn same_path_waiters_all_finish_and_apply_their_own_ids() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        let folder = media.path().join("Queued");
        std::fs::create_dir_all(&folder).expect("queued folder");
        std::fs::write(folder.join("clip.mp4"), b"not really video").expect("queued clip");
        let library = store
            .create_library(&NewLibrary {
                name: "Queued Home".to_owned(),
                kind: LibraryKind::Home,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let jobs = manager(store.clone(), artwork.path());
        jobs.statuses.lock().await.insert(
            library.id,
            ScanStatus {
                running: true,
                ..Default::default()
            },
        );

        let first = ScanRequest {
            id: "same-path-first".to_owned(),
            library_id: library.id,
            path: folder.clone(),
            ids: None,
            book: None,
            correlation_id: None,
            source: Some("first".to_owned()),
        };
        let second = ScanRequest {
            id: "same-path-second".to_owned(),
            ids: Some(IdHints {
                tmdb: Some(4242),
                ..Default::default()
            }),
            source: Some("second".to_owned()),
            ..first.clone()
        };
        assert!(jobs
            .request_scan(first)
            .await
            .expect("queue first")
            .is_none());
        assert!(jobs
            .request_scan(second)
            .await
            .expect("queue second")
            .is_none());
        jobs.statuses.lock().await.remove(&library.id);
        jobs.drain_pending(library.id).await;

        for request_id in ["same-path-first", "same-path-second"] {
            assert_eq!(
                jobs.scan_request(request_id)
                    .await
                    .expect("terminal waiter")
                    .status,
                "done",
                "every same-path waiter must reach a terminal state"
            );
        }
        let (counts, _) = jobs.metrics().snapshot();
        assert_eq!(
            counts
                .into_iter()
                .find(|(trigger, _)| *trigger == "targeted")
                .map(|(_, count)| count),
            Some(1),
            "same-path waiters share one physical scan/enrichment pass"
        );
        let second = jobs
            .scan_request("same-path-second")
            .await
            .expect("second waiter record");
        let item_id = second
            .items
            .expect("second waiter placed items")
            .first()
            .expect("placed item")
            .item_id;
        assert_eq!(
            store
                .get_item(item_id)
                .await
                .expect("item lookup")
                .expect("placed item")
                .tmdb_id,
            Some(4242),
            "the later waiter's caller-specific ids must not be discarded"
        );
    }

    #[tokio::test]
    async fn targeted_waiter_queue_is_bounded_and_overflow_is_terminal() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(store, artwork.path());
        let library_id = 406;
        jobs.statuses.lock().await.insert(
            library_id,
            ScanStatus {
                running: true,
                ..Default::default()
            },
        );
        for index in 0..MAX_PENDING_PER_LIBRARY {
            assert!(jobs
                .request_scan(scan_request_fixture(format!("bounded-{index}"), library_id))
                .await
                .expect("waiter within bound")
                .is_none());
        }
        let overflow = scan_request_fixture("bounded-overflow", library_id);
        let error = jobs
            .request_scan(overflow)
            .await
            .expect_err("overflow is explicit");
        assert!(error.to_string().contains("queue for library 406 is full"));
        assert_eq!(
            jobs.pending
                .lock()
                .await
                .get(&library_id)
                .expect("bounded queue")
                .len(),
            MAX_PENDING_PER_LIBRARY
        );
        assert_eq!(
            jobs.scan_request("bounded-overflow")
                .await
                .expect("overflow record")
                .status,
            "failed"
        );
    }

    #[tokio::test]
    async fn refresh_of_a_missing_library_finishes_with_an_error() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(store, artwork.path());
        assert!(jobs.trigger_refresh_as(999, ScanTrigger::Scheduled).await);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let status = jobs.all_statuses().await.remove(&999);
                if status.as_ref().is_some_and(|status| !status.running) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("missing library scan finished");
        let status = jobs.all_statuses().await.remove(&999).expect("status");
        assert_eq!(status.error.as_deref(), Some("library not found"));
        let (counts, _) = jobs.metrics().snapshot();
        assert_eq!(
            counts
                .into_iter()
                .find(|(trigger, _)| *trigger == "scheduled")
                .map(|(_, count)| count),
            Some(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn startup_scan_is_opt_in_and_runs_each_library_after_settling() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let jobs = manager(store.clone(), artwork.path());

        jobs.scan_on_startup().await;
        assert!(
            jobs.all_statuses().await.is_empty(),
            "startup scan is opt in"
        );

        store
            .put_setting(keys::JOB_SCAN_ON_STARTUP, "1")
            .await
            .expect("enable startup scan");
        let startup = tokio::spawn({
            let jobs = Arc::clone(&jobs);
            async move { jobs.scan_on_startup().await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        startup.await.expect("startup task");

        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if jobs
                    .all_statuses()
                    .await
                    .get(&library.id)
                    .is_some_and(|status| !status.running)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("startup scan finished");
        let status = jobs
            .all_statuses()
            .await
            .remove(&library.id)
            .expect("startup scan status");
        assert!(!status.running);
        assert!(status.last_scan.is_some());
        let (counts, _) = jobs.metrics().snapshot();
        assert_eq!(
            counts
                .into_iter()
                .find(|(trigger, _)| *trigger == "startup")
                .map(|(_, count)| count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn schedule_loop_dispatches_a_due_library_scan() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        let transcode = crate::test_tempdir().expect("transcode");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        store
            .set_library_schedule(library.id, 1, 0)
            .await
            .expect("schedule scan");
        let jobs = manager(store.clone(), artwork.path());
        let transcode = Arc::new(TranscodeManager::new(
            store.clone(),
            transcode.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        let scheduler = tokio::spawn(Arc::clone(&jobs).schedule_loop(transcode));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if jobs
                    .all_statuses()
                    .await
                    .get(&library.id)
                    .is_some_and(|status| !status.running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduled scan finished");
        scheduler.abort();
        assert!(scheduler
            .await
            .expect_err("scheduler keeps ticking")
            .is_cancelled());

        let status = jobs
            .all_statuses()
            .await
            .remove(&library.id)
            .expect("scheduled scan status");
        assert!(status.last_scan.is_some());
        assert!(status.error.is_none());
        let (counts, _) = jobs.metrics().snapshot();
        assert_eq!(
            counts
                .into_iter()
                .find(|(trigger, _)| *trigger == "scheduled")
                .map(|(_, count)| count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn due_jobs_dispatches_a_scheduled_metadata_refresh() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        let transcode = crate::test_tempdir().expect("transcode");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        store
            .set_library_schedule(library.id, 0, 1)
            .await
            .expect("schedule refresh");
        let jobs = manager(store.clone(), artwork.path());
        let transcode = Arc::new(TranscodeManager::new(
            store.clone(),
            transcode.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        jobs.run_due_jobs(&transcode).await.expect("scheduler tick");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if jobs
                    .all_statuses()
                    .await
                    .get(&library.id)
                    .is_some_and(|status| !status.running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduled refresh finished");

        let refreshed = store
            .get_library(library.id)
            .await
            .expect("read library")
            .expect("library remains");
        assert!(refreshed.last_refresh_at.is_some());
        let status = jobs
            .all_statuses()
            .await
            .remove(&library.id)
            .expect("scheduled refresh status");
        assert!(status.last_scan.is_some());
        assert!(status.error.is_none());
        let (counts, _) = jobs.metrics().snapshot();
        assert_eq!(
            counts
                .into_iter()
                .find(|(trigger, _)| *trigger == "scheduled")
                .map(|(_, count)| count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn refreshing_missing_item_artwork_is_an_empty_success() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(store, artwork.path());

        let outcome = jobs
            .refresh_item_artwork(i64::MAX)
            .await
            .expect("missing item is not a store failure");
        assert!(outcome.enrich.is_none());
        assert!(outcome.local_art.is_none());
    }

    /// The bug this whole change exists for, in one test.
    ///
    /// monarr POSTs `/api/v1/scan` the moment an import finishes and waits for
    /// the answer. Before this, the handler placed the row and stopped — no
    /// enrichment, no artwork — and the item sat with a blank card until
    /// somebody pressed Scan on the whole library. A home library is the
    /// provider-free way to prove it: its enrichment adopts the picture
    /// already sitting next to the file, so the assertion is about *whether
    /// enrichment ran at all*, not about anyone's network.
    #[tokio::test]
    async fn a_targeted_scan_enriches_what_it_placed() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        std::fs::create_dir_all(media.path().join("Holiday")).expect("mkdir");
        std::fs::write(media.path().join("Holiday/clip.mp4"), b"not really video").expect("clip");
        std::fs::write(media.path().join("Holiday/clip-thumb.jpg"), b"jpeg-ish").expect("thumb");

        let lib = store
            .create_library(&NewLibrary {
                name: "Home".into(),
                kind: LibraryKind::Home,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("lib");

        let jobs = manager(store.clone(), artwork.path());
        let scan = jobs
            .request_scan(ScanRequest {
                id: "req-1".into(),
                library_id: lib.id,
                path: media.path().join("Holiday"),
                ids: None,
                book: None,
                correlation_id: None,
                source: Some("monarr".into()),
            })
            .await
            .expect("scan ran")
            .expect("not queued");
        assert_eq!(scan.items.len(), 1, "the clip was placed");

        let clip = store
            .get_item(scan.items[0].item_id)
            .await
            .expect("get")
            .expect("item");
        assert!(
            clip.poster_path.is_some(),
            "a peer-ingested item must come out of the targeted scan with \
             artwork; before the fix this was None and stayed None"
        );

        // And the folder above it inherited that poster — which only happens
        // if the ancestors were enriched too, not just the placed row.
        let folder = store
            .get_item(clip.parent_id.expect("parent"))
            .await
            .expect("get")
            .expect("folder");
        assert_eq!(folder.kind, ItemKind::Folder);
        assert!(folder.poster_path.is_some());
    }

    /// A show can be old while the episode is brand new. The show row is the
    /// gateway to TMDB's season endpoint, so treating its earlier enrichment
    /// stamp as a reason to skip it strands the new season and episode with
    /// blank cards. This is the shape seen in the live library: movies from
    /// the same import window had posters, episodes under known shows did not.
    #[tokio::test]
    async fn a_targeted_scan_enriches_new_children_of_an_existing_show() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let artwork = crate::test_tempdir().expect("artwork");
        let show = media.path().join("Severance (2022)");
        let season_one = show.join("Season 01");
        std::fs::create_dir_all(&season_one).expect("mkdir s1");
        std::fs::write(season_one.join("Severance.S01E01.mkv"), b"video").expect("episode 1");

        let lib = store
            .create_library(&NewLibrary {
                name: "TV".into(),
                kind: LibraryKind::Shows,
                paths: vec![media.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("lib");
        store
            .put_setting(keys::TMDB_API_KEY, "test-key")
            .await
            .expect("key");

        let show_hits = Arc::new(AtomicUsize::new(0));
        let season_one_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(targeted_show_tmdb(
            Arc::clone(&show_hits),
            Arc::clone(&season_one_hits),
            Some("/season-1.jpg"),
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        let ids = Some(IdHints {
            series_tmdb: Some(42),
            episodeish: true,
            ..Default::default()
        });

        let first = jobs
            .request_scan(ScanRequest {
                id: "s1".into(),
                library_id: lib.id,
                path: season_one,
                ids: ids.clone(),
                book: None,
                correlation_id: None,
                source: Some("monarr".into()),
            })
            .await
            .expect("first scan")
            .expect("first ran");
        let first_episode = store
            .get_item(first.items[0].item_id)
            .await
            .expect("get first")
            .expect("first episode");
        assert!(first_episode.poster_path.is_some(), "initial episode art");
        let first_season = store
            .get_item(first_episode.parent_id.expect("first season"))
            .await
            .expect("get first season")
            .expect("first season");
        let first_show = store
            .get_item(first_season.parent_id.expect("first show"))
            .await
            .expect("get first show")
            .expect("first show");
        assert!(first_show.poster_path.is_some(), "initial show poster");
        assert!(first_show.backdrop_path.is_some(), "initial show backdrop");

        // The show is now metadata-stamped. A later notification for a new
        // season must still walk through it to hydrate the newly placed rows.
        let season_two = show.join("Season 02");
        std::fs::create_dir_all(&season_two).expect("mkdir s2");
        std::fs::write(season_two.join("Severance.S02E01.mkv"), b"video").expect("episode 2");
        let second = jobs
            .request_scan(ScanRequest {
                id: "s2".into(),
                library_id: lib.id,
                path: season_two,
                ids,
                book: None,
                correlation_id: None,
                source: Some("monarr".into()),
            })
            .await
            .expect("second scan")
            .expect("second ran");
        let second_episode = store
            .get_item(second.items[0].item_id)
            .await
            .expect("get second")
            .expect("second episode");
        assert!(
            second_episode.poster_path.is_some(),
            "a new episode under an existing show must leave the notification path with artwork"
        );
        let second_season = store
            .get_item(second_episode.parent_id.expect("season"))
            .await
            .expect("get season")
            .expect("second season");
        assert!(second_season.poster_path.is_some(), "new season poster");
        assert_eq!(
            season_one_hits.load(Ordering::SeqCst),
            1,
            "the targeted retry must not re-download every existing season"
        );
        assert_eq!(
            show_hits.load(Ordering::SeqCst),
            1,
            "the second import routes through the known show id without re-fetching the show"
        );
    }

    /// `scan_path` hands back the rows that own the *files* — episodes, not
    /// shows. Enriching those ids alone would enrich nothing, because a show
    /// is what the provider is asked about.
    #[tokio::test]
    async fn enrich_targets_walks_up_to_the_show() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let lib = store
            .create_library(&NewLibrary {
                name: "Shows".into(),
                kind: LibraryKind::Shows,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("lib");
        let new = |kind: ItemKind, parent: Option<i64>, title: &str| {
            let store = store.clone();
            let item = NewItem {
                library_id: lib.id,
                kind,
                parent_id: parent,
                title: title.into(),
                year: None,
                season_number: None,
                episode_number: None,
            };
            async move { store.insert_item(&item).await.expect("insert") }
        };
        let show = new(ItemKind::Show, None, "Severance").await;
        let season = new(ItemKind::Season, Some(show), "S1").await;
        let episode = new(ItemKind::Episode, Some(season), "E1").await;

        let jobs = manager(store.clone(), artwork.path());
        let targets = jobs.enrich_targets(&[episode]).await;
        assert!(targets.contains(&show), "the identity lives on the show");
        assert!(targets.contains(&season));
        assert!(targets.contains(&episode));

        // Two episodes of the same show contribute the show once, not twice —
        // a season import must not enrich the show ten times over.
        let episode2 = new(ItemKind::Episode, Some(season), "E2").await;
        let targets = jobs.enrich_targets(&[episode, episode2]).await;
        assert_eq!(targets.iter().filter(|id| **id == show).count(), 1);

        // A full 16-step walk that lands on a row seen by a later start still
        // has not proved that row's parent was visited. The later start must
        // continue walking rather than treating output deduplication as a
        // traversal cutoff.
        let mut chain = vec![show];
        for depth in 1..=16 {
            let parent = *chain.last().expect("parent");
            chain.push(new(ItemKind::Folder, Some(parent), &format!("F{depth}")).await);
        }
        let deepest = *chain.last().expect("deepest");
        let first_ancestor = chain[1];
        let targets = jobs.enrich_targets(&[deepest, first_ancestor]).await;
        assert!(
            targets.contains(&show),
            "a later start must finish a depth-capped ancestor walk"
        );
    }

    /// The sweep with nothing to sweep must not invent work — and must not
    /// need a provider key to say so.
    #[tokio::test]
    async fn the_artwork_sweep_is_a_no_op_on_a_healthy_library() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let jobs = manager(store.clone(), artwork.path());
        assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);
    }

    /// The daemon owns the bound, not merely the store query. An inherited
    /// backlog larger than one batch advances by exactly that batch and leaves
    /// the tail due for the next scheduler tick.
    #[tokio::test]
    async fn the_artwork_sweep_claims_exactly_one_bounded_batch() {
        const EXTRA: usize = 7;
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let episodes = seeded_episode_backlog(&store, ARTWORK_RETRY_BATCH as usize + EXTRA).await;
        let season_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(empty_season_tmdb(Arc::clone(&season_hits))).await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);

        assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);
        let mut stamped = 0;
        for id in episodes {
            if store
                .get_item(id)
                .await
                .expect("get episode")
                .expect("episode")
                .artwork_attempted_at
                .is_some()
            {
                stamped += 1;
            }
        }
        assert_eq!(stamped, ARTWORK_RETRY_BATCH as usize);
        let due = store
            .items_missing_artwork(
                None,
                keys::ARTWORK_RETRY_BACKOFF_SECS,
                ARTWORK_RETRY_BATCH + EXTRA as i64,
            )
            .await
            .expect("remaining due");
        assert_eq!(
            due.len(),
            EXTRA,
            "the unclaimed tail remains immediately due"
        );
        assert_eq!(season_hits.load(Ordering::SeqCst), 1);
    }

    /// A zero backoff is an adversarial but supported setting for the store
    /// query. Never-attempted rows must still all get one turn before an old
    /// row is reclaimed from the head of a larger-than-batch backlog.
    #[tokio::test]
    async fn artwork_retry_fairness_drains_fresh_rows_before_reclaiming_any() {
        const EXTRA: usize = 7;
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let episodes = seeded_episode_backlog(&store, ARTWORK_RETRY_BATCH as usize + EXTRA).await;
        let season_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(empty_season_tmdb(Arc::clone(&season_hits))).await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);

        let first = jobs
            .sweep_artwork_with_backoff(0)
            .await
            .expect("first pass");
        let second = jobs
            .sweep_artwork_with_backoff(0)
            .await
            .expect("second pass");
        assert_eq!(first.claimed_ids.len(), ARTWORK_RETRY_BATCH as usize);
        assert_eq!(second.claimed_ids.len(), EXTRA);
        let first_ids: HashSet<i64> = first.claimed_ids.into_iter().collect();
        assert!(
            second.claimed_ids.iter().all(|id| !first_ids.contains(id)),
            "no row is reclaimed before every never-attempted row is claimed"
        );
        let mut all = first_ids;
        all.extend(second.claimed_ids);
        assert_eq!(all.len(), episodes.len());
        assert_eq!(season_hits.load(Ordering::SeqCst), 2);
    }

    /// A missing key or a transient provider failure made no artwork attempt.
    /// Neither may be laundered into the 24-hour "provider has no image"
    /// backoff; the normal half-hour scheduler should get another chance.
    #[tokio::test]
    async fn artwork_retry_does_not_back_off_work_that_never_reached_a_provider_result() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let episode = seeded_episode_backlog(&store, 1).await[0];
        store
            .put_setting(keys::TMDB_API_KEY, "")
            .await
            .expect("remove key");
        let jobs = manager(store.clone(), artwork.path());
        assert_eq!(jobs.sweep_artwork().await.expect("missing-key pass"), 0);
        assert!(
            store
                .get_item(episode)
                .await
                .expect("get")
                .expect("episode")
                .artwork_attempted_at
                .is_none(),
            "skipping for a missing key is not an artwork attempt"
        );

        store
            .put_setting(keys::TMDB_API_KEY, "test-key")
            .await
            .expect("restore key");
        use axum::http::StatusCode;
        use axum::routing::get;
        let base = serve(axum::Router::new().route(
            "/tv/42/season/1",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        assert_eq!(jobs.sweep_artwork().await.expect("transient pass"), 0);
        let episode = store
            .get_item(episode)
            .await
            .expect("get")
            .expect("episode");
        assert!(episode.artwork_attempted_at.is_none());
        assert!(episode.artwork_error.is_none());
        assert_eq!(
            store
                .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
                .await
                .expect("still due")
                .len(),
            1,
            "a transient failure remains eligible for the next scheduler pass"
        );
    }

    /// The spawned entry point itself is single-flight. A second tick while a
    /// provider request is blocked must return without issuing another pass.
    #[tokio::test]
    async fn artwork_retry_pass_is_single_flight() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        seeded_episode_backlog(&store, 1).await;
        let hits = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(blocking_season_tmdb(
            Arc::clone(&hits),
            Arc::clone(&entered),
            Arc::clone(&release),
        ))
        .await;
        let jobs = manager_with_tmdb(store, artwork.path(), &base);

        let first = tokio::spawn(Arc::clone(&jobs).artwork_retry_pass());
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("first pass reached provider");
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Arc::clone(&jobs).artwork_retry_pass(),
        )
        .await
        .expect("second pass returned");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        release.notify_waiters();
        first.await.expect("first pass task");
        assert!(!jobs.retrying_artwork.load(Ordering::Relaxed));
    }

    /// Dispatching a slow artwork retry must not hold the scheduler loop. The
    /// cleanup job comes after artwork in `due_jobs`, so its persisted stamp is
    /// direct evidence that the tick continued while TMDB was still blocked.
    #[tokio::test]
    async fn due_jobs_continue_while_artwork_retry_is_in_flight() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let transcode_dir = crate::test_tempdir().expect("transcode");
        seeded_episode_backlog(&store, 1).await;
        store
            .put_setting(keys::JOB_TRANSCODE_CLEANUP_MINS, "15")
            .await
            .expect("schedule cleanup");
        let hits = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(blocking_season_tmdb(
            Arc::clone(&hits),
            Arc::clone(&entered),
            Arc::clone(&release),
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        let transcode = Arc::new(TranscodeManager::new(
            store.clone(),
            transcode_dir.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            jobs.run_due_jobs(&transcode),
        )
        .await
        .expect("scheduler returned while artwork was blocked")
        .expect("scheduler tick");
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("artwork reached provider");
        let cleanup_key = jobs.local_job_key(keys::JOB_LAST_TRANSCODE_CLEANUP);
        assert!(
            store
                .get_setting(&cleanup_key)
                .await
                .expect("cleanup stamp")
                .is_some(),
            "a job ordered after artwork completed in the same tick"
        );
        assert!(
            store
                .get_setting(keys::JOB_LAST_TRANSCODE_CLEANUP)
                .await
                .expect("legacy global cleanup stamp")
                .is_none(),
            "node-local cleanup must not recreate the cluster-wide clock"
        );
        let other_store: Arc<dyn Store> = store.clone();
        let other = JobManager::new_with_scan_prune_percent(
            other_store,
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            "other-cleanup-node".to_owned(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        );
        let other_key = other.local_job_key(keys::JOB_LAST_TRANSCODE_CLEANUP);
        assert_ne!(cleanup_key, other_key);
        assert_eq!(other.job_stamp(&other_key).await, None);
        other.stamp_local(keys::JOB_LAST_TRANSCODE_CLEANUP).await;
        assert!(jobs.job_stamp(&cleanup_key).await.is_some());
        assert!(other.job_stamp(&other_key).await.is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        release.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while jobs.retrying_artwork.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("artwork pass finished");
    }

    #[tokio::test]
    async fn scheduled_telemetry_prune_is_aged_bounded_and_stamped() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let now_secs = now();
        for (name, at) in [
            ("expired", (now_secs - 31 * 24 * 60 * 60) * 1_000),
            ("kept", (now_secs - 29 * 24 * 60 * 60) * 1_000),
        ] {
            store
                .record_playback_event(&PlaybackEvent {
                    at_unix_ms: at,
                    event: name.to_owned(),
                    ..PlaybackEvent::default()
                })
                .await
                .expect("seed telemetry");
        }
        let artwork = crate::test_tempdir().expect("artwork");
        let transcode_dir = crate::test_tempdir().expect("transcode");
        let jobs = manager(store.clone(), artwork.path());
        let transcode = Arc::new(TranscodeManager::new(
            store.clone(),
            transcode_dir.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        jobs.run_due_jobs(&transcode).await.expect("scheduler tick");
        let events = store
            .playback_events(&PlaybackEventQuery {
                limit: 10,
                ..PlaybackEventQuery::default()
            })
            .await
            .expect("remaining telemetry");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "kept");
        assert!(
            store
                .get_setting(&jobs.local_job_key(keys::JOB_LAST_TELEMETRY_PRUNE))
                .await
                .expect("prune stamp")
                .is_some(),
            "the scheduler persists the prune clock"
        );
    }

    /// Production shape: the known show has a poster and an enrichment stamp,
    /// while later-imported children have neither artwork nor an attempt
    /// stamp. One valid episode is repaired; one local episode TMDB does not
    /// list is stamped and backed off. The show is only a route and stays
    /// byte-for-byte untouched.
    #[tokio::test]
    async fn the_artwork_sweep_repairs_and_converges_blank_tv_children() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let (library_id, show, season) = seeded_enriched_show(&store).await;
        let episode = store
            .insert_item(&NewItem {
                library_id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: "Episode 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(1),
            })
            .await
            .expect("episode");
        let ghost = store
            .insert_item(&NewItem {
                library_id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: "Episode 99".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(99),
            })
            .await
            .expect("ghost episode");

        let show_hits = Arc::new(AtomicUsize::new(0));
        let season_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(targeted_show_tmdb(
            Arc::clone(&show_hits),
            Arc::clone(&season_hits),
            Some("/season-1.jpg"),
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        assert_eq!(
            jobs.sweep_artwork().await.expect("sweep"),
            2,
            "the return value counts repaired candidate rows, not the routing show"
        );

        let show = store.get_item(show).await.expect("get show").expect("show");
        let season = store
            .get_item(season)
            .await
            .expect("get season")
            .expect("season");
        let episode = store
            .get_item(episode)
            .await
            .expect("get episode")
            .expect("episode");
        let ghost = store
            .get_item(ghost)
            .await
            .expect("get ghost")
            .expect("ghost");
        assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
        assert!(season.poster_path.is_some(), "season card repaired");
        assert!(episode.poster_path.is_some(), "episode card repaired");
        assert!(ghost.poster_path.is_none(), "TMDB has no episode 99");
        assert!(
            ghost.artwork_attempted_at.is_some(),
            "an unmatched child is stamped so the daily backoff can engage"
        );
        assert!(store
            .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
            .await
            .expect("due after sweep")
            .is_empty());
        assert_eq!(jobs.sweep_artwork().await.expect("second sweep"), 0);
        assert_eq!(show_hits.load(Ordering::SeqCst), 0);
        assert_eq!(
            season_hits.load(Ordering::SeqCst),
            1,
            "a converged child cannot pin its show to a half-hour retry loop"
        );
    }

    /// A blank season is independently repairable even when every episode
    /// still is healthy. This is the empty-bucket path: no episode should be
    /// downloaded merely to make the season endpoint run.
    #[tokio::test]
    async fn the_artwork_sweep_repairs_a_blank_season_without_touching_healthy_rows() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let (library_id, show, season) = seeded_enriched_show(&store).await;
        let episode = store
            .insert_item(&NewItem {
                library_id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: "Episode 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(1),
            })
            .await
            .expect("episode");
        store
            .apply_metadata(
                episode,
                &MetadataPatch {
                    poster_path: Some("healthy-episode.jpg".into()),
                    artwork: Some(ArtworkAttempt::Stored),
                    ..Default::default()
                },
            )
            .await
            .expect("healthy episode");

        let show_hits = Arc::new(AtomicUsize::new(0));
        let season_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(targeted_show_tmdb(
            Arc::clone(&show_hits),
            Arc::clone(&season_hits),
            Some("/season-1.jpg"),
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 1);

        let show = store.get_item(show).await.expect("show").expect("show");
        let season = store
            .get_item(season)
            .await
            .expect("season")
            .expect("season");
        let episode = store
            .get_item(episode)
            .await
            .expect("episode")
            .expect("episode");
        assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
        assert!(season.poster_path.is_some());
        assert_eq!(episode.poster_path.as_deref(), Some("healthy-episode.jpg"));
        assert_eq!(show_hits.load(Ordering::SeqCst), 0);
        assert_eq!(season_hits.load(Ordering::SeqCst), 1);
    }

    /// TMDB can legitimately have no poster for a season. That is an attempt,
    /// not silence: the row stays blank but leaves the immediate due set.
    #[tokio::test]
    async fn the_artwork_sweep_stamps_a_season_when_tmdb_has_no_poster() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let artwork = crate::test_tempdir().expect("artwork");
        let (_, show, season) = seeded_enriched_show(&store).await;
        let show_hits = Arc::new(AtomicUsize::new(0));
        let season_hits = Arc::new(AtomicUsize::new(0));
        let base = serve(targeted_show_tmdb(
            Arc::clone(&show_hits),
            Arc::clone(&season_hits),
            None,
        ))
        .await;
        let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
        assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);

        let show = store.get_item(show).await.expect("show").expect("show");
        let season = store
            .get_item(season)
            .await
            .expect("season")
            .expect("season");
        assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
        assert!(season.poster_path.is_none());
        assert!(season.artwork_attempted_at.is_some());
        assert!(store
            .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
            .await
            .expect("due after unavailable poster")
            .is_empty());
        assert_eq!(show_hits.load(Ordering::SeqCst), 0);
        assert_eq!(season_hits.load(Ordering::SeqCst), 1);
    }
}
