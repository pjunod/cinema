//! HDHomeRun configuration, LAN trust boundary, and owner-side lineup cache.
//!
//! This module is always compiled. `live_tv.enabled` is an operator-controlled
//! runtime decision, never a Cargo feature or an environment-only escape hatch.

use std::collections::{BTreeMap, HashSet};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::store::{keys, Store};
use plurx_core::transcode::{EffectiveRateControl, Encoder};
use serde::{Deserialize, Serialize};

use crate::state::SystemInfo;

pub(crate) const SNAPSHOT_PATH: &str = "/_internal/v1/live-tv/snapshot";
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

#[derive(Clone, Debug)]
pub(crate) enum LiveTvError {
    InvalidConfig(String),
    DeviceUnavailable(String),
    InvalidResponse(String),
    OwnerUnavailable(String),
}

impl std::fmt::Display for LiveTvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidConfig(message)
            | Self::DeviceUnavailable(message)
            | Self::InvalidResponse(message)
            | Self::OwnerUnavailable(message) => message,
        };
        formatter.write_str(message)
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
    node_id: String,
    scratch_root: PathBuf,
    cache: SnapshotCache,
    graph_cache: tokio::sync::Mutex<Option<CachedGraphProbe>>,
}

impl LiveTvManager {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        system: Arc<SystemInfo>,
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
            node_id,
            scratch_root,
            cache: SnapshotCache::default(),
            graph_cache: tokio::sync::Mutex::new(None),
        })
    }

    pub(crate) async fn config(&self) -> Result<LiveTvConfig, LiveTvError> {
        let snapshot = self.store.settings_snapshot().await.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        Ok(LiveTvConfig::from_snapshot(&snapshot, &self.node_id))
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
}
