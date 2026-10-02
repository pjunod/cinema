//! Private media-capability directory and diagnostics-only placement offers.
//!
//! P4 observes and ranks; it deliberately does not forward or start playback.
//! Snapshot polling never touches source paths, and offer fan-out only asks a
//! node to inspect replicated facts, recent readability, and its local cache.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ActivityPeer, MembershipError, MembershipManager};
use plurx_core::domain::MediaFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::http::peer_transport::{deadline_after, PeerAuthMode, PeerTransport};
use crate::state::AppState;

pub(crate) const SNAPSHOT_PATH: &str = "/internal/v1/media/snapshot";
pub(crate) const QUALITY_CANDIDATES_V2_PATH: &str = "/internal/v2/media/quality-candidates";
pub(crate) const QUALITY_CANDIDATES_PATH: &str = "/internal/v1/media/quality-candidates";
pub(crate) const QUALITY_CATALOG_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const OFFERS_PATH: &str = "/internal/v1/media/offers";
/// Protocol 8 preserves canonical capability and planning bindings at dispatch,
/// in addition to pre-filter HEVC proof and source-fenced copy VOD. Exact-version
/// placement excludes strict older workers. Old ingress and sessions must be
/// drained on rollout.
pub(crate) const PROTOCOL_VERSION: i64 = 8;
pub(crate) const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const SNAPSHOT_EXPIRY: Duration = Duration::from_secs(15);
pub(crate) const OFFER_DEADLINE: Duration = Duration::from_millis(200);
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_OFFER_BYTES: usize = 64 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_PEERS: usize = 64;
const MAX_CAPABILITIES: usize = 16;
const MAX_CAPABILITY_BYTES: usize = 64;
const MAX_TRACK_INDEX: i64 = 1_024;
const MAX_START_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const ROOT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const ROOT_READABILITY_TTL: Duration = Duration::from_secs(45);
const ROOT_PROBE_COLLECTION_DEADLINE: Duration = Duration::from_secs(2);
const MAX_LIBRARY_ROOTS: usize = 64;
// One complete timed-out predecessor generation cannot prevent the current
// configured generation from being observed. The process ceiling remains
// hard; exhaustion fails source evidence closed.
const MAX_ROOT_PROBE_CHILDREN: usize = MAX_LIBRARY_ROOTS * 2;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PressureBand {
    #[default]
    Idle,
    Moderate,
    High,
    Unavailable,
}

impl PressureBand {
    fn preference(self) -> u8 {
        match self {
            Self::Unavailable => 0,
            Self::High => 1,
            Self::Moderate => 2,
            Self::Idle => 3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EncoderOfferCapability {
    pub family: String,
    pub max_target_height: i64,
    pub decoders: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToneMapOfferCapability {
    pub pipeline: String,
}

/// Recent observations, never capacity certificates. No sample is unknown,
/// and each fixed 30-second window expires without background probing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MediaIoObservation {
    pub storage_read_micros: Option<u64>,
    pub delivered_bytes_per_second: Option<u64>,
    pub peer_bytes_per_second: Option<u64>,
}
struct IoWindow {
    started: std::time::Instant,
    reads: u64,
    read_micros: u64,
    delivered: u64,
    peer: u64,
}
impl Default for IoWindow {
    fn default() -> Self {
        Self {
            started: std::time::Instant::now(),
            reads: 0,
            read_micros: 0,
            delivered: 0,
            peer: 0,
        }
    }
}
static MEDIA_IO: std::sync::LazyLock<Mutex<IoWindow>> =
    std::sync::LazyLock::new(|| Mutex::new(IoWindow::default()));
pub(crate) fn observe_media_io(delivered: u64, peer: u64, storage_read: Option<Duration>) {
    let mut window = MEDIA_IO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if window.started.elapsed() >= Duration::from_secs(30) {
        *window = IoWindow::default();
    }
    window.delivered = window.delivered.saturating_add(delivered);
    window.peer = window.peer.saturating_add(peer);
    if let Some(elapsed) = storage_read {
        window.reads = window.reads.saturating_add(1);
        window.read_micros = window
            .read_micros
            .saturating_add(elapsed.as_micros().min(u64::MAX as u128) as u64);
    }
}
fn media_io_observation() -> MediaIoObservation {
    let window = MEDIA_IO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let elapsed = window.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    if elapsed >= 30_000 {
        return MediaIoObservation::default();
    }
    let rate = |bytes: u64| {
        (elapsed >= 1500 && bytes > 0).then(|| bytes.saturating_mul(1000) / elapsed.max(1))
    };
    MediaIoObservation {
        storage_read_micros: (window.reads > 0).then(|| window.read_micros / window.reads.max(1)),
        delivered_bytes_per_second: rate(window.delivered),
        peer_bytes_per_second: rate(window.peer),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MediaNodeSnapshot {
    pub node_id: String,
    pub observed_at_unix_ms: i64,
    pub build: String,
    pub protocol_version: i64,
    pub encoders: Vec<EncoderOfferCapability>,
    pub tone_map: Vec<ToneMapOfferCapability>,
    pub hardware_slots_used: u32,
    pub hardware_slots_max: u32,
    pub software_threads_used: u32,
    pub software_threads_max: u32,
    pub scratch_bytes_free: u64,
    pub scratch_pressure: PressureBand,
    pub source_io_pressure: PressureBand,
    pub egress_pressure: PressureBand,
    pub live_waiting: bool,
    pub background_active: bool,
    #[serde(default)]
    pub io: MediaIoObservation,
    #[serde(default)]
    pub live_tv_processing: bool,
    /// Durable resource admission; distinct from the removed protocol-4 relay.
    #[serde(default)]
    pub live_tv_resource_processing: bool,
}

/// Separate additive protocol: old workers return 404 rather than parsing a
/// changed legacy placement envelope. Catalog inspection never starts media.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityCatalogRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_contract: Option<(bool, bool, bool)>,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub caps: plurx_core::playback::DeviceCaps,
    pub audio_index: Option<i64>,
    pub audio_offset_ms: i64,
    pub subtitle_burn: Option<i64>,
    pub presentation: crate::transcode::Presentation,
}

impl QualityCatalogRequest {
    pub(crate) fn validate(&self) -> Result<(), CatalogValidationError> {
        let failure = |clause: &str, observed, limit| CatalogValidationError {
            clause: clause.to_owned(),
            observed,
            limit,
        };
        if self
            .copy_contract
            .is_some_and(|(_, preserve, convert)| convert && !preserve)
        {
            return Err(failure("copy_contract", 1, 0));
        }
        if self.file_id <= 0 || self.source_size < 0 || self.source_mtime < 0 {
            return Err(failure("source_identity", 0, 1));
        }
        if self.caps.v != plurx_core::playback::DeviceCaps::VERSION {
            return Err(failure("caps_version", usize::from(self.caps.v), 2));
        }
        if self.caps.video.len() > plurx_core::playback::MAX_CLIENT_DECODER_ENTRIES {
            return Err(failure(
                "video_entries",
                self.caps.video.len(),
                plurx_core::playback::MAX_CLIENT_DECODER_ENTRIES,
            ));
        }
        if self.caps.validate_audio_sinks().is_err() {
            return Err(failure("audio_sinks", self.caps.audio_sinks.len(), 0));
        }
        if self
            .caps
            .validate_progressive_hevc_sample_entries()
            .is_err()
        {
            return Err(failure("progressive_hevc_sample_entries", 1, 0));
        }
        if !(-15_000..=15_000).contains(&self.audio_offset_ms) {
            return Err(failure(
                "audio_offset",
                self.audio_offset_ms.unsigned_abs() as usize,
                15_000,
            ));
        }
        if [self.audio_index, self.subtitle_burn]
            .into_iter()
            .flatten()
            .any(|index| !(0..=MAX_TRACK_INDEX).contains(&index))
        {
            return Err(failure("track_index", 1, MAX_TRACK_INDEX as usize));
        }
        Ok(())
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerQualityCandidate {
    pub node_id: String,
    pub candidate: plurx_core::playback::candidate::QualityCandidate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<PlanningBinding>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dispatch_supported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CatalogValidationError {
    pub clause: String,
    pub observed: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum CatalogCause {
    RequestInvalid(CatalogValidationError),
    LocalDeadline,
    PeerDeadline,
    PeerBusy,
    Transport,
    Maintenance,
    StaleSource,
    AuthorityRefused,
    SnapshotUnavailable,
    PeerProtocol,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanningBinding {
    pub generation: i64,
    pub source_digest: String,
}
impl PlanningBinding {
    pub(crate) fn from_snapshot(snapshot: &plurx_core::store::PlaybackPlanningSnapshot) -> Self {
        use sha2::{Digest, Sha256};
        let encoded = serde_json::to_vec(&(&snapshot.file, &snapshot.probe_json))
            .expect("source snapshot serializes");
        Self {
            generation: snapshot.generation,
            source_digest: hex::encode(Sha256::digest(encoded)),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetedCatalogRequest {
    pub expected_binding: Option<PlanningBinding>,
    pub budget_ms: u32,
    pub request: QualityCatalogRequest,
}

#[derive(Clone)]
pub(crate) struct CreateStartupBudget {
    pub deadline: tokio::time::Instant,
    calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    binding: std::sync::Arc<std::sync::Mutex<Option<PlanningBinding>>>,
}
impl CreateStartupBudget {
    pub(crate) fn new(remaining_ms: u64) -> Self {
        Self {
            deadline: tokio::time::Instant::now() + Duration::from_millis(remaining_ms.min(10_000)),
            calls: Default::default(),
            binding: Default::default(),
        }
    }
    pub(crate) fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) async fn scope<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        CREATE_STARTUP_BUDGET.scope(self.clone(), future).await
    }
}
pub(crate) fn create_stage_deadline(maximum: Duration) -> tokio::time::Instant {
    let deadline = deadline_after(maximum);
    CREATE_STARTUP_BUDGET
        .try_with(|budget| deadline.min(budget.deadline - Duration::from_millis(250)))
        .unwrap_or(deadline)
}

pub(crate) fn capture_create_planning_binding(
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
) {
    let _ = CREATE_STARTUP_BUDGET.try_with(|budget| {
        *budget
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(PlanningBinding::from_snapshot(snapshot));
    });
}

tokio::task_local! { static CREATE_STARTUP_BUDGET: CreateStartupBudget; }

/// Completion and bounded causes survive aggregation; empty rows alone are
/// never proof that discovery completed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityCatalogResult {
    pub candidates: Vec<WorkerQualityCandidate>,
    pub authority_refused: bool,
    pub complete: bool,
    pub causes: Vec<CatalogCause>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MediaOfferRequest {
    pub protocol_version: i64,
    pub request_fingerprint: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub decoder: String,
    pub target_height: i64,
    pub start_millis: i64,
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    pub hdr10: bool,
    pub output_contract: String,
    pub scratch_bytes_required: u64,
}

impl MediaOfferRequest {
    pub(crate) fn new(
        file: &MediaFile,
        target_height: i64,
        start_millis: i64,
        audio_index: Option<i64>,
        subtitle_index: Option<i64>,
        hdr10: bool,
    ) -> Result<Self, &'static str> {
        let decoder = decoder_contract(file).ok_or("unsupported_decoder")?;
        let scratch_bytes_required = scratch_estimate(file, target_height);
        let mut request = Self {
            protocol_version: PROTOCOL_VERSION,
            request_fingerprint: String::new(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            decoder: decoder.to_owned(),
            target_height,
            start_millis,
            audio_index,
            subtitle_index,
            hdr10,
            output_contract: "hls-mpegts-v1".to_owned(),
            scratch_bytes_required,
        };
        request.request_fingerprint = request.fingerprint();
        request
            .is_valid()
            .then_some(request)
            .ok_or("invalid_request")
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
            && self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
            && (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT)
                .contains(&self.target_height)
            && (0..=MAX_START_MILLIS).contains(&self.start_millis)
            && self
                .audio_index
                .is_none_or(|index| (0..=MAX_TRACK_INDEX).contains(&index))
            && self
                .subtitle_index
                .is_none_or(|index| (0..=MAX_TRACK_INDEX).contains(&index))
            && self.decoder.len() <= MAX_CAPABILITY_BYTES
            && !self.decoder.is_empty()
            && self.output_contract == "hls-mpegts-v1"
            && self.scratch_bytes_required > 0
            && self.request_fingerprint.len() == 64
            && self.request_fingerprint == self.fingerprint()
    }

    fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for value in [
            self.protocol_version.to_string(),
            self.file_id.to_string(),
            self.source_size.to_string(),
            self.source_mtime.to_string(),
            self.decoder.clone(),
            self.target_height.to_string(),
            self.start_millis.to_string(),
            self.audio_index
                .map_or_else(String::new, |value| value.to_string()),
            self.subtitle_index
                .map_or_else(String::new, |value| value.to_string()),
            u8::from(self.hdr10).to_string(),
            self.output_contract.clone(),
            self.scratch_bytes_required.to_string(),
        ] {
            hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaDeliveryMethod {
    Transcode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct MediaOffer {
    pub node_id: String,
    pub request_fingerprint: String,
    pub eligible: bool,
    pub refusal_code: Option<String>,
    pub cache_hit: bool,
    pub source_likely_readable: bool,
    pub scratch_bytes_required: u64,
    pub scratch_bytes_free: u64,
    pub source_io_pressure: PressureBand,
    pub egress_pressure: PressureBand,
    pub method: MediaDeliveryMethod,
    pub encoder: String,
    pub pipeline: String,
    pub free_hardware_slots: u32,
    pub free_software_threads: u32,
    pub recent_speed: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct PlacementDiagnostics {
    pub request_fingerprint: String,
    pub budget_ms: u64,
    pub selected_node_id: Option<String>,
    pub offers: Vec<MediaOffer>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MediaDirectoryDiagnostics {
    pub protocol_version: i64,
    pub remote_placement_enabled: bool,
    pub remote_placement_rollout_ready: bool,
    pub remote_placement_ready: bool,
    pub session_takeover_enabled: bool,
    pub session_takeover_ready: bool,
    pub local_active_sessions: usize,
    pub snapshot_interval_seconds: u64,
    pub snapshot_expiry_seconds: u64,
    pub nodes: Vec<MediaNodeSnapshot>,
}

#[derive(Clone, Debug)]
struct CachedSnapshot {
    snapshot: MediaNodeSnapshot,
    expires_at: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug)]
struct RootReadability {
    readable: bool,
    observed_at: tokio::time::Instant,
}

struct RootProbe {
    child: tokio::process::Child,
    kill_sent: bool,
    /// Keeps the probe listed with its priority class until it is reaped.
    _job: Option<crate::process_control::ChildJob>,
}

#[derive(Default)]
struct RootProbeRegistry {
    running: BTreeMap<PathBuf, RootProbe>,
    completed: Vec<(PathBuf, RootReadability)>,
}

#[derive(Clone)]
pub(crate) struct MediaPool {
    membership: MembershipManager,
    transport: PeerTransport,
    snapshots: Arc<RwLock<BTreeMap<String, CachedSnapshot>>>,
    root_readability: Arc<RwLock<BTreeMap<PathBuf, RootReadability>>>,
    root_probes: Arc<Mutex<RootProbeRegistry>>,
    root_probe_launcher: Arc<tokio::sync::Semaphore>,
}

impl MediaPool {
    pub(crate) fn new(membership: MembershipManager) -> Arc<Self> {
        Arc::new(Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            snapshots: Arc::new(RwLock::new(BTreeMap::new())),
            root_readability: Arc::new(RwLock::new(BTreeMap::new())),
            root_probes: Arc::new(Mutex::new(RootProbeRegistry::default())),
            root_probe_launcher: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    pub(crate) async fn poll_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = self.poll_once().await {
                tracing::debug!(%error, "media snapshot poll unavailable");
            }
        }
    }

    /// Refresh root-scoped source facts independently of placement traffic.
    /// Offers only read this memory; they never wake every candidate NAS.
    pub(crate) async fn root_readability_loop(
        self: Arc<Self>,
        store: Arc<dyn plurx_core::store::Store>,
    ) {
        let mut interval = tokio::time::interval(ROOT_REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.refresh_root_readability(store.as_ref()).await;
        }
    }

    async fn refresh_root_readability(&self, store: &dyn plurx_core::store::Store) {
        let peers = match tokio::time::timeout(
            ROOT_PROBE_COLLECTION_DEADLINE,
            self.membership.media_peers(),
        )
        .await
        {
            Ok(Ok(peers)) => peers,
            Ok(Err(error)) => {
                tracing::debug!(%error, "media root probe membership unavailable");
                return;
            }
            Err(_) => {
                tracing::debug!("media root probe membership timed out");
                return;
            }
        };
        if !has_reachable_media_peer(&peers) {
            self.root_readability.write().await.clear();
            return;
        }
        let libraries = match tokio::time::timeout(
            ROOT_PROBE_COLLECTION_DEADLINE,
            store.list_libraries(),
        )
        .await
        {
            Ok(Ok(libraries)) => libraries,
            Ok(Err(error)) => {
                tracing::debug!(%error, "media root readability inventory unavailable");
                return;
            }
            Err(_) => {
                tracing::debug!("media root readability inventory timed out");
                return;
            }
        };
        let roots =
            bounded_command_safe_roots(libraries.into_iter().flat_map(|library| library.paths));
        self.root_readability
            .write()
            .await
            .retain(|root, _| roots.contains(root));
        // The common budget includes synchronous child launch. A slow or
        // saturated process table cannot extend source admission past the
        // same deadline merely by making spawn calls expensive.
        let deadline = deadline_after(ROOT_PROBE_COLLECTION_DEADLINE);
        let mut outcomes = self.reap_root_probes();
        self.start_root_probes(&roots, deadline).await;
        outcomes.extend(self.reap_root_probes_until(&roots, deadline));
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                // Fence every still-tracked observation before inspecting
                // child status. A successful exit noticed after the deadline
                // is therefore permanently negative.
                self.signal_timed_out_root_probes(&roots);
                break;
            }
            outcomes.extend(self.reap_root_probes_until(&roots, deadline));
            if !self.has_running_root_probe(&roots) {
                break;
            }
            tokio::time::sleep_until((now + Duration::from_millis(20)).min(deadline)).await;
        }
        // Mark deadline expiry before the final reap. A process that completed
        // after the deadline but before this observation can only publish a
        // failed fact, never a fresh positive one.
        self.signal_timed_out_root_probes(&roots);
        outcomes.extend(self.reap_root_probes());
        let mut readability = self.root_readability.write().await;
        for (root, fact) in outcomes {
            if roots.contains(&root) {
                readability.insert(root, fact);
            }
        }
    }

    async fn start_root_probes(&self, roots: &BTreeSet<PathBuf>, deadline: tokio::time::Instant) {
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        let Ok(permit) = Arc::clone(&self.root_probe_launcher).try_acquire_owned() else {
            // A previous process-table call is still outstanding. Do not
            // queue another blocking launcher behind it.
            return;
        };
        let roots = roots.clone();
        let registry = Arc::clone(&self.root_probes);
        let runtime = tokio::runtime::Handle::current();
        let launcher = tokio::task::spawn_blocking(move || {
            let _runtime = runtime.enter();
            for root in roots {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                let can_start = {
                    let registry = registry
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    !registry.running.contains_key(&root)
                        && root_probe_capacity_remaining(registry.running.len()) > 0
                };
                if !can_start {
                    continue;
                }
                match spawn_library_root_probe(&root) {
                    Ok((mut child, job)) => {
                        let mut registry = registry
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        // Check while holding the publication lock. Otherwise
                        // this thread can be preempted after an earlier clock
                        // sample and insert a positive-capable child after the
                        // refresher has already performed its final fence.
                        let late = tokio::time::Instant::now() >= deadline;
                        if late {
                            let _ = child.start_kill();
                        }
                        registry.running.insert(
                            root,
                            RootProbe {
                                child,
                                kill_sent: late,
                                _job: Some(job),
                            },
                        );
                    }
                    Err(error) => {
                        tracing::debug!(%error, path = %root.display(), "could not start media root probe");
                        registry
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .completed
                            .push((
                                root,
                                RootReadability {
                                    readable: false,
                                    observed_at: tokio::time::Instant::now(),
                                },
                            ));
                    }
                }
            }
            drop(permit);
        });
        // Dropping the join wait at the common deadline does not cancel a
        // possibly blocked OS spawn. Its owned permit remains held until it
        // returns, which preserves the hard single-flight guarantee.
        let _ = tokio::time::timeout_at(deadline, launcher).await;
    }

    fn reap_root_probes(&self) -> Vec<(PathBuf, RootReadability)> {
        self.reap_root_probes_with_deadline(None)
    }

    fn reap_root_probes_until(
        &self,
        roots: &BTreeSet<PathBuf>,
        deadline: tokio::time::Instant,
    ) -> Vec<(PathBuf, RootReadability)> {
        self.reap_root_probes_with_deadline(Some((roots, deadline)))
    }

    fn reap_root_probes_with_deadline(
        &self,
        deadline: Option<(&BTreeSet<PathBuf>, tokio::time::Instant)>,
    ) -> Vec<(PathBuf, RootReadability)> {
        let mut registry = self
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut outcomes = std::mem::take(&mut registry.completed);
        let completed = registry
            .running
            .iter_mut()
            .filter_map(|(root, probe)| {
                if deadline.is_some_and(|(roots, deadline)| {
                    roots.contains(root) && tokio::time::Instant::now() >= deadline
                }) && !probe.kill_sent
                {
                    let _ = probe.child.start_kill();
                    probe.kill_sent = true;
                }
                match probe.child.try_wait() {
                    Ok(Some(status)) => {
                        // The status read itself can cross the deadline or be
                        // preempted. Inspect the clock again before permitting
                        // a positive fact, rather than trusting the time at the
                        // beginning of a potentially 128-child sweep.
                        if deadline.is_some_and(|(roots, deadline)| {
                            roots.contains(root) && tokio::time::Instant::now() >= deadline
                        }) {
                            probe.kill_sent = true;
                        }
                        // Once the common deadline fired this observation is
                        // permanently failed, even if the process happened to
                        // finish successfully before the later reap. A delayed
                        // success must never acquire a fresh TTL.
                        Some((root.clone(), status.success() && !probe.kill_sent))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        tracing::debug!(%error, path = %root.display(), "could not reap media root probe");
                        if !probe.kill_sent {
                            let _ = probe.child.start_kill();
                            probe.kill_sent = true;
                        }
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        for (root, _) in &completed {
            registry.running.remove(root);
        }
        let observed_at = tokio::time::Instant::now();
        outcomes.extend(completed.into_iter().map(|(root, readable)| {
            (
                root,
                RootReadability {
                    readable,
                    observed_at,
                },
            )
        }));
        outcomes
    }

    fn has_running_root_probe(&self, roots: &BTreeSet<PathBuf>) -> bool {
        self.root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
            .iter()
            .any(|(root, probe)| roots.contains(root) && !probe.kill_sent)
    }

    fn signal_timed_out_root_probes(&self, roots: &BTreeSet<PathBuf>) {
        let mut registry = self
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (root, probe) in &mut registry.running {
            if roots.contains(root) && !probe.kill_sent {
                let _ = probe.child.start_kill();
                probe.kill_sent = true;
            }
        }
    }

    async fn root_likely_readable(&self, path: &Path) -> bool {
        let now = tokio::time::Instant::now();
        let path = command_safe_path(path);
        self.root_readability
            .read()
            .await
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .is_some_and(|(_, fact)| {
                fact.readable && now.duration_since(fact.observed_at) <= ROOT_READABILITY_TTL
            })
    }

    async fn poll_once(&self) -> Result<(), MembershipError> {
        let peers = self
            .membership
            .media_peers()
            .await?
            .into_iter()
            .filter(|peer| peer.reachable && peer.http_base.is_some())
            .take(MAX_PEERS)
            .collect::<Vec<_>>();
        if peers.is_empty() {
            self.expire().await;
            return Ok(());
        }
        let deadline = deadline_after(SNAPSHOT_DEADLINE);
        let transport = self.transport.clone();
        let outcomes = stream::iter(peers.into_iter().map(|peer| {
            let transport = transport.clone();
            async move {
                let node_id = peer.node_id.clone();
                let snapshot = fetch_snapshot(&transport, peer, deadline).await;
                (node_id, snapshot)
            }
        }))
        // Start every bounded peer immediately under the one shared deadline.
        // Eight blackholed peers must not prevent a healthy ninth from ever
        // getting a request.
        .buffer_unordered(snapshot_fanout_concurrency())
        .collect::<Vec<_>>()
        .await;
        let now = tokio::time::Instant::now();
        let mut snapshots = self.snapshots.write().await;
        snapshots.retain(|_, cached| now <= cached.expires_at);
        for (node_id, snapshot) in outcomes {
            if let Some(snapshot) = snapshot {
                snapshots.insert(node_id, snapshot);
            }
        }
        Ok(())
    }

    async fn expire(&self) {
        let now = tokio::time::Instant::now();
        self.snapshots
            .write()
            .await
            .retain(|_, cached| now <= cached.expires_at);
    }

    /// Observed capacity ranks candidates; the selected worker still reserves
    /// the actual delivery plan atomically before starting its encoder.
    pub(crate) async fn live_tv_worker(&self, state: &AppState) -> String {
        if !self.remote_placement_ready(state).await {
            return state.node_id.clone();
        }
        let voters = match self.membership.activity_peers().await {
            Ok(peers) => peers
                .into_iter()
                .filter(|p| p.reachable)
                .map(|p| p.node_id)
                .collect::<BTreeSet<_>>(),
            Err(_) => return state.node_id.clone(),
        };
        self.expire().await;
        let mut nodes = vec![local_snapshot(state).await];
        nodes.extend(
            self.snapshots
                .read()
                .await
                .values()
                .map(|e| e.snapshot.clone()),
        );
        select_live_tv_worker(nodes, &voters, &state.node_id)
    }

    pub(crate) async fn diagnostics(&self, state: &AppState) -> MediaDirectoryDiagnostics {
        self.expire().await;
        let mut nodes = vec![local_snapshot(state).await];
        nodes.extend(
            self.snapshots
                .read()
                .await
                .values()
                .map(|cached| cached.snapshot.clone()),
        );
        nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let remote_placement_enabled = state
            .store
            .get_setting(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        let remote_placement_rollout_ready = self.remote_rollout_ready().await;
        let remote_placement_ready = remote_placement_enabled;
        let session_takeover_enabled = state
            .store
            .get_setting(plurx_core::store::keys::CLUSTER_SESSION_TAKEOVER_ENABLED)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        MediaDirectoryDiagnostics {
            protocol_version: PROTOCOL_VERSION,
            remote_placement_enabled,
            remote_placement_rollout_ready,
            remote_placement_ready,
            session_takeover_enabled,
            session_takeover_ready: session_takeover_enabled && remote_placement_ready,
            local_active_sessions: state.transcode.active_sessions().await,
            snapshot_interval_seconds: SNAPSHOT_INTERVAL.as_secs(),
            snapshot_expiry_seconds: SNAPSHOT_EXPIRY.as_secs(),
            nodes,
        }
    }

    /// The saved operator preference controls placement. Compatibility and
    /// capacity are evaluated for each candidate, independently of other peers.
    pub(crate) async fn remote_placement_ready(&self, state: &AppState) -> bool {
        state
            .store
            .get_setting(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    }

    /// Advisory observation of uniform protocol publication. It never controls
    /// preference persistence, placement, or session takeover.
    ///
    /// The question is whether every committed VOTER is visible in the peer
    /// directory, which is not the same as whether the local node is one.
    /// `activity_peers` returns the other voters — it excludes this node and
    /// filters to voters — so the voters this node can see number
    /// `peers.len()` plus itself only when it is a voter. Comparing against
    /// `peers.len() + 1` unconditionally made the predicate `n != n + 1` on a
    /// learner, so it was structurally false there and no learner could ever
    /// accept a delegated session or place one.
    pub(crate) async fn remote_rollout_ready(&self) -> bool {
        if !self.membership.is_replicated() {
            return false;
        }
        self.expire().await;
        let deadline = deadline_after(SNAPSHOT_DEADLINE);
        let directory = tokio::time::timeout_at(deadline, async {
            tokio::join!(
                self.membership.activity_peers(),
                self.membership.activity_voter_count(),
                self.membership.local_node_is_committed_voter()
            )
        })
        .await;
        let Ok((Ok(peers), Ok(voter_count), Ok(local_is_voter))) = directory else {
            return false;
        };
        if voter_count <= 1 {
            return false;
        }
        let snapshots = self.snapshots.read().await;
        remote_directory_ready(
            &peers,
            voter_count,
            local_is_voter,
            std::ops::Deref::deref(&snapshots),
            tokio::time::Instant::now(),
        )
    }

    /// Local and remote catalogs share one deadline. Missing/old/busy workers
    /// contribute no evidence; a successful worker stays explicitly attached
    /// to its exact recipe so placement cannot spend another worker's proof.
    pub(crate) async fn quality_candidates(
        &self,
        state: &AppState,
        request: QualityCatalogRequest,
    ) -> Vec<WorkerQualityCandidate> {
        self.quality_catalog(state, request).await.candidates
    }

    pub(crate) async fn quality_catalog(
        &self,
        state: &AppState,
        request: QualityCatalogRequest,
    ) -> QualityCatalogResult {
        let started = tokio::time::Instant::now();
        let deadline = CREATE_STARTUP_BUDGET
            .try_with(|budget| {
                let count = budget
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                tracing::info!(
                    file_id = request.file_id,
                    catalog_call = count,
                    "create catalog accounting"
                );
                deadline_after(QUALITY_CATALOG_DEADLINE)
                    .min(budget.deadline - Duration::from_millis(500))
            })
            .unwrap_or_else(|_| deadline_after(QUALITY_CATALOG_DEADLINE));
        if let Err(error) = request.validate() {
            tracing::warn!(
                file_id = request.file_id,
                video_entries = request.caps.video.len(),
                clause = error.clause,
                observed = error.observed,
                limit = error.limit,
                elapsed_ms = started.elapsed().as_millis(),
                "quality catalog request_invalid before discovery"
            );
            return QualityCatalogResult::unavailable(CatalogCause::RequestInvalid(error));
        }
        let expected_binding = CREATE_STARTUP_BUDGET
            .try_with(|budget| {
                budget
                    .binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .ok()
            .flatten();
        let local = local_quality_catalog(state, &request, deadline, expected_binding.as_ref());
        let remote = async {
            let peers = match tokio::time::timeout_at(deadline, self.membership.media_peers()).await
            {
                Ok(Ok(peers)) => peers,
                _ => return vec![QualityCatalogResult::unavailable(CatalogCause::Transport)],
            };
            let results = stream::iter(
                peers
                    .into_iter()
                    .filter(|peer| {
                        peer.reachable && peer.http_base.is_some() && peer.node_id != state.node_id
                    })
                    .take(MAX_PEERS)
                    .map(|peer| {
                        let request = request.clone();
                        let expected_binding = expected_binding.clone();
                        async move {
                            let Some(base) = peer.http_base.as_deref() else {
                                return QualityCatalogResult::unavailable(CatalogCause::Transport);
                            };
                            // Clock independent duration: subtract both transit reserves before
                            // signing, and never restart the parent deadline on fallback.
                            let budget_ms = deadline
                                .saturating_duration_since(tokio::time::Instant::now())
                                .saturating_sub(Duration::from_millis(500))
                                .as_millis()
                                .min(1500) as u32;
                            if budget_ms == 0 {
                                return QualityCatalogResult::unavailable(
                                    CatalogCause::PeerDeadline,
                                );
                            }
                            let Ok(body) = serde_json::to_vec(&BudgetedCatalogRequest {
                                budget_ms,
                                request: request.clone(),
                                expected_binding,
                            }) else {
                                return QualityCatalogResult::unavailable(
                                    CatalogCause::PeerProtocol,
                                );
                            };
                            if body.len() > MAX_REQUEST_BYTES {
                                return QualityCatalogResult::unavailable(
                                    CatalogCause::PeerProtocol,
                                );
                            }
                            let response = self
                                .transport
                                .request(
                                    &peer.node_id,
                                    base,
                                    reqwest::Method::POST,
                                    QUALITY_CANDIDATES_V2_PATH,
                                    body,
                                    deadline,
                                    MAX_OFFER_BYTES,
                                    PeerAuthMode::ExactRequest,
                                )
                                .await;
                            let Ok(mut response) = response else {
                                return QualityCatalogResult::unavailable(CatalogCause::Transport);
                            };
                            let legacy = response.status == reqwest::StatusCode::NOT_FOUND;
                            if legacy {
                                let Ok(body) = serde_json::to_vec(&request) else {
                                    return QualityCatalogResult::unavailable(
                                        CatalogCause::PeerProtocol,
                                    );
                                };
                                if body.len() > MAX_REQUEST_BYTES {
                                    return QualityCatalogResult::unavailable(
                                        CatalogCause::PeerProtocol,
                                    );
                                }
                                response = match self
                                    .transport
                                    .request(
                                        &peer.node_id,
                                        base,
                                        reqwest::Method::POST,
                                        QUALITY_CANDIDATES_PATH,
                                        body,
                                        deadline,
                                        MAX_OFFER_BYTES,
                                        PeerAuthMode::ExactRequest,
                                    )
                                    .await
                                {
                                    Ok(response) => response,
                                    Err(_) => {
                                        return QualityCatalogResult::unavailable(
                                            CatalogCause::Transport,
                                        )
                                    }
                                };
                            }
                            if !response.status.is_success() {
                                let cause =
                                    match response.status.as_u16() {
                                        429 => CatalogCause::PeerBusy,
                                        504 => CatalogCause::PeerDeadline,
                                        400 => CatalogCause::PeerProtocol,
                                        _ => {
                                            if serde_json::from_slice::<serde_json::Value>(
                                                &response.body,
                                            )
                                            .ok()
                                            .is_some_and(|value| {
                                                value.get("code").and_then(|code| code.as_str())
                                                    == Some("serving_fenced")
                                            }) {
                                                CatalogCause::AuthorityRefused
                                            } else {
                                                CatalogCause::Transport
                                            }
                                        }
                                    };
                                return QualityCatalogResult::unavailable(cause);
                            }
                            let outcome = if legacy {
                                serde_json::from_slice::<Vec<WorkerQualityCandidate>>(
                                    &response.body,
                                )
                                .map(|candidates| {
                                    QualityCatalogResult {
                                        candidates,
                                        complete: false,
                                        causes: vec![CatalogCause::PeerProtocol],
                                        authority_refused: false,
                                    }
                                })
                            } else {
                                serde_json::from_slice::<QualityCatalogResult>(&response.body)
                            };
                            let Ok(outcome) = outcome else {
                                return QualityCatalogResult::unavailable(
                                    CatalogCause::PeerProtocol,
                                );
                            };
                            if outcome.candidates.len() > 32
                                || outcome.causes.len() > 16
                                || outcome.candidates.iter().any(|entry| {
                                    entry.node_id != peer.node_id
                                        || !entry.candidate.identity_matches()
                                        || !(1..=16_384).contains(&entry.candidate.width)
                                        || !(1..=16_384).contains(&entry.candidate.height)
                                        || entry.binding.as_ref().is_some_and(|binding| {
                                            binding.generation < 0
                                                || binding.source_digest.len() != 64
                                        })
                                })
                            {
                                return QualityCatalogResult::unavailable(
                                    CatalogCause::PeerProtocol,
                                );
                            }
                            outcome
                        }
                    }),
            )
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
            results
        };
        let (mut result, remote) = tokio::join!(local, remote);
        for outcome in remote {
            result.complete &= outcome.complete;
            result.authority_refused |= outcome.authority_refused;
            result.candidates.extend(outcome.candidates);
            for cause in outcome.causes {
                if result.causes.len() < 16 && !result.causes.contains(&cause) {
                    result.causes.push(cause);
                }
            }
        }
        tracing::info!(file_id = request.file_id, video_entries = request.caps.video.len(),
            candidate_count = result.candidates.len(), complete = result.complete, causes = ?result.causes,
            elapsed_ms = started.elapsed().as_millis(), "quality catalog discovery finished");
        result
    }

    pub(crate) async fn offers(
        &self,
        state: &AppState,
        request: MediaOfferRequest,
    ) -> PlacementDiagnostics {
        let deadline = deadline_after(OFFER_DEADLINE);
        if !request.is_valid() {
            return PlacementDiagnostics {
                request_fingerprint: request.request_fingerprint,
                budget_ms: u64::try_from(OFFER_DEADLINE.as_millis()).unwrap_or(u64::MAX),
                selected_node_id: None,
                offers: Vec::new(),
            };
        }
        self.expire().await;
        let snapshots = self
            .snapshots
            .read()
            .await
            .values()
            .map(|cached| (cached.snapshot.node_id.clone(), cached.snapshot.clone()))
            .collect::<HashMap<_, _>>();
        let (mut offers, local_snapshot) = match tokio::time::timeout_at(deadline, async {
            tokio::join!(local_offer(state, &request), local_snapshot(state))
        })
        .await
        {
            Ok((offer, snapshot)) => (vec![offer], Some(snapshot)),
            Err(_) => (Vec::new(), None),
        };
        let peers = tokio::time::timeout_at(deadline, self.membership.media_peers())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|peer| {
                peer.reachable
                    && peer.http_base.is_some()
                    && snapshots
                        .get(&peer.node_id)
                        .is_some_and(|snapshot| snapshot_can_answer(snapshot, &request))
            })
            .take(MAX_PEERS)
            .collect::<Vec<_>>();
        if !peers.is_empty() && tokio::time::Instant::now() < deadline {
            let body = serde_json::to_vec(&request).unwrap_or_default();
            let transport = self.transport.clone();
            let remote = stream::iter(peers.into_iter().map(|peer| {
                let transport = transport.clone();
                let body = body.clone();
                let fingerprint = request.request_fingerprint.clone();
                let scratch_bytes_required = request.scratch_bytes_required;
                async move {
                    fetch_offer(
                        &transport,
                        peer,
                        body,
                        &fingerprint,
                        scratch_bytes_required,
                        deadline,
                    )
                    .await
                }
            }))
            .buffer_unordered(MAX_PEERS)
            .filter_map(|offer| async move { offer })
            .collect::<Vec<_>>();
            let remote = tokio::time::timeout_at(deadline, remote)
                .await
                .unwrap_or_default();
            offers.extend(remote);
        }
        let mut ranking_snapshots = snapshots;
        if let Some(local_snapshot) = local_snapshot {
            ranking_snapshots.insert(state.node_id.clone(), local_snapshot);
        }
        rank_offers(
            &mut offers,
            &ranking_snapshots,
            &request.request_fingerprint,
            &state.node_id,
        );
        let selected_node_id = offers
            .first()
            .filter(|offer| offer.eligible)
            .map(|offer| offer.node_id.clone());
        PlacementDiagnostics {
            request_fingerprint: request.request_fingerprint,
            budget_ms: u64::try_from(OFFER_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            selected_node_id,
            offers,
        }
    }
}

fn select_live_tv_worker(
    nodes: Vec<MediaNodeSnapshot>,
    voters: &BTreeSet<String>,
    local_node_id: &str,
) -> String {
    nodes
        .into_iter()
        .filter(|n| {
            (n.node_id == local_node_id || voters.contains(&n.node_id))
                && n.live_tv_resource_processing
        })
        .max_by_key(|n| {
            (
                n.hardware_slots_max.saturating_sub(n.hardware_slots_used),
                n.software_threads_max
                    .saturating_sub(n.software_threads_used),
                n.scratch_pressure.preference(),
                n.egress_pressure.preference(),
                n.node_id == local_node_id,
            )
        })
        .map_or_else(|| local_node_id.to_owned(), |n| n.node_id)
}

fn remote_directory_ready(
    peers: &[ActivityPeer],
    voter_count: usize,
    local_is_voter: bool,
    snapshots: &BTreeMap<String, CachedSnapshot>,
    now: tokio::time::Instant,
) -> bool {
    voter_count == peers.len().saturating_add(usize::from(local_is_voter))
        && peers.iter().all(|peer| {
            peer.reachable
                && peer.http_base.is_some()
                && snapshots.get(&peer.node_id).is_some_and(|cached| {
                    cached.snapshot.protocol_version == PROTOCOL_VERSION && now <= cached.expires_at
                })
        })
}

async fn fetch_snapshot(
    transport: &PeerTransport,
    peer: ActivityPeer,
    deadline: tokio::time::Instant,
) -> Option<CachedSnapshot> {
    let base = peer.http_base.as_deref()?;
    let response = transport
        .request(
            &peer.node_id,
            base,
            reqwest::Method::GET,
            SNAPSHOT_PATH,
            Vec::new(),
            deadline,
            MAX_SNAPSHOT_BYTES,
            PeerAuthMode::ExactRequest,
        )
        .await
        .ok()?;
    let snapshot = response
        .status
        .is_success()
        .then(|| serde_json::from_slice(&response.body).ok())
        .flatten()?;
    accepted_snapshot(snapshot, &peer.node_id)
}

fn command_safe_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

fn bounded_command_safe_roots(roots: impl IntoIterator<Item = PathBuf>) -> BTreeSet<PathBuf> {
    roots
        .into_iter()
        // Preserve the scanner's supported cwd-relative semantics while
        // turning expression-like values such as `-delete` into absolute
        // command data rather than `find` grammar.
        .map(|root| command_safe_path(&root))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_LIBRARY_ROOTS)
        .collect()
}

/// Spawn one tracked root probe. `-H` follows a command-line symlink root, so
/// success means the target directory was actually enumerated rather than
/// merely observing the symlink object. Callers retain the child until
/// `try_wait` confirms exit: SIGKILL cannot immediately release a process in
/// uninterruptible filesystem I/O, and dropping/resubmitting it would turn a
/// hard mount into unbounded PID growth.
fn spawn_library_root_probe(
    root: &Path,
) -> std::io::Result<(tokio::process::Child, crate::process_control::ChildJob)> {
    debug_assert!(root.is_absolute());
    // Appending `.` forces directory traversal. A dangling symlink or a link
    // to a regular file fails instead of being mistaken for an empty readable
    // directory by `find -H`.
    let directory = root.join(".");
    let mut command = tokio::process::Command::new("find");
    command
        .arg("-H")
        .arg(directory)
        .args(["-mindepth", "1", "-maxdepth", "1", "-print", "-quit"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("library root reachability probe"),
    )
}

async fn fetch_offer(
    transport: &PeerTransport,
    peer: ActivityPeer,
    body: Vec<u8>,
    fingerprint: &str,
    scratch_bytes_required: u64,
    deadline: tokio::time::Instant,
) -> Option<MediaOffer> {
    let base = peer.http_base.as_deref()?;
    let response = transport
        .request(
            &peer.node_id,
            base,
            reqwest::Method::POST,
            OFFERS_PATH,
            body,
            deadline,
            MAX_OFFER_BYTES,
            PeerAuthMode::ExactRequest,
        )
        .await
        .ok()?;
    if !response.status.is_success() {
        return None;
    }
    let offer = serde_json::from_slice::<MediaOffer>(&response.body).ok()?;
    offer_is_bounded(&offer, &peer.node_id, fingerprint, scratch_bytes_required).then_some(offer)
}

pub(crate) async fn local_snapshot(state: &AppState) -> MediaNodeSnapshot {
    let runtime = state.transcode.media_node_runtime().await;
    let scratch_pressure =
        capacity_pressure(runtime.scratch_bytes_free, runtime.scratch_target_bytes);
    let workload_pressure = count_pressure(runtime.active_sessions, runtime.session_pressure_limit);
    MediaNodeSnapshot {
        node_id: state.node_id.clone(),
        observed_at_unix_ms: unix_ms(),
        build: crate::version::BUILD.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        encoders: runtime
            .encoder_families
            .into_iter()
            .map(|family| EncoderOfferCapability {
                family,
                max_target_height: runtime.max_target_height,
                decoders: runtime.decoders.clone(),
            })
            .collect(),
        tone_map: runtime
            .tone_map_pipelines
            .into_iter()
            .map(|pipeline| ToneMapOfferCapability { pipeline })
            .collect(),
        hardware_slots_used: bounded_u32(runtime.hardware_slots_used),
        hardware_slots_max: bounded_u32(runtime.hardware_slots_max),
        software_threads_used: bounded_u32(runtime.software_threads_used),
        software_threads_max: bounded_u32(runtime.software_threads_max),
        scratch_bytes_free: runtime.scratch_bytes_free,
        scratch_pressure,
        source_io_pressure: workload_pressure,
        egress_pressure: workload_pressure,
        live_waiting: runtime.live_waiting,
        background_active: runtime.background_active,
        io: media_io_observation(),
        // Older owners must not dispatch their removed relay endpoint here.
        live_tv_processing: false,
        live_tv_resource_processing: state.serving.is_ready()
            && !state.membership.local_maintenance_active(),
    }
}

impl QualityCatalogResult {
    pub(crate) fn unavailable(cause: CatalogCause) -> Self {
        Self {
            candidates: Vec::new(),
            authority_refused: cause == CatalogCause::AuthorityRefused,
            complete: false,
            causes: vec![cause],
        }
    }
}

pub(crate) async fn local_quality_catalog(
    state: &AppState,
    request: &QualityCatalogRequest,
    deadline: tokio::time::Instant,
    expected_binding: Option<&PlanningBinding>,
) -> QualityCatalogResult {
    if deadline <= tokio::time::Instant::now() {
        return QualityCatalogResult::unavailable(CatalogCause::LocalDeadline);
    }
    if let Err(error) = request.validate() {
        return QualityCatalogResult::unavailable(CatalogCause::RequestInvalid(error));
    }
    if state.membership.local_maintenance_active() {
        return QualityCatalogResult::unavailable(CatalogCause::Maintenance);
    }
    let progress = std::sync::Mutex::new(Vec::new());
    let binding = std::sync::Mutex::new(None);
    let work = async {
        if !state.serving.accepting_new_media().await {
            return Err(CatalogCause::AuthorityRefused);
        }
        let snapshot = state
            .store
            .playback_planning_snapshot(request.file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
            .await
            .map_err(|_| CatalogCause::SnapshotUnavailable)?
            .ok_or(CatalogCause::StaleSource)?;
        if snapshot.file.size != request.source_size || snapshot.file.mtime != request.source_mtime
        {
            return Err(CatalogCause::StaleSource);
        }
        if expected_binding
            .is_some_and(|expected| *expected != PlanningBinding::from_snapshot(&snapshot))
        {
            return Err(CatalogCause::StaleSource);
        }
        *binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(PlanningBinding::from_snapshot(&snapshot));
        state
            .transcode
            .quality_candidates_from_snapshot_progress(
                &snapshot,
                &request.caps,
                request.audio_index,
                request.audio_offset_ms,
                request.subtitle_burn,
                request.presentation,
                request.copy_contract,
                Some(&progress),
                None,
            )
            .await;
        Ok(())
    };
    let (complete, causes) = match tokio::time::timeout_at(deadline, work).await {
        Ok(Ok(())) => (true, Vec::new()),
        Ok(Err(cause)) => (false, vec![cause]),
        Err(_) => (false, vec![CatalogCause::LocalDeadline]),
    };
    let binding = binding
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let candidates = progress
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .take(32)
        .map(|candidate| WorkerQualityCandidate {
            node_id: state.node_id.clone(),
            candidate,
            binding: binding.clone(),
            partial: !complete,
            dispatch_supported: true,
        })
        .collect();
    QualityCatalogResult {
        candidates,
        authority_refused: causes.contains(&CatalogCause::AuthorityRefused),
        complete,
        causes,
    }
}

pub(crate) async fn local_offer(state: &AppState, request: &MediaOfferRequest) -> MediaOffer {
    let base = || MediaOffer {
        node_id: state.node_id.clone(),
        request_fingerprint: request.request_fingerprint.clone(),
        eligible: false,
        refusal_code: None,
        cache_hit: false,
        source_likely_readable: false,
        scratch_bytes_required: request.scratch_bytes_required,
        scratch_bytes_free: 0,
        source_io_pressure: PressureBand::Idle,
        egress_pressure: PressureBand::Idle,
        method: MediaDeliveryMethod::Transcode,
        encoder: String::new(),
        pipeline: String::new(),
        free_hardware_slots: 0,
        free_software_threads: 0,
        recent_speed: None,
    };
    if !request.is_valid() {
        return MediaOffer {
            refusal_code: Some("invalid_request".to_owned()),
            ..base()
        };
    }
    if state.membership.local_maintenance_active() {
        return MediaOffer {
            refusal_code: Some("node_maintenance".to_owned()),
            ..base()
        };
    }
    if !state.serving.accepting_new_media().await {
        return MediaOffer {
            refusal_code: Some("serving_fenced".to_owned()),
            ..base()
        };
    }
    let file = match state.store.get_file(request.file_id).await {
        Ok(Some(file)) => file,
        _ => {
            return MediaOffer {
                refusal_code: Some("file_missing".to_owned()),
                ..base()
            }
        }
    };
    if file.size != request.source_size || file.mtime != request.source_mtime {
        return MediaOffer {
            refusal_code: Some("stale_source".to_owned()),
            ..base()
        };
    }
    if scratch_estimate(&file, request.target_height) != request.scratch_bytes_required {
        return MediaOffer {
            refusal_code: Some("scratch_contract_changed".to_owned()),
            ..base()
        };
    }
    if file
        .duration_ms
        .is_some_and(|duration| request.start_millis > duration.max(0))
    {
        return MediaOffer {
            refusal_code: Some("start_out_of_range".to_owned()),
            ..base()
        };
    }
    if decoder_contract(&file) != Some(request.decoder.as_str()) {
        return MediaOffer {
            refusal_code: Some("decoder_contract_changed".to_owned()),
            ..base()
        };
    }
    if !requested_tracks_exist(&file, request.audio_index, request.subtitle_index) {
        return MediaOffer {
            refusal_code: Some("track_missing".to_owned()),
            ..base()
        };
    }
    let source_likely_readable = state.availability.recently_present(file.id)
        || state.media_pool.root_likely_readable(&file.path).await;
    match state
        .transcode
        .media_offer_probe(
            &file,
            request.target_height,
            request.start_millis as f64 / 1_000.0,
            request.audio_index,
            request.subtitle_index,
            request.hdr10,
        )
        .await
    {
        Ok(probe) => {
            let source_io_pressure =
                count_pressure(probe.active_sessions, probe.session_pressure_limit);
            let egress_pressure = source_io_pressure;
            let scratch_ok = probe.scratch_bytes_free >= request.scratch_bytes_required;
            let capability_ok = probe.decoder_supported
                && probe.target_supported
                && request.output_contract == "hls-mpegts-v1";
            let pressure_ok = !matches!(egress_pressure, PressureBand::Unavailable);
            let eligible = capability_ok
                && pressure_ok
                && (probe.cache_hit || (source_likely_readable && scratch_ok));
            let refusal_code = (!eligible).then(|| {
                if !capability_ok {
                    "incapable"
                } else if !pressure_ok {
                    "egress_saturated"
                } else if !source_likely_readable && !probe.cache_hit {
                    "source_unverified"
                } else if !scratch_ok && !probe.cache_hit {
                    "scratch_full"
                } else {
                    "temporarily_busy"
                }
                .to_owned()
            });
            MediaOffer {
                eligible,
                refusal_code,
                cache_hit: probe.cache_hit,
                source_likely_readable,
                scratch_bytes_free: probe.scratch_bytes_free,
                source_io_pressure,
                egress_pressure,
                encoder: probe.encoder,
                pipeline: probe.pipeline,
                free_hardware_slots: bounded_u32(probe.free_hardware_slots),
                free_software_threads: bounded_u32(probe.free_software_threads),
                recent_speed: probe
                    .recent_speed
                    .filter(|speed| speed.is_finite() && *speed > 0.0),
                ..base()
            }
        }
        Err(code) => MediaOffer {
            refusal_code: Some(code.to_owned()),
            source_likely_readable,
            ..base()
        },
    }
}

fn has_reachable_media_peer(peers: &[ActivityPeer]) -> bool {
    peers
        .iter()
        .any(|peer| peer.reachable && peer.http_base.is_some())
}

const fn snapshot_fanout_concurrency() -> usize {
    MAX_PEERS
}

fn requested_tracks_exist(
    file: &MediaFile,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
) -> bool {
    audio_index.is_none_or(|index| file.audio_streams.iter().any(|track| track.index == index))
        && subtitle_index.is_none_or(|index| {
            file.subtitle_streams
                .iter()
                .any(|track| track.index == index)
        })
}

fn snapshot_can_answer(snapshot: &MediaNodeSnapshot, request: &MediaOfferRequest) -> bool {
    snapshot.protocol_version == PROTOCOL_VERSION
        && snapshot.egress_pressure != PressureBand::Unavailable
        && snapshot.encoders.iter().any(|capability| {
            capability.max_target_height >= request.target_height
                && capability
                    .decoders
                    .iter()
                    .any(|decoder| decoder == &request.decoder)
        })
        && (!request.hdr10 || !snapshot.tone_map.is_empty())
}

fn snapshot_is_bounded(snapshot: &MediaNodeSnapshot, expected_node_id: &str) -> bool {
    snapshot.node_id == expected_node_id
        && !snapshot.node_id.is_empty()
        && snapshot.node_id.len() <= 256
        && snapshot.build.len() <= 256
        && snapshot.protocol_version == PROTOCOL_VERSION
        && snapshot_remaining_ttl(snapshot.observed_at_unix_ms).is_some()
        && snapshot.encoders.len() <= MAX_CAPABILITIES
        && snapshot.tone_map.len() <= MAX_CAPABILITIES
        && snapshot.encoders.iter().all(|capability| {
            !capability.family.is_empty()
                && capability.family.len() <= MAX_CAPABILITY_BYTES
                && capability.max_target_height > 0
                && capability.decoders.len() <= MAX_CAPABILITIES
                && capability
                    .decoders
                    .iter()
                    .all(|decoder| !decoder.is_empty() && decoder.len() <= MAX_CAPABILITY_BYTES)
        })
        && snapshot.tone_map.iter().all(|capability| {
            !capability.pipeline.is_empty() && capability.pipeline.len() <= MAX_CAPABILITY_BYTES
        })
}

fn accepted_snapshot(
    snapshot: MediaNodeSnapshot,
    expected_node_id: &str,
) -> Option<CachedSnapshot> {
    if !snapshot_is_bounded(&snapshot, expected_node_id) {
        return None;
    }
    let remaining = snapshot_remaining_ttl(snapshot.observed_at_unix_ms)?;
    Some(CachedSnapshot {
        snapshot,
        expires_at: tokio::time::Instant::now() + remaining,
    })
}

fn snapshot_remaining_ttl(observed_at_unix_ms: i64) -> Option<Duration> {
    let budget_ms = u64::try_from(SNAPSHOT_EXPIRY.as_millis()).unwrap_or(u64::MAX);
    let age_ms = unix_ms().abs_diff(observed_at_unix_ms);
    budget_ms.checked_sub(age_ms).map(Duration::from_millis)
}

fn offer_is_bounded(
    offer: &MediaOffer,
    expected_node_id: &str,
    fingerprint: &str,
    scratch_bytes_required: u64,
) -> bool {
    offer.node_id == expected_node_id
        && offer.node_id.len() <= 256
        && offer.request_fingerprint == fingerprint
        && offer.request_fingerprint.len() == 64
        && offer.scratch_bytes_required == scratch_bytes_required
        && offer.eligible == offer.refusal_code.is_none()
        && offer
            .refusal_code
            .as_ref()
            .is_none_or(|value| value.len() <= 64)
        && offer.encoder.len() <= MAX_CAPABILITY_BYTES
        && offer.pipeline.len() <= MAX_CAPABILITY_BYTES
        && offer
            .recent_speed
            .is_none_or(|speed| speed.is_finite() && speed > 0.0)
}

fn rank_offers(
    offers: &mut [MediaOffer],
    snapshots: &HashMap<String, MediaNodeSnapshot>,
    fingerprint: &str,
    local_node_id: &str,
) {
    offers.sort_by(|left, right| {
        compare_offer(right, left, snapshots, fingerprint, local_node_id)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
}

fn compare_offer(
    left: &MediaOffer,
    right: &MediaOffer,
    snapshots: &HashMap<String, MediaNodeSnapshot>,
    fingerprint: &str,
    local_node_id: &str,
) -> Ordering {
    let left_snapshot = snapshots.get(&left.node_id);
    let right_snapshot = snapshots.get(&right.node_id);
    let left_can_start = can_start_now(left, left_snapshot);
    let right_can_start = can_start_now(right, right_snapshot);
    left.eligible
        .cmp(&right.eligible)
        .then_with(|| left.cache_hit.cmp(&right.cache_hit))
        .then_with(|| left_can_start.cmp(&right_can_start))
        .then_with(|| pressure_score(left).cmp(&pressure_score(right)))
        .then_with(|| {
            match (
                left_snapshot.and_then(|node| node.io.storage_read_micros),
                right_snapshot.and_then(|node| node.io.storage_read_micros),
            ) {
                (Some(left), Some(right)) => right.cmp(&left),
                (Some(_), None) => Ordering::Greater,
                (None, Some(_)) => Ordering::Less,
                (None, None) => Ordering::Equal,
            }
        })
        .then_with(|| compare_speed(left.recent_speed, right.recent_speed))
        .then_with(|| compare_spare(left, right, left_snapshot, right_snapshot))
        .then_with(|| {
            rendezvous(fingerprint, &left.node_id).cmp(&rendezvous(fingerprint, &right.node_id))
        })
        .then_with(|| (left.node_id == local_node_id).cmp(&(right.node_id == local_node_id)))
}

fn can_start_now(offer: &MediaOffer, snapshot: Option<&MediaNodeSnapshot>) -> bool {
    offer.cache_hit
        || (offer.free_hardware_slots > 0 || offer.free_software_threads > 0)
            && snapshot.is_none_or(|snapshot| !snapshot.background_active)
}

fn pressure_score(offer: &MediaOffer) -> (u8, u8, u8) {
    (
        capacity_pressure(offer.scratch_bytes_free, offer.scratch_bytes_required).preference(),
        offer.source_io_pressure.preference(),
        offer.egress_pressure.preference(),
    )
}

fn compare_speed(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn compare_spare(
    left: &MediaOffer,
    right: &MediaOffer,
    left_snapshot: Option<&MediaNodeSnapshot>,
    right_snapshot: Option<&MediaNodeSnapshot>,
) -> Ordering {
    let left_free = u64::from(left.free_hardware_slots) + u64::from(left.free_software_threads);
    let right_free = u64::from(right.free_hardware_slots) + u64::from(right.free_software_threads);
    let left_total = left_snapshot.map_or(left_free.max(1), |snapshot| {
        u64::from(snapshot.hardware_slots_max) + u64::from(snapshot.software_threads_max)
    });
    let right_total = right_snapshot.map_or(right_free.max(1), |snapshot| {
        u64::from(snapshot.hardware_slots_max) + u64::from(snapshot.software_threads_max)
    });
    left_free
        .saturating_mul(right_total.max(1))
        .cmp(&right_free.saturating_mul(left_total.max(1)))
}

fn rendezvous(fingerprint: &str, node_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(node_id.as_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
}

pub(crate) fn decoder_contract(file: &MediaFile) -> Option<&'static str> {
    match file.video_codec.as_deref()? {
        "h264" | "avc" => Some("h264"),
        "hevc" | "h265" | "hevc10" => Some("hevc"),
        "vp8" => Some("vp8"),
        "vp9" => Some("vp9"),
        "av1" => Some("av1"),
        "mpeg4" => Some("mpeg4"),
        "mpeg2video" => Some("mpeg2video"),
        _ => None,
    }
}

fn scratch_estimate(file: &MediaFile, target_height: i64) -> u64 {
    const OVERHEAD: u64 = 64 * 1024 * 1024;
    let duration_ms = u64::try_from(file.duration_ms.unwrap_or_default().max(0)).unwrap_or(0);
    let peak_kbps = match target_height {
        height if height <= 480 => 2_000_u64,
        height if height <= 720 => 4_000,
        height if height <= 1080 => 8_000,
        _ => 20_000,
    };
    duration_ms
        .saturating_mul(peak_kbps)
        .saturating_div(8)
        .saturating_mul(2)
        .saturating_add(OVERHEAD)
}

fn capacity_pressure(free: u64, target: u64) -> PressureBand {
    if free == 0 {
        PressureBand::Unavailable
    } else if target > 0 && free < target {
        PressureBand::High
    } else if target > 0 && free < target.saturating_mul(2) {
        PressureBand::Moderate
    } else {
        PressureBand::Idle
    }
}

fn count_pressure(used: usize, limit: usize) -> PressureBand {
    if limit == 0 || used >= limit {
        PressureBand::Unavailable
    } else if used.saturating_mul(4) >= limit.saturating_mul(3) {
        PressureBand::High
    } else if used.saturating_mul(2) >= limit {
        PressureBand::Moderate
    } else {
        PressureBand::Idle
    }
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn root_probe_capacity_remaining(running: usize) -> usize {
    MAX_ROOT_PROBE_CHILDREN.saturating_sub(running)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::{AudioStream, SubtitleStream};
    use tokio::sync::mpsc;

    fn snapshot(node: &str, decoders: &[&str], max_height: i64) -> MediaNodeSnapshot {
        MediaNodeSnapshot {
            node_id: node.to_owned(),
            observed_at_unix_ms: unix_ms(),
            build: "test".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            encoders: vec![EncoderOfferCapability {
                family: "software".to_owned(),
                max_target_height: max_height,
                decoders: decoders.iter().map(|value| (*value).to_owned()).collect(),
            }],
            tone_map: Vec::new(),
            hardware_slots_used: 0,
            hardware_slots_max: 0,
            software_threads_used: 0,
            software_threads_max: 4,
            scratch_bytes_free: 1_000,
            scratch_pressure: PressureBand::Idle,
            source_io_pressure: PressureBand::Idle,
            egress_pressure: PressureBand::Idle,
            live_waiting: false,
            background_active: false,
            io: MediaIoObservation::default(),
            live_tv_processing: false,
            live_tv_resource_processing: true,
        }
    }

    #[test]
    fn catalog_validation_retains_video_count_clause() {
        let caps = serde_json::from_value(serde_json::json!({
            "v": 2, "video": (0..65).map(|_| serde_json::json!({"codec": "h264", "present": ["sdr"]})).collect::<Vec<_>>()
        })).expect("valid decoder rows");
        let request = QualityCatalogRequest {
            copy_contract: None,
            file_id: 1,
            source_size: 10,
            source_mtime: 1,
            caps,
            audio_index: Some(1),
            audio_offset_ms: 0,
            subtitle_burn: None,
            presentation: crate::transcode::Presentation::Vod,
        };
        assert_eq!(
            request.validate(),
            Err(CatalogValidationError {
                clause: "video_entries".to_owned(),
                observed: 65,
                limit: 64,
            })
        );
    }

    #[tokio::test]
    async fn startup_remaining_allowance_is_capped_and_keeps_one_deadline() {
        let before = tokio::time::Instant::now();
        let bounded = CreateStartupBudget::new(u64::MAX);
        assert!(bounded.deadline <= before + Duration::from_millis(10_001));
        let short = CreateStartupBudget::new(25);
        let deadline = short.deadline;
        short
            .scope(async {
                assert_eq!(
                    CREATE_STARTUP_BUDGET.with(|budget| budget.deadline),
                    deadline
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
                assert_eq!(
                    CREATE_STARTUP_BUDGET.with(|budget| budget.deadline),
                    deadline
                );
            })
            .await;
        assert!(short.deadline < bounded.deadline);
    }

    #[test]
    fn quality_catalog_requires_bounded_source_tracks_and_current_caps() {
        let mut request = QualityCatalogRequest {
            copy_contract: None,
            file_id: 1,
            source_size: 10,
            source_mtime: 1,
            caps: plurx_core::playback::DeviceCaps {
                v: 2,
                ..Default::default()
            },
            audio_index: Some(0),
            audio_offset_ms: 15_000,
            subtitle_burn: None,
            presentation: crate::transcode::Presentation::Vod,
        };
        assert!(request.is_valid());
        request.audio_offset_ms = 15_001;
        assert!(!request.is_valid());
        request.audio_offset_ms = 0;
        request.subtitle_burn = Some(-1);
        assert!(!request.is_valid());
        request.subtitle_burn = None;
        request.caps.v = 1;
        assert!(!request.is_valid());
        request.caps.v = 2;
        request.source_size = -1;
        assert!(!request.is_valid());
    }

    fn offer(node: &str) -> MediaOffer {
        MediaOffer {
            node_id: node.to_owned(),
            request_fingerprint: "a".repeat(64),
            eligible: true,
            refusal_code: None,
            cache_hit: false,
            source_likely_readable: true,
            scratch_bytes_required: 100,
            scratch_bytes_free: 1_000,
            source_io_pressure: PressureBand::Idle,
            egress_pressure: PressureBand::Idle,
            method: MediaDeliveryMethod::Transcode,
            encoder: "software".to_owned(),
            pipeline: "cpu".to_owned(),
            free_hardware_slots: 0,
            free_software_threads: 4,
            recent_speed: Some(2.0),
        }
    }

    fn media_file_with_tracks() -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 1,
            item_id: 1,
            path: PathBuf::from("/media/movie.mkv"),
            size: 1,
            mtime: 1,
            duration_ms: Some(1_000),
            container: Some("mkv".to_owned()),
            video_codec: Some("h264".to_owned()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![AudioStream {
                index: 4,
                ..AudioStream::default()
            }],
            subtitle_streams: vec![SubtitleStream {
                index: 7,
                ..SubtitleStream::default()
            }],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    #[test]
    fn snapshot_bounds_reject_wrong_identity_and_protocol() {
        let accepted = snapshot("peer-a", &["hevc"], 1080);
        assert!(snapshot_is_bounded(&accepted, "peer-a"));
        assert!(!snapshot_is_bounded(&accepted, "peer-b"));

        let mut incompatible = accepted;
        incompatible.protocol_version = PROTOCOL_VERSION.saturating_add(1);
        assert!(!snapshot_is_bounded(&incompatible, "peer-a"));
    }

    // The arithmetic above is only right where it is used, and a helper test
    // leaves the call site free: the defect was one `+ 1` inside
    // `remote_rollout_ready`. Pin that it asks membership for the local role
    // and hands it through, and that the old unconditional form is gone.
    #[test]
    fn advisory_rollout_observation_asks_membership_whether_this_node_is_a_voter() {
        let body = include_str!("media_pool.rs")
            .split_once("pub(crate) async fn remote_rollout_ready(")
            .expect("remote_rollout_ready was renamed")
            .1
            .split_once("\n    }\n")
            .expect("remote_rollout_ready never closes at fn indent")
            .0;
        assert!(
            body.contains("local_node_is_committed_voter()"),
            "the rollout gate must read the local committed role, not assume it",
        );
        assert!(
            body.contains("local_is_voter,"),
            "the local role must reach remote_directory_ready",
        );
        assert!(
            !body.contains("peers.len().saturating_add(1)"),
            "the unconditional +1 is what made this unsatisfiable on a learner",
        );
    }

    #[test]
    fn advisory_rollout_observation_reports_every_voter_on_the_current_protocol() {
        let now = tokio::time::Instant::now();
        let peers = vec![ActivityPeer {
            node_id: "peer-a".to_owned(),
            raft_id: 2,
            http_base: Some("http://peer-a:8080".to_owned()),
            reachable: true,
        }];
        let mut snapshots = BTreeMap::from([(
            "peer-a".to_owned(),
            CachedSnapshot {
                snapshot: snapshot("peer-a", &["h264"], 1080),
                expires_at: now + Duration::from_secs(1),
            },
        )]);
        assert!(remote_directory_ready(&peers, 2, true, &snapshots, now));
        assert!(!remote_directory_ready(&peers, 3, true, &snapshots, now));

        // The same directory read from a LEARNER. `activity_peers` never
        // returns the local node and never returns a learner, so every voter a
        // learner can see is already in `peers` and there is no self to add.
        // Counting itself as a voter made the predicate `n != n + 1` there, so
        // it was unsatisfiable for any roster size. A learner that can see the
        // one voter passes the arithmetic (`remote_rollout_ready` separately
        // refuses a single-voter cluster); one that is missing a voter fails.
        assert!(remote_directory_ready(&peers, 1, false, &snapshots, now));
        assert!(!remote_directory_ready(&peers, 2, false, &snapshots, now));

        snapshots
            .get_mut("peer-a")
            .expect("peer snapshot")
            .snapshot
            .protocol_version = PROTOCOL_VERSION - 1;
        assert!(!remote_directory_ready(&peers, 2, true, &snapshots, now));
        assert!(!remote_directory_ready(&peers, 1, false, &snapshots, now));
    }

    #[test]
    fn verified_cache_beats_source_and_pressure() {
        let mut cached = offer("cache");
        cached.cache_hit = true;
        cached.source_likely_readable = false;
        cached.source_io_pressure = PressureBand::High;
        let idle = offer("idle");
        let mut offers = vec![idle, cached];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "idle");
        assert_eq!(offers[0].node_id, "cache");
    }

    #[test]
    fn hard_refusals_and_saturated_egress_never_win() {
        let healthy = offer("healthy");
        let mut stale = offer("stale");
        stale.eligible = false;
        stale.refusal_code = Some("stale_source".to_owned());
        stale.cache_hit = true;
        let mut saturated = offer("saturated");
        saturated.eligible = false;
        saturated.egress_pressure = PressureBand::Unavailable;
        let mut offers = vec![saturated, stale, healthy];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "healthy");
        assert_eq!(offers[0].node_id, "healthy");
    }

    #[test]
    fn heterogeneous_idle_nodes_are_shortlisted_only_when_capable() {
        let request = MediaOfferRequest {
            protocol_version: PROTOCOL_VERSION,
            request_fingerprint: "a".repeat(64),
            file_id: 1,
            source_size: 1,
            source_mtime: 1,
            decoder: "hevc".to_owned(),
            target_height: 1080,
            start_millis: 0,
            audio_index: None,
            subtitle_index: None,
            hdr10: false,
            output_contract: "hls-mpegts-v1".to_owned(),
            scratch_bytes_required: 1,
        };
        assert!(snapshot_can_answer(
            &snapshot("capable", &["hevc"], 1080),
            &request
        ));
        assert!(!snapshot_can_answer(
            &snapshot("wrong-codec", &["h264"], 2160),
            &request
        ));
        assert!(!snapshot_can_answer(
            &snapshot("too-small", &["hevc"], 720),
            &request
        ));
    }

    #[test]
    fn scratch_full_stale_source_and_saturated_egress_cannot_outrank_healthy() {
        let healthy = offer("healthy");
        let mut scratch_full = offer("scratch-full");
        scratch_full.eligible = false;
        scratch_full.refusal_code = Some("scratch_full".to_owned());
        scratch_full.scratch_bytes_free = 0;
        scratch_full.recent_speed = Some(100.0);
        let mut stale = offer("stale");
        stale.eligible = false;
        stale.refusal_code = Some("stale_source".to_owned());
        stale.free_hardware_slots = 100;
        let mut saturated = offer("saturated");
        saturated.eligible = false;
        saturated.refusal_code = Some("egress_saturated".to_owned());
        saturated.egress_pressure = PressureBand::Unavailable;
        saturated.cache_hit = true;
        let mut offers = vec![scratch_full, stale, saturated, healthy];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "healthy");
        assert_eq!(offers[0].node_id, "healthy");
    }

    #[test]
    fn requested_tracks_must_exist_in_the_exact_file_snapshot() {
        let file = media_file_with_tracks();
        assert!(requested_tracks_exist(&file, Some(4), Some(7)));
        assert!(!requested_tracks_exist(&file, Some(3), Some(7)));
        assert!(!requested_tracks_exist(&file, Some(4), Some(6)));
    }

    #[test]
    fn root_probes_require_a_reachable_remote_media_consumer() {
        assert!(!has_reachable_media_peer(&[]));
        assert!(!has_reachable_media_peer(&[ActivityPeer {
            node_id: "offline".to_owned(),
            raft_id: 2,
            http_base: Some("http://offline:32400".to_owned()),
            reachable: false,
        }]));
        assert!(has_reachable_media_peer(&[ActivityPeer {
            node_id: "peer".to_owned(),
            raft_id: 2,
            http_base: Some("http://peer:32400".to_owned()),
            reachable: true,
        }]));
    }

    #[test]
    fn root_probe_command_makes_relative_paths_safe_and_keeps_a_hard_bound() {
        let roots = bounded_command_safe_roots([
            PathBuf::from("-delete"),
            PathBuf::from("relative/library"),
            PathBuf::from("/media/library"),
        ]);
        assert_eq!(roots.len(), 3);
        assert!(roots.iter().all(|root| root.is_absolute()));
        assert!(roots.contains(&PathBuf::from("/media/library")));

        let bounded = bounded_command_safe_roots(
            (0..=MAX_LIBRARY_ROOTS).map(|index| PathBuf::from(format!("/media/{index}"))),
        );
        assert_eq!(bounded.len(), MAX_LIBRARY_ROOTS);
    }

    #[test]
    fn one_timed_out_generation_leaves_room_for_one_current_generation() {
        assert_eq!(
            root_probe_capacity_remaining(MAX_LIBRARY_ROOTS),
            MAX_LIBRARY_ROOTS
        );
        assert_eq!(root_probe_capacity_remaining(MAX_ROOT_PROBE_CHILDREN), 0);
        assert_eq!(
            root_probe_capacity_remaining(MAX_ROOT_PROBE_CHILDREN.saturating_add(1)),
            0
        );
    }

    #[tokio::test]
    async fn deadline_fencing_retains_children_and_rejects_delayed_success() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        let running_root = PathBuf::from("/media/running");
        let running_child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn retained root probe");
        pool.root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
            .insert(
                running_root.clone(),
                RootProbe {
                    child: running_child,
                    kill_sent: false,
                    _job: None,
                },
            );
        pool.signal_timed_out_root_probes(&BTreeSet::from([running_root.clone()]));
        let mut retained = pool
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
            .remove(&running_root)
            .expect("timed-out child remains tracked until reap");
        assert!(retained.kill_sent);
        retained
            .child
            .wait()
            .await
            .expect("reap retained root probe");

        let completed_root = PathBuf::from("/media/completed-after-deadline");
        let mut completed_child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn completed root probe");
        loop {
            if completed_child
                .try_wait()
                .expect("observe completed root probe")
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        pool.root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
            .insert(
                completed_root.clone(),
                RootProbe {
                    child: completed_child,
                    kill_sent: false,
                    _job: None,
                },
            );
        let outcomes = pool.reap_root_probes_until(
            &BTreeSet::from([completed_root.clone()]),
            tokio::time::Instant::now(),
        );
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, completed_root);
        assert!(!outcomes[0].1.readable);
    }

    #[tokio::test]
    async fn blackholed_first_eight_peers_do_not_starve_the_ninth() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let fanout = stream::iter((0..9).map(move |index| {
            let started_tx = started_tx.clone();
            async move {
                if index < 8 {
                    std::future::pending::<()>().await;
                }
                let _ = started_tx.send(index);
            }
        }))
        .buffer_unordered(snapshot_fanout_concurrency())
        .collect::<Vec<_>>();
        let task = tokio::spawn(fanout);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), started_rx.recv())
                .await
                .expect("ninth peer starts without waiting for the first eight")
                .expect("fanout sender remains open"),
            8
        );
        task.abort();
    }

    #[test]
    fn a_busy_tuner_owner_uses_a_free_processor_despite_an_incompatible_peer() {
        let mut busy = snapshot("owner", &["h264"], 1080);
        busy.hardware_slots_max = 1;
        busy.hardware_slots_used = 1;
        busy.software_threads_max = 4;
        busy.software_threads_used = 4;
        let mut free = snapshot("free", &["h264"], 1080);
        free.hardware_slots_max = 1;
        free.hardware_slots_used = 0;
        free.io = MediaIoObservation::default();
        let mut old = free.clone();
        old.node_id = "old".into();
        old.live_tv_processing = true;
        old.live_tv_resource_processing = false;
        old.hardware_slots_max = 100;
        let mut learner = free.clone();
        learner.node_id = "learner".into();
        learner.hardware_slots_max = 100;
        let voters = BTreeSet::from(["free".into(), "old".into()]);
        assert_eq!(
            select_live_tv_worker(vec![busy, free, old, learner], &voters, "owner"),
            "free"
        );
    }

    #[test]
    fn recent_storage_latency_ranks_workers_without_refusing_missing_samples() {
        let mut slow = snapshot("slow", &["h264"], 1080);
        slow.io.storage_read_micros = Some(50_000);
        let mut fast = snapshot("fast", &["h264"], 1080);
        fast.io.storage_read_micros = Some(1000);
        let snapshots = HashMap::from([("slow".into(), slow), ("fast".into(), fast)]);
        let mut candidates = vec![offer("slow"), offer("fast")];
        rank_offers(&mut candidates, &snapshots, "request", "slow");
        assert_eq!(candidates[0].node_id, "fast");
        let unknown = offer("unknown");
        assert!(unknown.eligible);
        assert!(offer_is_bounded(
            &unknown,
            "unknown",
            &"a".repeat(64),
            unknown.scratch_bytes_required
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn snapshots_expire_after_fifteen_seconds() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        pool.snapshots.write().await.insert(
            "peer".to_owned(),
            CachedSnapshot {
                snapshot: MediaNodeSnapshot {
                    node_id: "peer".to_owned(),
                    observed_at_unix_ms: 1,
                    build: "test".to_owned(),
                    protocol_version: PROTOCOL_VERSION,
                    encoders: Vec::new(),
                    tone_map: Vec::new(),
                    hardware_slots_used: 0,
                    hardware_slots_max: 0,
                    software_threads_used: 0,
                    software_threads_max: 0,
                    scratch_bytes_free: 0,
                    scratch_pressure: PressureBand::Unavailable,
                    source_io_pressure: PressureBand::Unavailable,
                    egress_pressure: PressureBand::Unavailable,
                    live_waiting: false,
                    background_active: false,
                    io: MediaIoObservation::default(),
                    live_tv_processing: false,
                    live_tv_resource_processing: true,
                },
                expires_at: tokio::time::Instant::now() + SNAPSHOT_EXPIRY,
            },
        );
        tokio::time::advance(SNAPSHOT_EXPIRY + Duration::from_millis(1)).await;
        pool.expire().await;
        assert!(pool.snapshots.read().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_cannot_restart_an_old_snapshots_full_ttl() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        let mut old = snapshot("peer", &["h264"], 1080);
        old.observed_at_unix_ms = unix_ms() - 10_000;
        let cached = accepted_snapshot(old, "peer").expect("ten-second-old snapshot remains fresh");
        pool.snapshots
            .write()
            .await
            .insert("peer".to_owned(), cached);

        tokio::time::advance(Duration::from_millis(5_100)).await;
        pool.expire().await;
        assert!(pool.snapshots.read().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn root_readability_is_specific_and_age_bounded() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        let observed_at = tokio::time::Instant::now();
        pool.root_readability.write().await.extend([
            (
                PathBuf::from("/media"),
                RootReadability {
                    readable: true,
                    observed_at,
                },
            ),
            (
                PathBuf::from("/media/offline"),
                RootReadability {
                    readable: false,
                    observed_at,
                },
            ),
        ]);

        assert!(
            pool.root_likely_readable(Path::new("/media/movie.mkv"))
                .await
        );
        assert!(
            !pool
                .root_likely_readable(Path::new("/media/offline/movie.mkv"))
                .await
        );
        tokio::time::advance(ROOT_READABILITY_TTL + Duration::from_millis(1)).await;
        assert!(
            !pool
                .root_likely_readable(Path::new("/media/movie.mkv"))
                .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn common_offer_deadline_is_exact() {
        let started = tokio::time::Instant::now();
        let deadline = started + OFFER_DEADLINE;
        let fanout = stream::iter((0..MAX_PEERS).map(|_| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }))
        .buffer_unordered(MAX_PEERS)
        .collect::<Vec<_>>();
        let outcome = tokio::time::timeout_at(deadline, fanout).await;
        assert!(outcome.is_err());
        assert_eq!(started.elapsed(), OFFER_DEADLINE);
    }

    #[tokio::test]
    async fn a_single_node_poll_performs_no_peer_http_work() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        pool.poll_once().await.expect("empty membership is a no-op");
        assert!(pool.snapshots.read().await.is_empty());
    }
}
