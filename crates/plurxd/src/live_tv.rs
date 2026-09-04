//! HDHomeRun configuration, LAN trust boundary, and owner-side lineup cache.
//!
//! This module is always compiled. `live_tv.enabled` is an operator-controlled
//! runtime decision, never a Cargo feature or an environment-only escape hatch.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use futures_util::StreamExt;
use plurx_core::store::{keys, Store};
use plurx_core::transcode::{EffectiveRateControl, Encoder};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::state::SystemInfo;

pub(crate) const SNAPSHOT_PATH: &str = "/_internal/v1/live-tv/snapshot";
pub(crate) const START_PATH: &str = "/_internal/v1/live-tv/start";
pub(crate) const ACTIVATE_PATH: &str = "/_internal/v1/live-tv/activate";
pub(crate) const RESOURCE_PATH: &str = "/_internal/v1/live-tv/resource";
pub(crate) const STOP_PATH: &str = "/_internal/v1/live-tv/stop";
pub(crate) const DRAIN_PATH: &str = "/_internal/v1/live-tv/drain";
pub(crate) const MAX_INTERNAL_BODY_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_CHANNELS: usize = 512;
const MAX_GUIDE_NUMBER_BYTES: usize = 32;
const MAX_GUIDE_NAME_BYTES: usize = 256;
const MAX_DEVICE_FIELD_BYTES: usize = 256;
const SNAPSHOT_TTL: Duration = Duration::from_secs(30);
const STALE_TTL: Duration = Duration::from_secs(5 * 60);
const GRAPH_PROBE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const GRAPH_PROBE_MIN_INTERVAL: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// Forced refreshes are expensive, signed owner operations. One caller does
/// the work while a small bounded set of followers may wait for its result;
/// additional callers fail promptly instead of building an unbounded queue.
const MAX_FORCED_REFRESH_CALLERS: usize = 8;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const TUNER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PRODUCER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
const CAPABILITY_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const PROVISIONAL_TIMEOUT: Duration = Duration::from_secs(20);
const SESSION_TICK: Duration = Duration::from_millis(250);
const ADMISSION_WAIT: Duration = Duration::from_secs(5);
const MAX_PLAYLIST_BYTES: u64 = 64 * 1024;
const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LISTED_SEGMENTS: usize = 6;
const MAX_DELETION_LAG_SEGMENTS: usize = 1;
const PUMP_CHANNEL_CAPACITY: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTvConfig {
    pub(crate) enabled: bool,
    pub(crate) device_ipv4: Option<Ipv4Addr>,
    pub(crate) owner_node_id: String,
    pub(crate) max_sessions: u8,
    pub(crate) output_height: u16,
    pub(crate) generation: i64,
}

impl LiveTvConfig {
    pub(crate) fn from_snapshot(settings: &BTreeMap<String, String>, local_node_id: &str) -> Self {
        let setting = |key: &str| settings.get(key).map(String::as_str);
        Self {
            enabled: setting(keys::LIVE_TV_ENABLED) == Some("1"),
            device_ipv4: setting(keys::LIVE_TV_DEVICE_IPV4)
                .filter(|value| !value.is_empty())
                .and_then(|value| value.parse().ok()),
            owner_node_id: setting(keys::LIVE_TV_OWNER_NODE_ID)
                .filter(|value| !value.is_empty())
                .unwrap_or(local_node_id)
                .to_owned(),
            max_sessions: setting(keys::LIVE_TV_MAX_SESSIONS)
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
            output_height: setting(keys::LIVE_TV_OUTPUT_HEIGHT)
                .and_then(|value| value.parse().ok())
                .unwrap_or(720),
            generation: setting(keys::LIVE_TV_CONFIG_GENERATION)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        }
    }

    pub(crate) fn validate_static(&self) -> Result<(), LiveTvError> {
        if self.generation < 0 {
            return Err(LiveTvError::InvalidConfig(
                "live-TV configuration generation is invalid".to_owned(),
            ));
        }
        if !(1..=4).contains(&self.max_sessions) {
            return Err(LiveTvError::InvalidConfig(
                "maximum live-TV sessions must be between 1 and 4".to_owned(),
            ));
        }
        if !matches!(self.output_height, 720 | 1080) {
            return Err(LiveTvError::InvalidConfig(
                "live-TV output height must be 720 or 1080".to_owned(),
            ));
        }
        if self.owner_node_id.trim().is_empty() || self.owner_node_id.len() > 256 {
            return Err(LiveTvError::InvalidConfig(
                "live-TV owner node is invalid".to_owned(),
            ));
        }
        if let Some(address) = self.device_ipv4 {
            validate_device_ipv4(address)?;
        } else if self.enabled {
            return Err(LiveTvError::InvalidConfig(
                "an HDHomeRun private IPv4 address is required before enabling".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn parse_device_ipv4(value: &str) -> Result<Option<Ipv4Addr>, LiveTvError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let address = value.parse::<Ipv4Addr>().map_err(|_| {
        LiveTvError::InvalidConfig(
            "HDHomeRun address must be a canonical private IPv4 literal".to_owned(),
        )
    })?;
    if address.to_string() != value {
        return Err(LiveTvError::InvalidConfig(
            "HDHomeRun address must use canonical IPv4 notation".to_owned(),
        ));
    }
    validate_device_ipv4(address)?;
    Ok(Some(address))
}

fn validate_device_ipv4(address: Ipv4Addr) -> Result<(), LiveTvError> {
    if !(address.is_private() || address.is_link_local())
        || address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        || address == Ipv4Addr::new(169, 254, 169, 254)
    {
        return Err(LiveTvError::InvalidConfig(
            "HDHomeRun address must be private or link-local unicast IPv4".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LiveTvDevice {
    pub(crate) device_id: String,
    pub(crate) friendly_name: String,
    pub(crate) model_number: String,
    pub(crate) firmware_version: String,
    pub(crate) tuner_count: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTvChannelSupport {
    Ready,
    DrmUnsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LiveTvChannel {
    pub(crate) id: String,
    pub(crate) guide_number: String,
    pub(crate) guide_name: String,
    pub(crate) favorite: bool,
    pub(crate) drm: bool,
    pub(crate) support: LiveTvChannelSupport,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SnapshotFreshness {
    Fresh,
    Stale,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LiveTvSnapshot {
    pub(crate) generation: i64,
    pub(crate) device: LiveTvDevice,
    pub(crate) channels: Vec<LiveTvChannel>,
    pub(crate) freshness: SnapshotFreshness,
    pub(crate) age_seconds: u64,
    pub(crate) last_success_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_error: Option<String>,
    pub(crate) ffmpeg_graph_ready: bool,
    pub(crate) ffmpeg_graph_message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotRequest {
    pub(crate) generation: i64,
    pub(crate) force: bool,
    /// Only an administrator-facing readiness request may ask the owner to
    /// exercise an encoder. Ordinary lineup readers always send false.
    pub(crate) probe_graph: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveTvError {
    InvalidConfig(String),
    DeviceUnavailable(String),
    InvalidResponse(String),
    OwnerUnavailable(String),
    Disabled(String),
    Capacity(String),
    TunerUnavailable(String),
    ChannelNotFound(String),
    DrmUnsupported(String),
    CodecUnsupported(String),
    StartupTimeout(String),
    StreamFailed(String),
    Conflict(String),
    CapabilityExpired(String),
}

impl std::fmt::Display for LiveTvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidConfig(message)
            | Self::DeviceUnavailable(message)
            | Self::InvalidResponse(message)
            | Self::OwnerUnavailable(message)
            | Self::Disabled(message)
            | Self::Capacity(message)
            | Self::TunerUnavailable(message)
            | Self::ChannelNotFound(message)
            | Self::DrmUnsupported(message)
            | Self::CodecUnsupported(message)
            | Self::StartupTimeout(message)
            | Self::StreamFailed(message)
            | Self::Conflict(message)
            | Self::CapabilityExpired(message) => message,
        };
        formatter.write_str(message)
    }
}

impl LiveTvError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) | Self::InvalidResponse(_) => "invalid_request",
            Self::DeviceUnavailable(_) => "device_unavailable",
            Self::OwnerUnavailable(_) => "owner_unavailable",
            Self::Disabled(_) => "live_tv_disabled",
            Self::Capacity(_) => "tuner_capacity",
            Self::TunerUnavailable(_) => "tuner_unavailable",
            Self::ChannelNotFound(_) => "channel_not_found",
            Self::DrmUnsupported(_) => "drm_unsupported",
            Self::CodecUnsupported(_) => "codec_unsupported",
            Self::StartupTimeout(_) => "startup_timeout",
            Self::StreamFailed(_) => "stream_failed",
            Self::Conflict(_) => "settings_conflict",
            Self::CapabilityExpired(_) => "capability_expired",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvStartRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) source_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) user_name: String,
    pub(crate) request_id: String,
    pub(crate) channel_id: String,
    pub(crate) config_generation: i64,
    pub(crate) source_serving_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvActivateRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) capability: String,
    pub(crate) activation_token: String,
    pub(crate) config_generation: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LiveTvResourceRequest {
    Playlist { capability: String },
    Segment { capability: String, sequence: u64 },
    Status { capability: String },
    Keepalive { capability: String },
}

impl LiveTvResourceRequest {
    pub(crate) fn capability(&self) -> &str {
        match self {
            Self::Playlist { capability }
            | Self::Segment { capability, .. }
            | Self::Status { capability }
            | Self::Keepalive { capability } => capability,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvStopRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) capability: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvDrainRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) keep_generation: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvOutput {
    pub(crate) container: String,
    pub(crate) video: String,
    pub(crate) audio: String,
    pub(crate) height: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTvStartOutcome {
    Created,
    Recovered,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvProvisional {
    pub(crate) outcome: LiveTvStartOutcome,
    pub(crate) session_id: String,
    pub(crate) capability: String,
    pub(crate) activation_token: String,
    pub(crate) channel: LiveTvChannel,
    pub(crate) output: LiveTvOutput,
    pub(crate) config_generation: i64,
    pub(crate) owner_serving_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvActivated {
    pub(crate) session_id: String,
    pub(crate) playlist_url: String,
    pub(crate) channel: LiveTvChannel,
    pub(crate) output: LiveTvOutput,
    pub(crate) live: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LiveTvSessionPhase {
    Starting,
    Provisional,
    Active,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvSessionStatus {
    pub(crate) state: String,
    pub(crate) channel: LiveTvChannel,
    pub(crate) owner_node_id: String,
    pub(crate) encoder: String,
    pub(crate) output_height: u16,
    pub(crate) media_sequence: u64,
    pub(crate) age_seconds: u64,
    pub(crate) idle_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LiveTvActivity {
    pub(crate) channel_number: String,
    pub(crate) channel_name: String,
    pub(crate) user: String,
    pub(crate) owner_node_id: String,
    pub(crate) encoder: String,
    pub(crate) age_seconds: u64,
    pub(crate) output_height: u16,
    pub(crate) state: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct LiveTvRequestKey {
    source_node_id: String,
    user_id: i64,
    request_id: String,
}

impl From<&LiveTvStartRequest> for LiveTvRequestKey {
    fn from(request: &LiveTvStartRequest) -> Self {
        Self {
            source_node_id: request.source_node_id.clone(),
            user_id: request.user_id,
            request_id: request.request_id.clone(),
        }
    }
}

struct LiveTvSessionState {
    phase: LiveTvSessionPhase,
    startup: Option<Result<LiveTvProvisional, LiveTvError>>,
    activated: bool,
    last_touch: tokio::time::Instant,
    last_progress: tokio::time::Instant,
    media_sequence: u64,
    inventory: HashMap<u64, (String, u64)>,
    encoder: String,
    error: Option<String>,
}

struct LiveTvSession {
    capability: String,
    activation_token: String,
    request: LiveTvStartRequest,
    channel: LiveTvChannel,
    output: LiveTvOutput,
    owner_serving_generation: u64,
    started: tokio::time::Instant,
    directory: PathBuf,
    cancel: CancellationToken,
    changed: tokio::sync::Notify,
    state: StdMutex<LiveTvSessionState>,
}

impl LiveTvSession {
    fn status(&self, owner_node_id: &str) -> LiveTvSessionStatus {
        let now = tokio::time::Instant::now();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        LiveTvSessionStatus {
            state: session_phase_name(state.phase).to_owned(),
            channel: self.channel.clone(),
            owner_node_id: owner_node_id.to_owned(),
            encoder: state.encoder.clone(),
            output_height: self.output.height,
            media_sequence: state.media_sequence,
            age_seconds: now.duration_since(self.started).as_secs(),
            idle_seconds: now.duration_since(state.last_touch).as_secs(),
            error: state.error.clone(),
        }
    }
}

#[derive(Default)]
struct LiveTvRegistry {
    sessions: HashMap<String, Arc<LiveTvSession>>,
    requests: HashMap<LiveTvRequestKey, String>,
}

#[derive(Default)]
struct LiveTvMetrics {
    enabled: AtomicBool,
    device_ready: AtomicBool,
    ready_channels: AtomicU64,
    drm_channels: AtomicU64,
    starts_created: AtomicU64,
    starts_recovered: AtomicU64,
    starts_failed: AtomicU64,
    ended: AtomicU64,
    relay_bytes: Arc<AtomicU64>,
}

fn session_phase_name(phase: LiveTvSessionPhase) -> &'static str {
    match phase {
        LiveTvSessionPhase::Starting => "starting",
        LiveTvSessionPhase::Provisional => "provisional",
        LiveTvSessionPhase::Active => "active",
        LiveTvSessionPhase::Failed => "failed",
    }
}

#[derive(Clone)]
struct CachedSnapshot {
    generation: i64,
    observed: tokio::time::Instant,
    snapshot: LiveTvSnapshot,
}

#[derive(Clone)]
struct CompletedSnapshotRefresh {
    generation: i64,
    completed: tokio::time::Instant,
    result: Result<LiveTvSnapshot, LiveTvError>,
}

#[derive(Default)]
struct SnapshotCacheState {
    snapshot: Option<CachedSnapshot>,
    completed_refresh: Option<CompletedSnapshotRefresh>,
}

struct SnapshotCache {
    state: tokio::sync::Mutex<SnapshotCacheState>,
    forced_admission: tokio::sync::Semaphore,
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(SnapshotCacheState::default()),
            forced_admission: tokio::sync::Semaphore::new(MAX_FORCED_REFRESH_CALLERS),
        }
    }
}

impl SnapshotCache {
    async fn get_or_refresh<F, Future>(
        &self,
        generation: i64,
        force: bool,
        refresh: F,
    ) -> Result<LiveTvSnapshot, LiveTvError>
    where
        F: FnOnce() -> Future,
        Future: std::future::Future<Output = Result<LiveTvSnapshot, LiveTvError>>,
    {
        self.get_or_refresh_inner(generation, force, || {}, refresh)
            .await
    }

    async fn get_or_refresh_inner<F, Future, Admitted>(
        &self,
        generation: i64,
        force: bool,
        admitted: Admitted,
        refresh: F,
    ) -> Result<LiveTvSnapshot, LiveTvError>
    where
        F: FnOnce() -> Future,
        Future: std::future::Future<Output = Result<LiveTvSnapshot, LiveTvError>>,
        Admitted: FnOnce(),
    {
        let requested = tokio::time::Instant::now();
        let _forced_permit = if force {
            Some(self.forced_admission.try_acquire().map_err(|_| {
                LiveTvError::DeviceUnavailable(
                    "HDHomeRun refresh is busy; retry shortly".to_owned(),
                )
            })?)
        } else {
            None
        };
        // Kept as an explicit seam so concurrency regressions can prove every
        // follower recorded its request time and acquired bounded admission
        // before the leading device operation is released. Production passes
        // a no-op and pays no synchronization cost.
        admitted();
        let mut state = self.state.lock().await;
        let now = tokio::time::Instant::now();

        // A caller that arrived while another refresh was in flight is a
        // follower, even when it also requested `force`. Reuse the completed
        // result so one burst performs one device operation. This includes a
        // failed result: otherwise a down tuner turns the mutex into a serial
        // retry queue.
        if let Some(completed) = state.completed_refresh.as_ref().filter(|completed| {
            completed.generation == generation && completed.completed >= requested
        }) {
            return completed.result.clone();
        }
        if !force {
            if let Some(cached) = state.snapshot.as_ref().filter(|cached| {
                cached.generation == generation
                    && now.duration_since(cached.observed) <= SNAPSHOT_TTL
            }) {
                return Ok(with_age(cached, now, SnapshotFreshness::Fresh, None));
            }
        }

        let result = match refresh().await {
            Ok(mut snapshot) => {
                let observed = tokio::time::Instant::now();
                snapshot.age_seconds = 0;
                snapshot.freshness = SnapshotFreshness::Fresh;
                snapshot.refresh_error = None;
                state.snapshot = Some(CachedSnapshot {
                    generation,
                    observed,
                    snapshot: snapshot.clone(),
                });
                Ok(snapshot)
            }
            Err(error) => {
                let failed_at = tokio::time::Instant::now();
                state
                    .snapshot
                    .as_ref()
                    .filter(|cached| {
                        cached.generation == generation
                            && failed_at.duration_since(cached.observed) <= STALE_TTL
                    })
                    .map(|cached| {
                        with_age(
                            cached,
                            failed_at,
                            SnapshotFreshness::Stale,
                            Some(sanitize_error(&error.to_string())),
                        )
                    })
                    .ok_or(error)
            }
        };
        state.completed_refresh = Some(CompletedSnapshotRefresh {
            generation,
            completed: tokio::time::Instant::now(),
            result: result.clone(),
        });
        result
    }
}

#[derive(Clone)]
struct CachedGraphProbe {
    height: u16,
    encoder: Encoder,
    observed: tokio::time::Instant,
    result: (bool, String),
}

pub(crate) struct LiveTvManager {
    store: Arc<dyn Store>,
    client: Result<reqwest::Client, String>,
    system: Arc<SystemInfo>,
    transcode: Arc<crate::transcode::TranscodeManager>,
    serving: crate::serving_fence::ServingAuthority,
    node_id: String,
    scratch_root: PathBuf,
    cache: SnapshotCache,
    graph_cache: tokio::sync::Mutex<Option<CachedGraphProbe>>,
    registry: StdMutex<LiveTvRegistry>,
    metrics: LiveTvMetrics,
}

impl LiveTvManager {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        system: Arc<SystemInfo>,
        transcode: Arc<crate::transcode::TranscodeManager>,
        serving: crate::serving_fence::ServingAuthority,
        node_id: String,
        scratch_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(|error| error.to_string()),
            system,
            transcode,
            serving,
            node_id,
            scratch_root,
            cache: SnapshotCache::default(),
            graph_cache: tokio::sync::Mutex::new(None),
            registry: StdMutex::new(LiveTvRegistry::default()),
            metrics: LiveTvMetrics::default(),
        })
    }

    pub(crate) async fn config(&self) -> Result<LiveTvConfig, LiveTvError> {
        let snapshot = self.store.settings_snapshot().await.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        let config = LiveTvConfig::from_snapshot(&snapshot, &self.node_id);
        self.metrics
            .enabled
            .store(config.enabled, Ordering::Release);
        Ok(config)
    }

    pub(crate) async fn local_snapshot(
        &self,
        config: &LiveTvConfig,
        force: bool,
        probe_graph: bool,
    ) -> Result<LiveTvSnapshot, LiveTvError> {
        config.validate_static()?;
        if config.owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "this node is not the configured HDHomeRun owner".to_owned(),
            ));
        }
        let address = config.device_ipv4.ok_or_else(|| {
            LiveTvError::InvalidConfig("an HDHomeRun IPv4 address is required".to_owned())
        })?;
        let snapshot = self
            .cache
            .get_or_refresh(config.generation, force, || {
                self.fetch_snapshot(address, config)
            })
            .await?;
        self.metrics.device_ready.store(true, Ordering::Release);
        self.metrics.ready_channels.store(
            snapshot
                .channels
                .iter()
                .filter(|channel| channel.support == LiveTvChannelSupport::Ready)
                .count() as u64,
            Ordering::Release,
        );
        self.metrics.drm_channels.store(
            snapshot
                .channels
                .iter()
                .filter(|channel| channel.drm)
                .count() as u64,
            Ordering::Release,
        );
        Ok(self
            .with_graph_probe(snapshot, config, force, probe_graph)
            .await)
    }

    async fn fetch_snapshot(
        &self,
        address: Ipv4Addr,
        config: &LiveTvConfig,
    ) -> Result<LiveTvSnapshot, LiveTvError> {
        let client = self.client.as_ref().map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("building HDHomeRun HTTP client: {error}"))
        })?;
        let discover_url = pinned_url(address, 80, "/discover.json")?;
        let discover: DiscoverDocument = fetch_json(client, discover_url).await?;
        let device = discover.validated()?;
        if config.max_sessions > device.tuner_count.min(4) {
            return Err(LiveTvError::InvalidConfig(format!(
                "maximum sessions {} exceeds this device's reported tuner count {}",
                config.max_sessions, device.tuner_count
            )));
        }
        let lineup_url = lineup_url(address, discover.lineup_url.as_deref())?;
        let rows: Vec<serde_json::Value> = fetch_json(client, lineup_url).await?;
        let channels = validate_lineup(rows)?;
        Ok(LiveTvSnapshot {
            generation: config.generation,
            device,
            channels,
            freshness: SnapshotFreshness::Fresh,
            age_seconds: 0,
            last_success_at: unix_seconds(),
            refresh_error: None,
            ffmpeg_graph_ready: false,
            ffmpeg_graph_message:
                "Run the Developer readiness check to test the live-TV FFmpeg graph".to_owned(),
        })
    }

    async fn with_graph_probe(
        &self,
        mut snapshot: LiveTvSnapshot,
        config: &LiveTvConfig,
        force: bool,
        probe_graph: bool,
    ) -> LiveTvSnapshot {
        if probe_graph {
            let (ready, message) = self.probe_graph(config.output_height, force).await;
            snapshot.ffmpeg_graph_ready = ready;
            snapshot.ffmpeg_graph_message = message;
        } else if let Some(cached) = self.graph_cache.lock().await.as_ref() {
            if cached.height == config.output_height {
                snapshot.ffmpeg_graph_ready = cached.result.0;
                snapshot.ffmpeg_graph_message = cached.result.1.clone();
            }
        }
        snapshot
    }

    async fn probe_graph(&self, height: u16, force: bool) -> (bool, String) {
        let encoder = self.system.encoders.choose(&self.system.hwaccel_pref);
        let now = tokio::time::Instant::now();
        let mut cache = self.graph_cache.lock().await;
        if let Some(cached) = cache.as_ref().filter(|cached| {
            cached.height == height
                && cached.encoder == encoder
                && (now.duration_since(cached.observed) <= GRAPH_PROBE_MIN_INTERVAL
                    || (!force && now.duration_since(cached.observed) <= GRAPH_PROBE_TTL))
        }) {
            return cached.result.clone();
        }
        let result = self.run_graph_probe(height, encoder).await;
        *cache = Some(CachedGraphProbe {
            height,
            encoder,
            observed: tokio::time::Instant::now(),
            result: result.clone(),
        });
        result
    }

    async fn run_graph_probe(&self, height: u16, encoder: Encoder) -> (bool, String) {
        if self.system.ffmpeg.trim().is_empty() {
            return (
                false,
                "FFmpeg is not configured on the tuner owner".to_owned(),
            );
        }
        let probe_dir = self
            .scratch_root
            .join(format!("live-tv-readiness-{}", uuid::Uuid::new_v4()));
        if let Err(error) = tokio::fs::create_dir_all(&probe_dir).await {
            return (
                false,
                format!("cannot create live-TV probe scratch: {error}"),
            );
        }
        let result = run_graph_probe(
            &self.system.ffmpeg,
            encoder,
            &self.system,
            height,
            &probe_dir,
        )
        .await;
        let _ = tokio::fs::remove_dir_all(&probe_dir).await;
        match result {
            Ok(()) => (
                true,
                format!(
                    "{} produced H.264/AAC MPEG-TS live HLS at {height}p",
                    encoder.label()
                ),
            ),
            Err(message) => (false, sanitize_error(&message)),
        }
    }

    pub(crate) async fn start_local(
        self: &Arc<Self>,
        request: LiveTvStartRequest,
    ) -> Result<LiveTvProvisional, LiveTvError> {
        validate_start_request(&request)?;
        if request.expected_owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "the start request names a different tuner owner".to_owned(),
            ));
        }
        let serving_generation = self.serving.admit().ok_or_else(|| {
            LiveTvError::OwnerUnavailable(crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned())
        })?;
        let config = self.config().await?;
        validate_start_config(&config, &request, &self.node_id)?;

        let key = LiveTvRequestKey::from(&request);
        if let Some(session) = self.session_for_request(&key, &request)? {
            self.metrics
                .starts_recovered
                .fetch_add(1, Ordering::Relaxed);
            return wait_for_startup(session, true).await;
        }

        // A start never rides the five-minute stale projection. The owner
        // refreshes this exact row immediately before admission so a removed
        // or newly protected channel cannot consume a tuner.
        let snapshot = self.local_snapshot(&config, true, false).await?;
        if snapshot.freshness != SnapshotFreshness::Fresh {
            return Err(LiveTvError::DeviceUnavailable(
                "a fresh HDHomeRun lineup is required to start Live TV".to_owned(),
            ));
        }
        let channel = snapshot
            .channels
            .into_iter()
            .find(|channel| channel.id == request.channel_id)
            .ok_or_else(|| {
                LiveTvError::ChannelNotFound("the selected channel is no longer available".into())
            })?;
        if channel.support == LiveTvChannelSupport::DrmUnsupported || channel.drm {
            return Err(LiveTvError::DrmUnsupported(
                "the selected channel is protected and is not supported by plurx".into(),
            ));
        }
        let address = config.device_ipv4.ok_or_else(|| {
            LiveTvError::InvalidConfig("an HDHomeRun IPv4 address is required".to_owned())
        })?;
        let path = format!("/auto/v{}", channel.guide_number);
        let stream_url = pinned_url(address, 5004, &path)?;
        let random = uuid::Uuid::new_v4().to_string();
        let capability = format!(
            "ltv1.{}.{}",
            base64url_encode(self.node_id.as_bytes()),
            random
        );
        let activation_token = uuid::Uuid::new_v4().to_string();
        let directory = self.scratch_root.join(format!("live-tv-{random}"));
        let now = tokio::time::Instant::now();
        let output = LiveTvOutput {
            container: "hls".to_owned(),
            video: "h264".to_owned(),
            audio: "aac".to_owned(),
            height: config.output_height,
        };
        let session = Arc::new(LiveTvSession {
            capability: capability.clone(),
            activation_token,
            request: request.clone(),
            channel,
            output,
            owner_serving_generation: serving_generation,
            started: now,
            directory,
            cancel: CancellationToken::new(),
            changed: tokio::sync::Notify::new(),
            state: StdMutex::new(LiveTvSessionState {
                phase: LiveTvSessionPhase::Starting,
                startup: None,
                activated: false,
                last_touch: now,
                last_progress: now,
                media_sequence: 0,
                inventory: HashMap::new(),
                encoder: "pending".to_owned(),
                error: None,
            }),
        });

        let raced_session = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A concurrent identical request may have won while the lineup
            // refresh was in flight. Recover it without consuming capacity.
            if let Some(capability) = registry.requests.get(&key) {
                let existing = registry.sessions.get(capability).cloned();
                Some(existing.ok_or_else(|| {
                    LiveTvError::Conflict("live-TV request recovery state is inconsistent".into())
                })?)
            } else {
                if registry.sessions.len() >= usize::from(config.max_sessions) {
                    self.metrics.starts_failed.fetch_add(1, Ordering::Relaxed);
                    return Err(LiveTvError::Capacity(format!(
                        "all {} plurx Live TV session slots are in use",
                        config.max_sessions
                    )));
                }
                registry.requests.insert(key, capability.clone());
                registry.sessions.insert(capability, Arc::clone(&session));
                None
            }
        };
        if let Some(existing) = raced_session {
            if existing.request != request {
                return Err(LiveTvError::Conflict(
                    "the live-TV request id was replayed with different fields".into(),
                ));
            }
            self.metrics
                .starts_recovered
                .fetch_add(1, Ordering::Relaxed);
            return wait_for_startup(existing, true).await;
        }

        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            run_live_session(manager, session, config, stream_url).await;
        });
        self.metrics.starts_created.fetch_add(1, Ordering::Relaxed);
        wait_for_startup(
            self.session_for_request(&LiveTvRequestKey::from(&request), &request)?
                .ok_or_else(|| LiveTvError::StreamFailed("live-TV start disappeared".into()))?,
            false,
        )
        .await
    }

    fn session_for_request(
        &self,
        key: &LiveTvRequestKey,
        request: &LiveTvStartRequest,
    ) -> Result<Option<Arc<LiveTvSession>>, LiveTvError> {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(capability) = registry.requests.get(key) else {
            return Ok(None);
        };
        let session = registry.sessions.get(capability).cloned().ok_or_else(|| {
            LiveTvError::Conflict("live-TV request recovery state is inconsistent".into())
        })?;
        if session.request != *request {
            return Err(LiveTvError::Conflict(
                "the live-TV request id was replayed with different fields".into(),
            ));
        }
        Ok(Some(session))
    }

    pub(crate) async fn activate_local(
        &self,
        request: &LiveTvActivateRequest,
    ) -> Result<LiveTvActivated, LiveTvError> {
        if request.expected_owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "the activation request names a different tuner owner".into(),
            ));
        }
        validate_capability_owner(&request.capability, &self.node_id)?;
        let session = self.session(&request.capability)?;
        if session.activation_token != request.activation_token {
            return Err(LiveTvError::Conflict(
                "the live-TV activation token does not match".into(),
            ));
        }
        if session.request.config_generation != request.config_generation {
            return Err(LiveTvError::Conflict(
                "the live-TV activation generation does not match".into(),
            ));
        }
        let config = self.config().await?;
        validate_start_config(&config, &session.request, &self.node_id)?;
        if !self.serving.is_current(session.owner_serving_generation) {
            session.cancel.cancel();
            return Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            ));
        }
        {
            let mut state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.phase == LiveTvSessionPhase::Failed {
                return Err(LiveTvError::StreamFailed(
                    state
                        .error
                        .clone()
                        .unwrap_or_else(|| "the live-TV stream ended".into()),
                ));
            }
            if !matches!(
                state.phase,
                LiveTvSessionPhase::Provisional | LiveTvSessionPhase::Active
            ) {
                return Err(LiveTvError::Conflict(
                    "the live-TV session is not ready for activation".into(),
                ));
            }
            state.activated = true;
            state.phase = LiveTvSessionPhase::Active;
            state.last_touch = tokio::time::Instant::now();
        }
        Ok(LiveTvActivated {
            session_id: session.capability.clone(),
            playlist_url: format!("/api/v1/live-tv/sessions/{}/index.m3u8", session.capability),
            channel: session.channel.clone(),
            output: session.output.clone(),
            live: true,
        })
    }

    pub(crate) async fn resource_local(
        &self,
        request: LiveTvResourceRequest,
    ) -> Result<Response<Body>, LiveTvError> {
        validate_capability_owner(request.capability(), &self.node_id)?;
        let session = self.session(request.capability())?;
        {
            let mut state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.activated || state.phase != LiveTvSessionPhase::Active {
                return Err(LiveTvError::CapabilityExpired(
                    "the live-TV capability is not active".into(),
                ));
            }
            state.last_touch = tokio::time::Instant::now();
        }
        match request {
            LiveTvResourceRequest::Playlist { .. } => {
                let bytes = read_bounded_regular_file(
                    &session.directory.join("index.m3u8"),
                    MAX_PLAYLIST_BYTES,
                )
                .await?;
                parse_playlist_bytes(&bytes)?;
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(Body::from(bytes))
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Segment { sequence, .. } => {
                let (name, expected_len) = {
                    let state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.inventory.get(&sequence).cloned().ok_or_else(|| {
                        LiveTvError::CapabilityExpired(
                            "the requested live segment is outside the current window".into(),
                        )
                    })?
                };
                let path = session.directory.join(&name);
                let metadata = tokio::fs::symlink_metadata(&path)
                    .await
                    .map_err(|_| LiveTvError::CapabilityExpired("live segment expired".into()))?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() != expected_len
                    || expected_len > MAX_SESSION_BYTES
                {
                    return Err(LiveTvError::StreamFailed(
                        "live segment failed its bounded inventory check".into(),
                    ));
                }
                let file = tokio::fs::File::open(path)
                    .await
                    .map_err(|_| LiveTvError::CapabilityExpired("live segment expired".into()))?;
                let stream = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(
                    file,
                    expected_len,
                ));
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "video/mp2t")
                    .header(header::CACHE_CONTROL, "no-store")
                    .header(header::CONTENT_LENGTH, expected_len)
                    .body(Body::from_stream(stream))
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Status { .. } => {
                let body = serde_json::to_vec(&session.status(&self.node_id))
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))?;
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(Body::from(body))
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Keepalive { .. } => Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .map_err(|error| LiveTvError::StreamFailed(error.to_string())),
        }
    }

    pub(crate) fn stop_local(&self, capability: &str) -> Result<(), LiveTvError> {
        validate_capability_owner(capability, &self.node_id)?;
        let session = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(capability)
            .cloned();
        if let Some(session) = session {
            session.cancel.cancel();
        }
        Ok(())
    }

    pub(crate) fn drain_stale(&self, keep_generation: i64) -> usize {
        let sessions = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .values()
            .filter(|session| session.request.config_generation != keep_generation)
            .cloned()
            .collect::<Vec<_>>();
        for session in &sessions {
            session.cancel.cancel();
        }
        sessions.len()
    }

    pub(crate) fn activities(&self) -> Vec<LiveTvActivity> {
        let now = tokio::time::Instant::now();
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .values()
            .map(|session| {
                let state = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                LiveTvActivity {
                    channel_number: session.channel.guide_number.clone(),
                    channel_name: session.channel.guide_name.clone(),
                    user: session.request.user_name.clone(),
                    owner_node_id: self.node_id.clone(),
                    encoder: state.encoder.clone(),
                    age_seconds: now.duration_since(session.started).as_secs(),
                    output_height: session.output.height,
                    state: session_phase_name(state.phase).to_owned(),
                }
            })
            .collect()
    }

    pub(crate) fn prometheus(&self) -> String {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut starting = 0usize;
        let mut active = 0usize;
        for session in registry.sessions.values() {
            let phase = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .phase;
            if phase == LiveTvSessionPhase::Active {
                active += 1;
            } else if matches!(
                phase,
                LiveTvSessionPhase::Starting | LiveTvSessionPhase::Provisional
            ) {
                starting += 1;
            }
        }
        format!(
            "# HELP plurx_live_tv_enabled Whether runtime Live TV is enabled.\n\
             # TYPE plurx_live_tv_enabled gauge\n\
             plurx_live_tv_enabled {}\n\
             # HELP plurx_live_tv_device_ready Whether the configured owner last proved the device.\n\
             # TYPE plurx_live_tv_device_ready gauge\n\
             plurx_live_tv_device_ready {}\n\
             # HELP plurx_live_tv_lineup_channels Sanitized lineup channels by support.\n\
             # TYPE plurx_live_tv_lineup_channels gauge\n\
             plurx_live_tv_lineup_channels{{support=\"ready\"}} {}\n\
             plurx_live_tv_lineup_channels{{support=\"drm_unsupported\"}} {}\n\
             # HELP plurx_live_tv_sessions Process-local Live TV sessions by state.\n\
             # TYPE plurx_live_tv_sessions gauge\n\
             plurx_live_tv_sessions{{state=\"starting\"}} {starting}\n\
             plurx_live_tv_sessions{{state=\"active\"}} {active}\n\
             # HELP plurx_live_tv_starts_total Live TV starts by outcome.\n\
             # TYPE plurx_live_tv_starts_total counter\n\
             plurx_live_tv_starts_total{{outcome=\"created\"}} {}\n\
             plurx_live_tv_starts_total{{outcome=\"recovered\"}} {}\n\
             plurx_live_tv_starts_total{{outcome=\"failed\"}} {}\n\
             # HELP plurx_live_tv_session_ends_total Live TV session owners released.\n\
             # TYPE plurx_live_tv_session_ends_total counter\n\
             plurx_live_tv_session_ends_total{{reason=\"terminal\"}} {}\n\
             # HELP plurx_live_tv_relay_bytes_total Bytes relayed for Live TV resources.\n\
             # TYPE plurx_live_tv_relay_bytes_total counter\n\
             plurx_live_tv_relay_bytes_total {}\n",
            u8::from(self.metrics.enabled.load(Ordering::Acquire)),
            u8::from(self.metrics.device_ready.load(Ordering::Acquire)),
            self.metrics.ready_channels.load(Ordering::Acquire),
            self.metrics.drm_channels.load(Ordering::Acquire),
            self.metrics.starts_created.load(Ordering::Acquire),
            self.metrics.starts_recovered.load(Ordering::Acquire),
            self.metrics.starts_failed.load(Ordering::Acquire),
            self.metrics.ended.load(Ordering::Acquire),
            self.metrics.relay_bytes.load(Ordering::Acquire),
        )
    }

    pub(crate) fn relay_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.metrics.relay_bytes)
    }

    fn session(&self, capability: &str) -> Result<Arc<LiveTvSession>, LiveTvError> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(capability)
            .cloned()
            .ok_or_else(|| LiveTvError::CapabilityExpired("live-TV capability expired".into()))
    }
}

impl Drop for LiveTvManager {
    fn drop(&mut self) {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for session in registry.sessions.values() {
            session.cancel.cancel();
        }
    }
}

#[derive(Debug)]
struct ScratchInventory {
    media_sequence: u64,
    segments: HashMap<u64, (String, u64)>,
}

async fn wait_for_startup(
    session: Arc<LiveTvSession>,
    recovered: bool,
) -> Result<LiveTvProvisional, LiveTvError> {
    let deadline = session.started + STARTUP_TIMEOUT + Duration::from_secs(2);
    loop {
        let notified = session.changed.notified();
        if let Some(result) = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .startup
            .clone()
        {
            return result.map(|mut provisional| {
                if recovered {
                    provisional.outcome = LiveTvStartOutcome::Recovered;
                }
                provisional
            });
        }
        tokio::time::timeout_at(deadline, notified)
            .await
            .map_err(|_| {
                LiveTvError::StartupTimeout(
                    "the tuner did not publish the first live segment in time".into(),
                )
            })?;
    }
}

fn validate_start_request(request: &LiveTvStartRequest) -> Result<(), LiveTvError> {
    let request_id = uuid::Uuid::parse_str(&request.request_id)
        .map_err(|_| LiveTvError::InvalidResponse("live-TV request id is invalid".into()))?;
    if request_id.get_version_num() != 4
        || request.source_node_id.trim().is_empty()
        || request.source_node_id.len() > 256
        || request.user_name.trim().is_empty()
        || request.user_name.len() > 256
        || request.user_id <= 0
        || !valid_guide_number(&request.channel_id)
    {
        return Err(LiveTvError::InvalidResponse(
            "live-TV start fields are invalid".into(),
        ));
    }
    Ok(())
}

fn validate_start_config(
    config: &LiveTvConfig,
    request: &LiveTvStartRequest,
    local_node_id: &str,
) -> Result<(), LiveTvError> {
    config.validate_static()?;
    if !config.enabled {
        return Err(LiveTvError::Disabled(
            "Live TV is disabled in Settings → Developer".into(),
        ));
    }
    if config.owner_node_id != local_node_id
        || config.owner_node_id != request.expected_owner_node_id
    {
        return Err(LiveTvError::OwnerUnavailable(
            "this node is not the current HDHomeRun owner".into(),
        ));
    }
    if config.generation != request.config_generation {
        return Err(LiveTvError::Conflict(
            "the live-TV configuration changed; reload channels".into(),
        ));
    }
    Ok(())
}

async fn run_live_session(
    manager: Weak<LiveTvManager>,
    session: Arc<LiveTvSession>,
    config: LiveTvConfig,
    stream_url: reqwest::Url,
) {
    let result = run_live_session_inner(&manager, &session, &config, stream_url).await;
    session.cancel.cancel();
    if let Err(error) = result {
        if let Some(manager) = manager.upgrade() {
            manager
                .metrics
                .starts_failed
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut state = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.startup.is_none() {
            state.startup = Some(Err(error.clone()));
        }
        state.phase = LiveTvSessionPhase::Failed;
        state.error = Some(sanitize_error(&error.to_string()));
        drop(state);
        session.changed.notify_waiters();
    }
    let _ = tokio::fs::remove_dir_all(&session.directory).await;
    if let Some(manager) = manager.upgrade() {
        let mut registry = manager
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.sessions.remove(&session.capability);
        registry
            .requests
            .retain(|_, capability| capability != &session.capability);
        manager.metrics.ended.fetch_add(1, Ordering::Relaxed);
    }
}

async fn run_live_session_inner(
    manager: &Weak<LiveTvManager>,
    session: &Arc<LiveTvSession>,
    config: &LiveTvConfig,
    stream_url: reqwest::Url,
) -> Result<(), LiveTvError> {
    let owner = manager
        .upgrade()
        .ok_or_else(|| LiveTvError::StreamFailed("live-TV manager stopped".into()))?;
    let admission = owner
        .transcode
        .admit_live_tv(config.output_height, ADMISSION_WAIT)
        .await
        .map_err(LiveTvError::Capacity)?;
    {
        let mut state = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.encoder = admission.encoder.label().to_owned();
    }
    ensure_session_fence(&owner, session).await?;
    tokio::fs::create_dir_all(&session.directory)
        .await
        .map_err(|error| LiveTvError::StreamFailed(format!("creating live-TV scratch: {error}")))?;

    let client = owner.client.as_ref().map_err(|_| {
        LiveTvError::DeviceUnavailable("the HDHomeRun HTTP client is unavailable".into())
    })?;
    let startup_deadline = session.started + STARTUP_TIMEOUT;
    let response = open_tuner_stream(client, stream_url, startup_deadline).await?;
    let mut child = spawn_live_ffmpeg(
        &owner.system,
        admission.encoder,
        admission.software_threads(),
        config.output_height,
        &session.directory,
    )?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| LiveTvError::StreamFailed("live-TV FFmpeg did not expose stdin".into()))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut stderr = stderr;
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });
    }
    let pump = tokio::spawn(pump_tuner_stream(response, stdin, session.cancel.clone()));
    drop(owner);

    let mut tick = tokio::time::interval(SESSION_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0u64;
    let mut published = false;
    let terminal = loop {
        tick.tick().await;
        if session.cancel.is_cancelled() {
            break Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| LiveTvError::StreamFailed(format!("waiting for FFmpeg: {error}")))?
        {
            break Err(LiveTvError::StreamFailed(format!(
                "live-TV FFmpeg exited with {status}"
            )));
        }
        if pump.is_finished() {
            break Err(LiveTvError::StreamFailed(
                "the tuner stream ended unexpectedly".into(),
            ));
        }

        match inspect_scratch(&session.directory).await {
            Ok(Some(inventory)) => {
                let now = tokio::time::Instant::now();
                {
                    let mut state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if inventory.media_sequence > state.media_sequence || state.inventory.is_empty()
                    {
                        state.last_progress = now;
                    }
                    state.media_sequence = inventory.media_sequence;
                    state.inventory = inventory.segments;
                }
                if !published {
                    let owner = manager.upgrade().ok_or_else(|| {
                        LiveTvError::StreamFailed("live-TV manager stopped".into())
                    })?;
                    ensure_session_fence(&owner, session).await?;
                    let provisional = LiveTvProvisional {
                        outcome: LiveTvStartOutcome::Created,
                        session_id: session.capability.clone(),
                        capability: session.capability.clone(),
                        activation_token: session.activation_token.clone(),
                        channel: session.channel.clone(),
                        output: session.output.clone(),
                        config_generation: session.request.config_generation,
                        owner_serving_generation: session.owner_serving_generation,
                    };
                    let mut state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.phase = LiveTvSessionPhase::Provisional;
                    state.startup = Some(Ok(provisional));
                    drop(state);
                    session.changed.notify_waiters();
                    published = true;
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }

        let now = tokio::time::Instant::now();
        if !published && now >= startup_deadline {
            break Err(LiveTvError::StartupTimeout(
                "the tuner did not publish the first live segment in time".into(),
            ));
        }
        {
            let state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if published
                && !state.activated
                && now.duration_since(session.started) >= PROVISIONAL_TIMEOUT
            {
                break Err(LiveTvError::CapabilityExpired(
                    "the provisional live-TV start was not activated".into(),
                ));
            }
            if state.activated && now.duration_since(state.last_touch) >= CAPABILITY_IDLE_TIMEOUT {
                break Err(LiveTvError::CapabilityExpired(
                    "the live-TV capability became idle".into(),
                ));
            }
            if published && now.duration_since(state.last_progress) >= PRODUCER_PROGRESS_TIMEOUT {
                break Err(LiveTvError::StreamFailed(
                    "the live-TV producer stopped advancing".into(),
                ));
            }
        }
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(4) {
            let owner = manager
                .upgrade()
                .ok_or_else(|| LiveTvError::StreamFailed("live-TV manager stopped".into()))?;
            ensure_session_fence(&owner, session).await?;
        }
    };

    session.cancel.cancel();
    pump.abort();
    let _ = pump.await;
    if child
        .try_wait()
        .map_err(|error| LiveTvError::StreamFailed(format!("waiting for FFmpeg: {error}")))?
        .is_none()
    {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
    drop(admission);
    terminal
}

async fn ensure_session_fence(
    manager: &LiveTvManager,
    session: &LiveTvSession,
) -> Result<(), LiveTvError> {
    if !manager.serving.is_current(session.owner_serving_generation) {
        return Err(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
        ));
    }
    let config = manager.config().await?;
    validate_start_config(&config, &session.request, &manager.node_id)
}

async fn open_tuner_stream(
    client: &reqwest::Client,
    url: reqwest::Url,
    deadline: tokio::time::Instant,
) -> Result<reqwest::Response, LiveTvError> {
    let response = tokio::time::timeout_at(deadline, client.get(url).send())
        .await
        .map_err(|_| LiveTvError::StartupTimeout("the HDHomeRun stream headers timed out".into()))?
        .map_err(|error| {
            tracing::warn!(error = %error, "HDHomeRun stream request failed on the tuner owner");
            LiveTvError::DeviceUnavailable("the HDHomeRun stream request failed".into())
        })?;
    match response.status() {
        reqwest::StatusCode::OK => Ok(response),
        reqwest::StatusCode::NOT_FOUND => Err(LiveTvError::ChannelNotFound(
            "the HDHomeRun no longer recognizes this channel".into(),
        )),
        reqwest::StatusCode::SERVICE_UNAVAILABLE => Err(LiveTvError::TunerUnavailable(
            "the HDHomeRun could not start this channel; a tuner, signal, or authorization may be unavailable".into(),
        )),
        status => Err(LiveTvError::DeviceUnavailable(format!(
            "the HDHomeRun returned HTTP {} for stream startup",
            status.as_u16()
        ))),
    }
}

fn spawn_live_ffmpeg(
    system: &SystemInfo,
    encoder: Encoder,
    software_threads: Option<u32>,
    height: u16,
    directory: &Path,
) -> Result<tokio::process::Child, LiveTvError> {
    if system.ffmpeg.trim().is_empty() {
        return Err(LiveTvError::CodecUnsupported(
            "FFmpeg is not configured on the tuner owner".into(),
        ));
    }
    let playlist = directory.join("index.m3u8");
    let segments = directory.join("segment-%06d.ts");
    let mut command = tokio::process::Command::new(&system.ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-nostdin", "-y"])
        .args(encoder.init_args())
        .args([
            "-fflags",
            "+genpts+discardcorrupt",
            "-probesize",
            "8388608",
            "-analyzeduration",
            "5000000",
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-sn",
            "-dn",
        ]);
    let mut filter = format!("bwdif=mode=send_frame,scale=-2:{height}");
    if let Some(suffix) = encoder.filter_suffix() {
        filter.push(',');
        filter.push_str(suffix);
    }
    command.args(["-vf", &filter]);
    command.args(encoder.encode_args(
        if height == 1080 { 8_000 } else { 4_000 },
        EffectiveRateControl::Vbr,
        system.encoders.forced_idr.wanted_by(encoder),
        software_threads,
    ));
    command.args([
        "-force_key_frames",
        "expr:gte(t,n_forced*4)",
        "-g",
        "120",
        "-keyint_min",
        "120",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ac",
        "2",
        "-f",
        "hls",
        "-hls_time",
        "4",
        "-hls_list_size",
        "6",
        "-hls_delete_threshold",
        "1",
        "-hls_flags",
        "delete_segments+temp_file+independent_segments+omit_endlist",
        "-hls_segment_filename",
    ]);
    command
        .arg(segments)
        .arg(playlist)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| LiveTvError::CodecUnsupported(format!("starting live-TV FFmpeg: {error}")))
}

async fn pump_tuner_stream(
    response: reqwest::Response,
    mut stdin: tokio::process::ChildStdin,
    cancel: CancellationToken,
) -> Result<(), LiveTvError> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(PUMP_CHANNEL_CAPACITY);
    let reader_cancel = cancel.clone();
    let reader = tokio::spawn(async move {
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! {
                _ = reader_cancel.cancelled() => return Ok::<(), LiveTvError>(()),
                next = tokio::time::timeout(TUNER_READ_TIMEOUT, stream.next()) => next,
            }
            .map_err(|_| {
                LiveTvError::StreamFailed("the HDHomeRun stream stopped producing bytes".into())
            })?;
            match next {
                Some(Ok(bytes)) => sender.send(bytes).await.map_err(|_| {
                    LiveTvError::StreamFailed("the FFmpeg input pump stopped".into())
                })?,
                Some(Err(error)) => {
                    tracing::warn!(error = %error, "HDHomeRun stream body failed on the tuner owner");
                    return Err(LiveTvError::StreamFailed(
                        "the HDHomeRun stream body failed".into(),
                    ));
                }
                None => {
                    return Err(LiveTvError::StreamFailed(
                        "the HDHomeRun stream ended".into(),
                    ));
                }
            }
        }
    });
    let writer_result = loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            next = tokio::time::timeout(TUNER_READ_TIMEOUT, receiver.recv()) => next,
        }
        .map_err(|_| LiveTvError::StreamFailed("FFmpeg stopped accepting tuner bytes".into()))?;
        let Some(bytes) = next else {
            break Err(LiveTvError::StreamFailed(
                "the HDHomeRun stream pump ended".into(),
            ));
        };
        tokio::time::timeout(TUNER_READ_TIMEOUT, stdin.write_all(&bytes))
            .await
            .map_err(|_| LiveTvError::StreamFailed("FFmpeg input stalled".into()))?
            .map_err(|error| LiveTvError::StreamFailed(format!("writing FFmpeg input: {error}")))?;
    };
    reader.abort();
    let reader_result = reader.await;
    drop(stdin);
    match (writer_result, reader_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(Err(error))) => Err(error),
        (Ok(()), _) => Ok(()),
    }
}

async fn inspect_scratch(directory: &Path) -> Result<Option<ScratchInventory>, LiveTvError> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LiveTvError::StreamFailed(format!(
                "reading live-TV scratch: {error}"
            )))
        }
    };
    let mut total = 0u64;
    let mut final_segments = HashMap::new();
    let mut temporary_count = 0usize;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| LiveTvError::StreamFailed(format!("reading live-TV scratch: {error}")))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = tokio::fs::symlink_metadata(entry.path())
            .await
            .map_err(|error| {
                LiveTvError::StreamFailed(format!("checking live-TV scratch: {error}"))
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LiveTvError::StreamFailed(
                "live-TV scratch contains a non-regular entry".into(),
            ));
        }
        total = total.saturating_add(metadata.len());
        if total > MAX_SESSION_BYTES {
            return Err(LiveTvError::StreamFailed(
                "live-TV scratch exceeded 128 MiB".into(),
            ));
        }
        match name.as_str() {
            "index.m3u8" => {
                if metadata.len() > MAX_PLAYLIST_BYTES {
                    return Err(LiveTvError::StreamFailed(
                        "live-TV playlist exceeded its byte budget".into(),
                    ));
                }
            }
            "index.m3u8.tmp" => temporary_count += 1,
            _ => {
                if let Some(sequence) = parse_segment_name(&name, false) {
                    final_segments.insert(sequence, (name, metadata.len()));
                } else if parse_segment_name(&name, true).is_some() {
                    temporary_count += 1;
                } else {
                    return Err(LiveTvError::StreamFailed(
                        "live-TV scratch contains an unexpected filename".into(),
                    ));
                }
            }
        }
    }
    if temporary_count > 1 {
        return Err(LiveTvError::StreamFailed(
            "live-TV scratch contains too many temporary files".into(),
        ));
    }
    let playlist =
        match read_bounded_regular_file(&directory.join("index.m3u8"), MAX_PLAYLIST_BYTES).await {
            Ok(playlist) => playlist,
            Err(LiveTvError::CapabilityExpired(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
    let parsed = parse_playlist_bytes(&playlist)?;
    if parsed.segments.is_empty() {
        return Ok(None);
    }
    let mut inventory = HashMap::new();
    for sequence in &parsed.segments {
        let Some((name, len)) = final_segments.get(sequence).cloned() else {
            return Ok(None);
        };
        if len == 0 {
            return Ok(None);
        }
        inventory.insert(*sequence, (name, len));
    }
    let unlisted = final_segments
        .keys()
        .filter(|sequence| !inventory.contains_key(sequence))
        .count();
    if inventory.len() > MAX_LISTED_SEGMENTS || unlisted > MAX_DELETION_LAG_SEGMENTS {
        return Err(LiveTvError::StreamFailed(
            "live-TV scratch exceeded its segment inventory budget".into(),
        ));
    }
    Ok(Some(ScratchInventory {
        media_sequence: parsed.media_sequence,
        segments: inventory,
    }))
}

struct ParsedPlaylist {
    media_sequence: u64,
    segments: Vec<u64>,
}

fn parse_playlist_bytes(bytes: &[u8]) -> Result<ParsedPlaylist, LiveTvError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LiveTvError::StreamFailed("live-TV playlist is not UTF-8".into()))?;
    if text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        != Some("#EXTM3U")
    {
        return Err(LiveTvError::StreamFailed(
            "live-TV playlist is missing its HLS header".into(),
        ));
    }
    if text.contains("#EXT-X-ENDLIST") {
        return Err(LiveTvError::StreamFailed(
            "live-TV playlist unexpectedly ended".into(),
        ));
    }
    let mut media_sequence = None;
    let mut segments = Vec::new();
    let mut expect_segment = false;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            if media_sequence.is_some() {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist repeats its media sequence".into(),
                ));
            }
            media_sequence = Some(value.trim().parse::<u64>().map_err(|_| {
                LiveTvError::StreamFailed("live-TV media sequence is invalid".into())
            })?);
        } else if line.starts_with("#EXTINF:") {
            if expect_segment {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist has a segment without a resource".into(),
                ));
            }
            expect_segment = true;
        } else if !line.is_empty() && !line.starts_with('#') {
            if !expect_segment {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist contains an unbound resource".into(),
                ));
            }
            let sequence = parse_segment_name(line, false).ok_or_else(|| {
                LiveTvError::StreamFailed("live-TV playlist contains an invalid segment".into())
            })?;
            if segments
                .last()
                .is_some_and(|previous| *previous >= sequence)
            {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist segment order is invalid".into(),
                ));
            }
            segments.push(sequence);
            expect_segment = false;
        }
    }
    if expect_segment {
        return Err(LiveTvError::StreamFailed(
            "live-TV playlist has a segment without a resource".into(),
        ));
    }
    if segments.len() > MAX_LISTED_SEGMENTS {
        return Err(LiveTvError::StreamFailed(
            "live-TV playlist exceeds six segments".into(),
        ));
    }
    let media_sequence = match (media_sequence, segments.first().copied()) {
        (Some(sequence), _) => sequence,
        (None, None) => 0,
        (None, Some(_)) => {
            return Err(LiveTvError::StreamFailed(
                "live-TV playlist is missing its media sequence".into(),
            ))
        }
    };
    if segments
        .first()
        .is_some_and(|first| *first != media_sequence)
    {
        return Err(LiveTvError::StreamFailed(
            "live-TV media sequence does not match its first segment".into(),
        ));
    }
    Ok(ParsedPlaylist {
        media_sequence,
        segments,
    })
}

fn parse_segment_name(name: &str, temporary: bool) -> Option<u64> {
    let stem = if temporary {
        name.strip_suffix(".ts.tmp")?
    } else {
        name.strip_suffix(".ts")?
    };
    let digits = stem.strip_prefix("segment-")?;
    if digits.is_empty() || digits.len() > 10 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

async fn read_bounded_regular_file(path: &Path, max: u64) -> Result<Vec<u8>, LiveTvError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| LiveTvError::CapabilityExpired("live-TV resource expired".into()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max {
        return Err(LiveTvError::StreamFailed(
            "live-TV resource failed its bounded file check".into(),
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| LiveTvError::CapabilityExpired("live-TV resource expired".into()))?;
    if bytes.len() as u64 > max {
        return Err(LiveTvError::StreamFailed(
            "live-TV resource exceeded its byte budget".into(),
        ));
    }
    Ok(bytes)
}

pub(crate) fn capability_owner(capability: &str) -> Result<String, LiveTvError> {
    let mut parts = capability.split('.');
    if parts.next() != Some("ltv1") {
        return Err(LiveTvError::CapabilityExpired(
            "live-TV capability is invalid".into(),
        ));
    }
    let encoded = parts
        .next()
        .ok_or_else(|| LiveTvError::CapabilityExpired("live-TV capability is invalid".into()))?;
    let random = parts
        .next()
        .ok_or_else(|| LiveTvError::CapabilityExpired("live-TV capability is invalid".into()))?;
    if parts.next().is_some()
        || uuid::Uuid::parse_str(random)
            .ok()
            .is_none_or(|uuid| uuid.get_version_num() != 4)
    {
        return Err(LiveTvError::CapabilityExpired(
            "live-TV capability is invalid".into(),
        ));
    }
    let bytes = base64url_decode(encoded)
        .ok_or_else(|| LiveTvError::CapabilityExpired("live-TV capability is invalid".into()))?;
    let owner = String::from_utf8(bytes)
        .map_err(|_| LiveTvError::CapabilityExpired("live-TV capability is invalid".into()))?;
    if owner.trim().is_empty() || owner.len() > 256 || base64url_encode(owner.as_bytes()) != encoded
    {
        return Err(LiveTvError::CapabilityExpired(
            "live-TV capability is invalid".into(),
        ));
    }
    Ok(owner)
}

fn validate_capability_owner(capability: &str, expected: &str) -> Result<(), LiveTvError> {
    if capability_owner(capability)? == expected {
        Ok(())
    } else {
        Err(LiveTvError::CapabilityExpired(
            "live-TV capability names a different owner".into(),
        ))
    }
}

fn base64url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 63) as usize] as char);
        output.push(TABLE[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[((value >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(TABLE[(value & 63) as usize] as char);
        }
    }
    output
}

fn base64url_decode(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 4 == 1 || value.bytes().any(|byte| byte == b'=') {
        return None;
    }
    let decode = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    };
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    for chunk in value.as_bytes().chunks(4) {
        let a = u32::from(decode(chunk[0])?);
        let b = u32::from(decode(*chunk.get(1)?)?);
        let c = match chunk.get(2) {
            Some(byte) => Some(u32::from(decode(*byte)?)),
            None => None,
        };
        let d = match chunk.get(3) {
            Some(byte) => Some(u32::from(decode(*byte)?)),
            None => None,
        };
        output.push(((a << 2) | (b >> 4)) as u8);
        if let Some(c) = c {
            output.push(((b << 4) | (c >> 2)) as u8);
            if let Some(d) = d {
                output.push(((c << 6) | d) as u8);
            }
        }
    }
    Some(output)
}

fn with_age(
    cached: &CachedSnapshot,
    now: tokio::time::Instant,
    freshness: SnapshotFreshness,
    refresh_error: Option<String>,
) -> LiveTvSnapshot {
    let mut snapshot = cached.snapshot.clone();
    snapshot.age_seconds = now.duration_since(cached.observed).as_secs();
    snapshot.freshness = freshness;
    snapshot.refresh_error = refresh_error;
    snapshot
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DiscoverDocument {
    friendly_name: Option<String>,
    model_number: Option<String>,
    firmware_version: Option<String>,
    #[serde(rename = "DeviceID")]
    device_id: Option<String>,
    tuner_count: Option<u8>,
    #[serde(rename = "BaseURL")]
    base_url: Option<String>,
    #[serde(rename = "LineupURL")]
    lineup_url: Option<String>,
    #[allow(dead_code)]
    device_auth: Option<String>,
}

impl DiscoverDocument {
    fn validated(&self) -> Result<LiveTvDevice, LiveTvError> {
        if let Some(base) = self.base_url.as_deref() {
            validate_advertised_url(base, &["/"], &[80, 5004])?;
        }
        Ok(LiveTvDevice {
            device_id: required_field("DeviceID", self.device_id.as_deref())?,
            friendly_name: required_field("FriendlyName", self.friendly_name.as_deref())?,
            model_number: required_field("ModelNumber", self.model_number.as_deref())?,
            firmware_version: required_field("FirmwareVersion", self.firmware_version.as_deref())?,
            tuner_count: self
                .tuner_count
                .filter(|count| (1..=16).contains(count))
                .ok_or_else(|| {
                    LiveTvError::InvalidResponse(
                        "HDHomeRun discovery returned an invalid tuner count".to_owned(),
                    )
                })?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LineupDocument {
    guide_number: Option<String>,
    guide_name: Option<String>,
    tags: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
}

fn top_level_lineup_marker(value: Option<&serde_json::Value>, fail_closed: bool) -> bool {
    match value {
        None => false,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Number(value)) if value.as_u64() == Some(0) => false,
        Some(serde_json::Value::Number(value)) if value.as_u64() == Some(1) => true,
        // Firmware fields that decide whether plurx may open a tuner are not
        // permissively coerced. An unfamiliar DRM marker means protected;
        // an unfamiliar Favorite marker is merely not a favorite.
        Some(_) => fail_closed,
    }
}

fn validate_lineup(rows: Vec<serde_json::Value>) -> Result<Vec<LiveTvChannel>, LiveTvError> {
    if rows.len() > MAX_CHANNELS {
        return Err(LiveTvError::InvalidResponse(format!(
            "HDHomeRun lineup exceeds {MAX_CHANNELS} channels"
        )));
    }
    let mut seen = HashSet::with_capacity(rows.len());
    let mut channels = Vec::with_capacity(rows.len());
    for raw_row in rows {
        let top_level_drm = top_level_lineup_marker(raw_row.get("DRM"), true);
        let top_level_favorite = top_level_lineup_marker(raw_row.get("Favorite"), false);
        let Ok(row) = serde_json::from_value::<LineupDocument>(raw_row) else {
            continue;
        };
        let Some(guide_number) = row.guide_number.as_deref() else {
            continue;
        };
        let Some(guide_name) = row.guide_name.as_deref() else {
            continue;
        };
        if !valid_guide_number(guide_number)
            || guide_name.trim().is_empty()
            || guide_name.len() > MAX_GUIDE_NAME_BYTES
            || guide_name.chars().any(char::is_control)
        {
            continue;
        }
        if !seen.insert(guide_number.to_owned()) {
            return Err(LiveTvError::InvalidResponse(format!(
                "HDHomeRun lineup contains duplicate guide number {guide_number}"
            )));
        }
        if let Some(url) = row.url.as_deref() {
            validate_advertised_url(url, &[&format!("/auto/v{guide_number}")], &[80, 5004])?;
        }
        let tags = row
            .tags
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        if tags
            .iter()
            .any(|tag| tag.len() > 64 || tag.chars().any(char::is_control))
        {
            continue;
        }
        let favorite =
            top_level_favorite || tags.iter().any(|tag| tag.eq_ignore_ascii_case("favorite"));
        let drm = top_level_drm || tags.iter().any(|tag| tag.eq_ignore_ascii_case("drm"));
        channels.push(LiveTvChannel {
            id: guide_number.to_owned(),
            guide_number: guide_number.to_owned(),
            guide_name: guide_name.trim().to_owned(),
            favorite,
            drm,
            support: if drm {
                LiveTvChannelSupport::DrmUnsupported
            } else {
                LiveTvChannelSupport::Ready
            },
        });
    }
    channels.sort_by(|left, right| guide_order(&left.guide_number, &right.guide_number));
    Ok(channels)
}

fn guide_order(left: &str, right: &str) -> std::cmp::Ordering {
    let number = |value: &str| {
        let mut parts = value.split('.');
        (
            parts.next().and_then(|part| part.parse::<u16>().ok()),
            parts.next().and_then(|part| part.parse::<u16>().ok()),
        )
    };
    number(left)
        .cmp(&number(right))
        .then_with(|| left.cmp(right))
}

fn valid_guide_number(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_GUIDE_NUMBER_BYTES
        || value.starts_with('0') && value != "0"
    {
        return false;
    }
    let parts = value.split('.').collect::<Vec<_>>();
    (parts.len() == 1 || parts.len() == 2)
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 4
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

fn required_field(label: &str, value: Option<&str>) -> Result<String, LiveTvError> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty()
        || value.len() > MAX_DEVICE_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(LiveTvError::InvalidResponse(format!(
            "HDHomeRun discovery returned an invalid {label}"
        )));
    }
    Ok(value.to_owned())
}

fn lineup_url(address: Ipv4Addr, advertised: Option<&str>) -> Result<reqwest::Url, LiveTvError> {
    if let Some(advertised) = advertised {
        let parsed = validate_advertised_url(advertised, &["/lineup.json"], &[80, 5004])?;
        return pinned_url(address, parsed.port().unwrap_or(80), "/lineup.json");
    }
    pinned_url(address, 80, "/lineup.json")
}

fn validate_advertised_url(
    value: &str,
    allowed_paths: &[&str],
    allowed_ports: &[u16],
) -> Result<reqwest::Url, LiveTvError> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(LiveTvError::InvalidResponse(
            "HDHomeRun advertised an invalid URL".to_owned(),
        ));
    }
    let url = reqwest::Url::parse(value).map_err(|_| {
        LiveTvError::InvalidResponse("HDHomeRun advertised an invalid URL".to_owned())
    })?;
    let port = url.port().unwrap_or(80);
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
        || !allowed_ports.contains(&port)
        || !allowed_paths.contains(&url.path())
    {
        return Err(LiveTvError::InvalidResponse(
            "HDHomeRun advertised a URL outside the pinned LAN contract".to_owned(),
        ));
    }
    Ok(url)
}

fn pinned_url(address: Ipv4Addr, port: u16, path: &str) -> Result<reqwest::Url, LiveTvError> {
    reqwest::Url::parse(&format!("http://{address}:{port}{path}"))
        .map_err(|_| LiveTvError::InvalidConfig("could not construct HDHomeRun URL".to_owned()))
}

pub(crate) async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: reqwest::Url,
) -> Result<T, LiveTvError> {
    let deadline = tokio::time::Instant::now() + DOCUMENT_TIMEOUT;
    let mut response = tokio::time::timeout_at(deadline, client.get(url).send())
        .await
        .map_err(|_| LiveTvError::DeviceUnavailable("HDHomeRun request timed out".to_owned()))?
        .map_err(|error| {
            tracing::warn!(error = %error, "HDHomeRun document request failed on the tuner owner");
            LiveTvError::DeviceUnavailable("HDHomeRun device request failed".to_owned())
        })?;
    if !response.status().is_success() {
        return Err(LiveTvError::DeviceUnavailable(format!(
            "HDHomeRun returned HTTP {}",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
    {
        return Err(LiveTvError::InvalidResponse(
            "HDHomeRun document is too large".to_owned(),
        ));
    }
    let mut body = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| LiveTvError::DeviceUnavailable("HDHomeRun body timed out".to_owned()))?
            .map_err(|error| {
                tracing::warn!(error = %error, "HDHomeRun response body failed on the tuner owner");
                LiveTvError::DeviceUnavailable("HDHomeRun response body failed".to_owned())
            })?;
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
            return Err(LiveTvError::InvalidResponse(
                "HDHomeRun document is too large".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| LiveTvError::InvalidResponse("HDHomeRun returned invalid JSON".to_owned()))
}

async fn run_graph_probe(
    ffmpeg: &str,
    encoder: Encoder,
    system: &SystemInfo,
    height: u16,
    directory: &Path,
) -> Result<(), String> {
    let playlist = directory.join("index.m3u8");
    let segments = directory.join("segment-%06d.ts");
    let mut command = tokio::process::Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    command.args(encoder.init_args());
    command.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1920x1080:rate=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:sample_rate=48000",
        "-t",
        "4.25",
    ]);
    let mut filter = format!("bwdif=mode=send_frame,scale=-2:{height}");
    if let Some(suffix) = encoder.filter_suffix() {
        filter.push(',');
        filter.push_str(suffix);
    }
    command.args(["-vf", &filter]);
    command.args(encoder.encode_args(
        if height == 1080 { 8_000 } else { 4_000 },
        EffectiveRateControl::Vbr,
        system.encoders.forced_idr.wanted_by(encoder),
        (encoder == Encoder::Software).then_some(2),
    ));
    command.args([
        "-force_key_frames",
        "expr:gte(t,n_forced*4)",
        "-g",
        "120",
        "-keyint_min",
        "120",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ac",
        "2",
        "-f",
        "hls",
        "-hls_time",
        "4",
        "-hls_list_size",
        "6",
        "-hls_delete_threshold",
        "1",
        "-hls_flags",
        "delete_segments+temp_file+independent_segments+omit_endlist",
        "-hls_segment_filename",
    ]);
    command.arg(segments).arg(&playlist);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "live-TV FFmpeg graph probe timed out".to_owned())?
        .map_err(|error| format!("could not start live-TV FFmpeg probe: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "live-TV FFmpeg graph failed: {}",
            stderr.lines().take(3).collect::<Vec<_>>().join(" ")
        ));
    }
    let manifest = tokio::fs::read_to_string(&playlist)
        .await
        .map_err(|error| format!("live-TV probe did not publish a playlist: {error}"))?;
    if manifest.contains("#EXT-X-ENDLIST") || !manifest.contains(".ts") {
        return Err("live-TV probe published an invalid live playlist".to_owned());
    }
    Ok(())
}

fn sanitize_error(message: &str) -> String {
    let cleaned = message
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect::<String>();
    if cleaned.contains("http://") || cleaned.contains("https://") {
        "HDHomeRun request failed; see owner logs for the sanitized cause".to_owned()
    } else {
        cleaned
    }
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn serve_once(response: Vec<u8>) -> (reqwest::Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake HDHomeRun");
        let address = listener.local_addr().expect("fake address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept fake request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read fake request");
            let _ = stream.write_all(&response).await;
        });
        (
            reqwest::Url::parse(&format!("http://{address}/document.json")).expect("fake URL"),
            server,
        )
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("test client")
    }

    #[test]
    fn device_address_is_a_private_canonical_literal() {
        assert_eq!(
            parse_device_ipv4("192.168.4.20").expect("private"),
            Some(Ipv4Addr::new(192, 168, 4, 20))
        );
        for refused in [
            "127.0.0.1",
            "169.254.169.254",
            "8.8.8.8",
            "224.0.0.1",
            "255.255.255.255",
            "hdhomerun.local",
            "192.168.004.020",
        ] {
            assert!(parse_device_ipv4(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn lineup_drops_bad_rows_and_fails_closed_on_duplicates_and_urls() {
        let rows = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[
              {"GuideNumber":"7.1","GuideName":"WABC","Tags":"favorite"},
              {"GuideNumber":"oops/../","GuideName":"bad"},
              {"GuideNumber":"8.1","GuideName":42},
              "not-an-object",
              {"GuideNumber":"10","GuideName":"Ten","Tags":"DRM"}
            ]"#,
        )
        .expect("lineup");
        let channels = validate_lineup(rows).expect("validated");
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].guide_number, "7.1");
        assert!(channels[0].favorite);
        assert_eq!(channels[1].support, LiveTvChannelSupport::DrmUnsupported);

        let duplicate = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[
              {"GuideNumber":"7.1","GuideName":"A"},
              {"GuideNumber":"7.1","GuideName":"B"}
            ]"#,
        )
        .expect("duplicate");
        assert!(validate_lineup(duplicate).is_err());

        let hostile = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[{"GuideNumber":"7.1","GuideName":"A","URL":"http://169.254.169.254/latest/meta-data?x=1"}]"#,
        )
        .expect("hostile");
        assert!(validate_lineup(hostile).is_err());
    }

    #[test]
    fn flex_lineup_combines_current_markers_and_fails_closed_on_unknown_drm() {
        let rows = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[
              {"GuideNumber":"2.1","GuideName":"Protected","DRM":1},
              {"GuideNumber":"4.1","GuideName":"Clear","DRM":0},
              {"GuideNumber":"5.1","GuideName":"Favorite","Favorite":1},
              {"GuideNumber":"6.1","GuideName":"Legacy","Tags":"favorite, DRM","DRM":0},
              {"GuideNumber":"7.1","GuideName":"Unknown","DRM":"unknown"},
              {"GuideNumber":"8.1","GuideName":"Null","DRM":null}
            ]"#,
        )
        .expect("FLEX lineup");
        let channels = validate_lineup(rows).expect("validated FLEX lineup");
        assert_eq!(channels.len(), 6);
        assert!(channels[0].drm);
        assert_eq!(channels[0].support, LiveTvChannelSupport::DrmUnsupported);
        assert!(!channels[1].drm);
        assert_eq!(channels[1].support, LiveTvChannelSupport::Ready);
        assert!(channels[2].favorite);
        assert!(channels[3].favorite);
        assert!(channels[3].drm, "legacy DRM must combine with DRM:0");
        assert!(channels[4].drm, "an unknown DRM marker must fail closed");
        assert!(channels[5].drm, "a null DRM marker must fail closed");
    }

    fn cache_test_snapshot(generation: i64) -> LiveTvSnapshot {
        LiveTvSnapshot {
            generation,
            device: LiveTvDevice {
                device_id: "test-device".to_owned(),
                friendly_name: "HDHomeRun FLEX 4K".to_owned(),
                model_number: "HDFX-4K".to_owned(),
                firmware_version: "test".to_owned(),
                tuner_count: 4,
            },
            channels: Vec::new(),
            freshness: SnapshotFreshness::Fresh,
            age_seconds: 0,
            last_success_at: 1,
            refresh_error: None,
            ffmpeg_graph_ready: false,
            ffmpeg_graph_message: "not probed".to_owned(),
        }
    }

    async fn forced_refresh_burst(
        cache: Arc<SnapshotCache>,
        outcome: Result<LiveTvSnapshot, LiveTvError>,
    ) -> Vec<Result<LiveTvSnapshot, LiveTvError>> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        for _ in 0..MAX_FORCED_REFRESH_CALLERS {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let release = Arc::clone(&release);
            let entered_tx = entered_tx.clone();
            let outcome = outcome.clone();
            tasks.push(tokio::spawn(async move {
                cache
                    .get_or_refresh_inner(
                        7,
                        true,
                        move || entered_tx.send(()).expect("entry observer"),
                        move || async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            let permit = release.acquire_owned().await.expect("release semaphore");
                            permit.forget();
                            outcome
                        },
                    )
                    .await
            }));
        }
        drop(entered_tx);
        // Each notification occurs after the caller recorded its request time
        // and acquired one bounded permit, but before it tries the cache lock.
        // Releasing the leader now therefore makes every other task a follower
        // by construction rather than by a wall-clock scheduling assumption.
        for _ in 0..MAX_FORCED_REFRESH_CALLERS {
            entered_rx.recv().await.expect("admitted caller");
        }
        release.add_permits(1);
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.expect("refresh task"));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one forced burst must perform exactly one device refresh"
        );
        assert_eq!(
            cache.forced_admission.available_permits(),
            MAX_FORCED_REFRESH_CALLERS,
            "every forced admission must be restored"
        );
        results
    }

    #[tokio::test]
    async fn concurrent_forced_refreshes_perform_one_bounded_device_operation() {
        let cache = Arc::new(SnapshotCache::default());
        let results = forced_refresh_burst(Arc::clone(&cache), Ok(cache_test_snapshot(7))).await;
        for result in results {
            assert_eq!(result.expect("snapshot").generation, 7);
        }

        let held = (0..MAX_FORCED_REFRESH_CALLERS)
            .map(|_| {
                cache
                    .forced_admission
                    .try_acquire()
                    .expect("bounded permit")
            })
            .collect::<Vec<_>>();
        assert!(
            cache.forced_admission.try_acquire().is_err(),
            "forced-refresh admission must have a fixed ceiling"
        );
        drop(held);
    }

    #[tokio::test]
    async fn concurrent_forced_refreshes_share_failure_and_stale_fallback() {
        let cache = Arc::new(SnapshotCache::default());
        let failures = forced_refresh_burst(
            Arc::clone(&cache),
            Err(LiveTvError::DeviceUnavailable("device offline".to_owned())),
        )
        .await;
        for failure in failures {
            assert_eq!(
                failure.expect_err("offline tuner must fail").to_string(),
                "device offline"
            );
        }

        let cache = Arc::new(SnapshotCache::default());
        cache
            .get_or_refresh(7, false, || async { Ok(cache_test_snapshot(7)) })
            .await
            .expect("prime fresh snapshot");
        let stale = forced_refresh_burst(
            cache,
            Err(LiveTvError::DeviceUnavailable("device offline".to_owned())),
        )
        .await;
        for snapshot in stale {
            let snapshot = snapshot.expect("stale fallback");
            assert_eq!(snapshot.freshness, SnapshotFreshness::Stale);
            assert_eq!(snapshot.refresh_error.as_deref(), Some("device offline"));
        }
    }

    #[test]
    fn discovery_accepts_the_documented_acronym_field_names() {
        let document = serde_json::from_str::<DiscoverDocument>(
            r#"{
              "FriendlyName":"HDHomeRun FLEX 4K",
              "ModelNumber":"HDFX-4K",
              "FirmwareVersion":"20250623",
              "DeviceID":"10ABCDEF",
              "TunerCount":4,
              "BaseURL":"http://hdhomerun.local:5004",
              "LineupURL":"http://hdhomerun.local:5004/lineup.json"
            }"#,
        )
        .expect("discovery document");
        let device = document.validated().expect("validated discovery");
        assert_eq!(device.device_id, "10ABCDEF");
        assert_eq!(device.tuner_count, 4);
        assert_eq!(
            document.lineup_url.as_deref(),
            Some("http://hdhomerun.local:5004/lineup.json")
        );
    }

    #[test]
    fn advertised_hostname_is_ignored_but_path_port_and_scheme_are_strict() {
        let allowed = validate_advertised_url(
            "http://hdhomerun.local:5004/auto/v7.1",
            &["/auto/v7.1"],
            &[80, 5004],
        )
        .expect("safe routing hint");
        assert_eq!(allowed.host_str(), Some("hdhomerun.local"));
        assert_eq!(
            lineup_url(
                Ipv4Addr::new(192, 168, 4, 20),
                Some("http://never-resolve.invalid:5004/lineup.json"),
            )
            .expect("pinned lineup")
            .as_str(),
            "http://192.168.4.20:5004/lineup.json"
        );
        for refused in [
            "https://hdhomerun.local:5004/auto/v7.1",
            "http://user@hdhomerun.local:5004/auto/v7.1",
            "http://hdhomerun.local:8080/auto/v7.1",
            "http://hdhomerun.local:5004/auto/v8.1",
            "http://hdhomerun.local:5004/auto/v7.1?redirect=x",
        ] {
            assert!(
                validate_advertised_url(refused, &["/auto/v7.1"], &[80, 5004]).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn config_defaults_disabled_without_a_compile_time_gate() {
        let config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        assert!(!config.enabled);
        assert_eq!(config.owner_node_id, "node-a");
        assert_eq!(config.max_sessions, 2);
        assert_eq!(config.output_height, 720);
        assert_eq!(config.generation, 0);
    }

    #[tokio::test]
    async fn device_documents_refuse_redirects_and_declared_oversize_bodies() {
        let (redirect, redirect_server) = serve_once(
            b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        )
        .await;
        let redirected = fetch_json::<serde_json::Value>(&test_client(), redirect).await;
        redirect_server.await.expect("redirect server");
        assert!(
            matches!(redirected, Err(LiveTvError::DeviceUnavailable(message)) if message.contains("HTTP 302"))
        );

        let (oversized, oversized_server) = serve_once(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_DOCUMENT_BYTES + 1
            )
            .into_bytes(),
        )
        .await;
        let result = fetch_json::<serde_json::Value>(&test_client(), oversized).await;
        oversized_server.await.expect("oversize server");
        assert!(matches!(
            result,
            Err(LiveTvError::InvalidResponse(message)) if message.contains("too large")
        ));
    }

    #[tokio::test]
    async fn device_documents_bound_chunked_bodies_without_content_length() {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            MAX_DOCUMENT_BYTES + 1
        )
        .into_bytes();
        response.resize(response.len() + MAX_DOCUMENT_BYTES + 1, b'x');
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let (url, server) = serve_once(response).await;
        let result = fetch_json::<serde_json::Value>(&test_client(), url).await;
        server.await.expect("chunked server");
        assert!(matches!(
            result,
            Err(LiveTvError::InvalidResponse(message)) if message.contains("too large")
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn device_document_deadline_covers_a_stalled_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake HDHomeRun");
        let address = listener.local_addr().expect("fake address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept fake request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read fake request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("write headers");
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let url =
            reqwest::Url::parse(&format!("http://{address}/document.json")).expect("fake URL");
        let fetch =
            tokio::spawn(async move { fetch_json::<serde_json::Value>(&test_client(), url).await });
        tokio::task::yield_now().await;
        tokio::time::advance(DOCUMENT_TIMEOUT + Duration::from_secs(1)).await;
        let result = fetch.await.expect("fetch task");
        assert!(matches!(
            result,
            Err(LiveTvError::DeviceUnavailable(message)) if message.contains("timed out")
        ));
        server.abort();
    }

    fn live_playlist(first: u64, count: usize) -> Vec<u8> {
        let mut playlist = format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
        for sequence in first..first + count as u64 {
            playlist.push_str(&format!("#EXTINF:4.000,\nsegment-{sequence:06}.ts\n"));
        }
        playlist.into_bytes()
    }

    #[test]
    fn live_capabilities_are_canonical_and_owner_bound() {
        let owner = "node-a/with punctuation";
        let capability = format!(
            "ltv1.{}.{}",
            base64url_encode(owner.as_bytes()),
            "d03bb893-a723-43c8-994d-8cb86d9e337a"
        );
        assert_eq!(capability_owner(&capability).expect("owner"), owner);
        validate_capability_owner(&capability, owner).expect("matching owner");
        assert!(validate_capability_owner(&capability, "node-b").is_err());

        for refused in [
            "ltv1.bm9kZS1h.d03bb893-a723-43c8-994d-8cb86d9e337a.extra",
            "ltv1.bm9kZS1h=.d03bb893-a723-43c8-994d-8cb86d9e337a",
            "ltv1.bm9kZS1h.not-a-uuid",
            "other.bm9kZS1h.d03bb893-a723-43c8-994d-8cb86d9e337a",
        ] {
            assert!(capability_owner(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn live_playlist_accepts_only_the_closed_numeric_inventory() {
        let parsed = parse_playlist_bytes(&live_playlist(41, 6)).expect("bounded playlist");
        assert_eq!(parsed.media_sequence, 41);
        assert_eq!(parsed.segments, vec![41, 42, 43, 44, 45, 46]);

        for refused in [
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\n../secret.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\nsegment-000001.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:bad\n#EXTINF:4,\nsegment-000001.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\nsegment-000001.ts\n#EXT-X-ENDLIST\n"
                .as_slice(),
            b"#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4,\n".as_slice(),
            b"#EXTM3U\n#EXTINF:4,\nsegment-000001.ts\n".as_slice(),
        ] {
            assert!(parse_playlist_bytes(refused).is_err());
        }
        assert!(parse_playlist_bytes(&live_playlist(1, 7)).is_err());
    }

    #[tokio::test]
    async fn live_scratch_inventory_enforces_list_deletion_and_temp_budgets() {
        let temp = crate::test_tempdir().expect("scratch root");
        let directory = temp.path();
        tokio::fs::write(directory.join("index.m3u8"), live_playlist(10, 3))
            .await
            .expect("playlist");
        for sequence in 10..=12 {
            tokio::fs::write(
                directory.join(format!("segment-{sequence:06}.ts")),
                b"transport",
            )
            .await
            .expect("segment");
        }
        let inventory = inspect_scratch(directory)
            .await
            .expect("scratch")
            .expect("published");
        assert_eq!(inventory.media_sequence, 10);
        assert_eq!(inventory.segments.len(), 3);

        tokio::fs::write(directory.join("segment-000009.ts"), b"lag")
            .await
            .expect("one deletion lag");
        assert!(inspect_scratch(directory).await.is_ok());
        tokio::fs::write(directory.join("segment-000008.ts"), b"excess lag")
            .await
            .expect("second deletion lag");
        assert!(inspect_scratch(directory).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_scratch_inventory_rejects_symlinks() {
        let temp = crate::test_tempdir().expect("scratch root");
        let directory = temp.path().join("session");
        tokio::fs::create_dir(&directory)
            .await
            .expect("session dir");
        tokio::fs::write(directory.join("index.m3u8"), live_playlist(1, 1))
            .await
            .expect("playlist");
        let outside = temp.path().join("outside.ts");
        tokio::fs::write(&outside, b"transport")
            .await
            .expect("outside segment");
        std::os::unix::fs::symlink(&outside, directory.join("segment-000001.ts"))
            .expect("segment symlink");
        assert!(inspect_scratch(&directory).await.is_err());
    }

    #[tokio::test]
    async fn live_tuner_open_returns_after_headers_without_buffering_the_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake tuner");
        let address = listener.local_addr().expect("fake address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept tuner open");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).await.expect("read tuner open");
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /auto/v7.1 "));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ndata\r\n",
                )
                .await
                .expect("write stream prefix");
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let url = reqwest::Url::parse(&format!("http://{address}/auto/v7.1")).expect("stream URL");
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            open_tuner_stream(
                &test_client(),
                url,
                tokio::time::Instant::now() + Duration::from_secs(1),
            ),
        )
        .await
        .expect("headers must not wait for the endless body")
        .expect("stream response");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        drop(response);
        server.abort();
    }

    #[tokio::test]
    async fn live_tuner_open_does_not_retry_a_failed_get() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake tuner");
        let address = listener.local_addr().expect("fake address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept tuner open");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read tuner open");
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write failure");
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_ok()
        });
        let url = reqwest::Url::parse(&format!("http://{address}/auto/v7.1")).expect("stream URL");
        let result = open_tuner_stream(
            &test_client(),
            url,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Err(LiveTvError::TunerUnavailable(_))));
        assert!(
            !server.await.expect("retry observer"),
            "a tuner GET was retried"
        );
    }
}
