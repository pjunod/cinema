//! HDHomeRun configuration, LAN trust boundary, and owner-side lineup cache.
//!
//! This module is always compiled. `live_tv.enabled` is an operator-controlled
//! runtime decision, never a Cargo feature or an environment-only escape hatch.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use futures_util::StreamExt;
use plurx_core::store::{keys, Store};
use plurx_core::transcode::{EffectiveRateControl, Encoder};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::state::SystemInfo;

pub(crate) mod guide;

pub(crate) use guide::{GuideFreshness, GuideWindow, LiveTvGuide, LiveTvGuideChannel};

pub(crate) const SNAPSHOT_PATH: &str = "/_internal/v1/live-tv/snapshot";
pub(crate) const START_PATH: &str = "/_internal/v1/live-tv/start";
pub(crate) const ACTIVATE_PATH: &str = "/_internal/v1/live-tv/activate";
pub(crate) const RESOURCE_PATH: &str = "/_internal/v1/live-tv/resource";
pub(crate) const STOP_PATH: &str = "/_internal/v1/live-tv/stop";
pub(crate) const DRAIN_PATH: &str = "/_internal/v1/live-tv/drain";
pub(crate) const GUIDE_PATH: &str = "/_internal/v1/live-tv/guide";
pub(crate) const MAX_INTERNAL_BODY_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_CHANNELS: usize = 512;
const MAX_GUIDE_NUMBER_BYTES: usize = 32;
const MAX_GUIDE_NAME_BYTES: usize = 256;
const MAX_CHANNEL_FORMAT_BYTES: usize = 32;
const MAX_DEVICE_FIELD_BYTES: usize = 256;
const MAX_TUNER_STATUS_BYTES: usize = 64 * 1024;
const SNAPSHOT_TTL: Duration = Duration::from_secs(30);
const STALE_TTL: Duration = Duration::from_secs(5 * 60);
const GRAPH_PROBE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const GRAPH_PROBE_MIN_INTERVAL: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// Signal is a live diagnostic, not a condition for keeping playback alive.
/// Cache it briefly so concurrent client polls do not become a request burst
/// against the tuner, and bound an optional/unsupported endpoint tightly.
const TUNER_STATUS_TTL: Duration = Duration::from_secs(4);
const TUNER_STATUS_UNAVAILABLE_TTL: Duration = Duration::from_secs(15);
const TUNER_STATUS_TIMEOUT: Duration = Duration::from_secs(2);
/// Forced refreshes are expensive, signed owner operations. One caller does
/// the work while a small bounded set of followers may wait for its result;
/// additional callers fail promptly instead of building an unbounded queue.
const MAX_FORCED_REFRESH_CALLERS: usize = 8;
/// Guide look-ahead bounds. Four hours is what the free HDHomeRun tier
/// actually answers; seventy-two is as far as a memory cache is worth filling.
const DEFAULT_GUIDE_HOURS: u8 = 24;
const MIN_GUIDE_HOURS: u8 = 4;
const MAX_GUIDE_HOURS: u8 = 72;
const MAX_XMLTV_URL_BYTES: usize = 1024;
/// An ingress remembers the owner's answer for this long, so a click-storm on
/// a busy page is not a relay-storm on the owner. It is deliberately shorter
/// than the refresh interval: this bounds fan-out, it does not add staleness
/// the owner has not already accounted for.
const RELAY_GUIDE_MEMORY: Duration = Duration::from_secs(60);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const TUNER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PRODUCER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
/// The budget once the tuner is actually feeding the graph.
///
/// A real HDHomeRun FLEX 4K published an ATSC 3.0 (HEVC Main 10 1080) channel's
/// first segment at 18.1 s, while an ATSC 1.0 MPEG-2 channel on the same device
/// and host took 7.04 s. `STARTUP_TIMEOUT` therefore refused a channel that
/// works, after the lineup had already called it playable -- the worst of the
/// three available behaviours. Neither the codec nor the encode was the cost:
/// that source decodes and re-encodes to 720p at 2.37x realtime on the same
/// machine, and the device delivered its first byte in 2.77 s.
///
/// This is deliberately the producer-progress budget rather than a number of
/// its own: once bytes are flowing, "no segment yet" and "stopped advancing"
/// are the same question, and two constants would drift. It leaves 18.1 s about
/// 65% of headroom.
///
/// A tuner that has sent NOTHING keeps the short budget. Two ATSC 3.0 channels
/// on that antenna return zero bytes, and making an operator wait twice as long
/// to be told a mux is dead is its own defect.
const STARTUP_FEEDING_TIMEOUT: Duration = PRODUCER_PROGRESS_TIMEOUT;
/// How often a request waiting for a start re-asks whether the budget grew.
/// Without this the waiter would commit to the budget in force when it began,
/// and a tuner that starts feeding a second later would be refused anyway.
const STARTUP_BUDGET_REVIEW: Duration = Duration::from_secs(1);
const CAPABILITY_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
// Exceeds the controller's common 24-second start + two 5-second activation
// exchanges, even if the owner publishes immediately and the response is lost.
const PROVISIONAL_TIMEOUT: Duration = Duration::from_secs(40);
const SESSION_TICK: Duration = Duration::from_millis(250);
const ADMISSION_WAIT: Duration = Duration::from_secs(5);
// Publish one short segment so a channel can reach the player promptly, then
// return to the four-second steady cadence that keeps the six-segment live
// window resilient. Forced one-second keyframes let the HLS muxer honor the
// initial target without making every steady-state segment one second long.
const LIVE_HLS_OUTPUT_ARGS: [&str; 25] = [
    "-force_key_frames",
    "expr:gte(t,n_forced*1)",
    "-g",
    "120",
    "-keyint_min",
    "1",
    "-c:a",
    "aac",
    "-b:a",
    "192k",
    "-ac",
    "2",
    "-f",
    "hls",
    "-hls_init_time",
    "1",
    "-hls_time",
    "4",
    "-hls_list_size",
    "6",
    "-hls_delete_threshold",
    "1",
    "-hls_flags",
    "delete_segments+temp_file+independent_segments+omit_endlist",
    "-hls_segment_filename",
];
pub(crate) const MAX_PLAYLIST_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_SEGMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SESSION_BYTES: u64 = MAX_SEGMENT_BYTES;
const MAX_LISTED_SEGMENTS: usize = 6;
const MAX_DELETION_LAG_SEGMENTS: usize = 1;
const PUMP_CHANNEL_CAPACITY: usize = 2;
const LOCAL_RESOURCE_CHUNK_BYTES: usize = 64 * 1024;
const LOCAL_RESOURCE_CONCURRENCY: usize = 4;
const LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_RESOURCE_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINAL_TOMBSTONE_TTL: Duration = Duration::from_secs(60);
const MAX_TERMINAL_TOMBSTONES: usize = 256;
const SCRATCH_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTvConfig {
    pub(crate) enabled: bool,
    pub(crate) device_ipv4: Option<Ipv4Addr>,
    pub(crate) owner_node_id: String,
    pub(crate) max_sessions: u8,
    pub(crate) output_height: u16,
    pub(crate) generation: i64,
    /// Empty/zero means no pending handoff. The original owner and cutoff
    /// survive disabled configuration edits until cleanup is confirmed.
    pub(crate) transition_from_owner_node_id: String,
    pub(crate) transition_drain_before: i64,
    /// The programme guide. Read-only information the tuner contract never
    /// depends on, which is why these three ride the same generation CAS but
    /// not the "disable before editing" rule.
    pub(crate) guide_source: GuideSource,
    pub(crate) xmltv_url: String,
    pub(crate) guide_hours: u8,
}

/// Where programme data comes from. `HdHomeRun` is the zero-setup default once
/// an operator turns the guide on; `Xmltv` is the override for someone who
/// already runs a grabber. Precedence is this explicit choice and never an
/// inference from which fields happen to be filled in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuideSource {
    #[default]
    Off,
    HdHomeRun,
    Xmltv,
}

impl GuideSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::HdHomeRun => "hdhomerun",
            Self::Xmltv => "xmltv",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "hdhomerun" => Some(Self::HdHomeRun),
            "xmltv" => Some(Self::Xmltv),
            _ => None,
        }
    }
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
            transition_from_owner_node_id: setting(keys::LIVE_TV_TRANSITION_FROM_OWNER_NODE_ID)
                .unwrap_or_default()
                .to_owned(),
            transition_drain_before: setting(keys::LIVE_TV_TRANSITION_DRAIN_BEFORE)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            guide_source: setting(keys::LIVE_TV_GUIDE_SOURCE)
                .and_then(GuideSource::parse)
                .unwrap_or_default(),
            xmltv_url: setting(keys::LIVE_TV_XMLTV_URL)
                .unwrap_or_default()
                .to_owned(),
            guide_hours: setting(keys::LIVE_TV_GUIDE_HOURS)
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_GUIDE_HOURS),
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
        if self.transition_from_owner_node_id.len() > 256
            || self.transition_drain_before < 0
            || (self.transition_from_owner_node_id.is_empty()
                != (self.transition_drain_before == 0))
        {
            return Err(LiveTvError::InvalidConfig(
                "live-TV owner transition barrier is invalid".to_owned(),
            ));
        }
        if let Some(address) = self.device_ipv4 {
            validate_device_ipv4(address)?;
        } else if self.enabled {
            return Err(LiveTvError::InvalidConfig(
                "an HDHomeRun private IPv4 address is required before enabling".to_owned(),
            ));
        }
        self.validate_guide()?;
        Ok(())
    }

    /// Guide validation is structural only — a URL that cannot be fetched
    /// safely, or a look-ahead the cache cannot hold. Whether the source will
    /// actually answer is advisory and belongs on the Developer card, not in a
    /// refusal: an operator who cannot turn a read-only feed on cannot
    /// diagnose why it is off.
    pub(crate) fn validate_guide(&self) -> Result<(), LiveTvError> {
        if !(MIN_GUIDE_HOURS..=MAX_GUIDE_HOURS).contains(&self.guide_hours) {
            return Err(LiveTvError::InvalidConfig(format!(
                "guide look-ahead must be between {MIN_GUIDE_HOURS} and {MAX_GUIDE_HOURS} hours"
            )));
        }
        if self.xmltv_url.is_empty() {
            if self.guide_source == GuideSource::Xmltv {
                return Err(LiveTvError::InvalidConfig(
                    "an XMLTV URL is required when the guide source is XMLTV".to_owned(),
                ));
            }
            return Ok(());
        }
        validate_xmltv_url(&self.xmltv_url)?;
        Ok(())
    }

    /// The guide refresh loop runs only for a source that fetches something.
    pub(crate) fn guide_fetches(&self) -> bool {
        self.enabled && self.guide_source != GuideSource::Off
    }

    pub(crate) fn admission_ready(&self) -> bool {
        self.transition_from_owner_node_id.is_empty()
    }
}

/// The admin's own URL, so the host is theirs to choose — but the shape is
/// not. Userinfo in a URL is a credential this process would then hold and
/// log; a non-http scheme is a filesystem or protocol reach the guide has no
/// business making.
pub(crate) fn validate_xmltv_url(value: &str) -> Result<reqwest::Url, LiveTvError> {
    if value.len() > MAX_XMLTV_URL_BYTES {
        return Err(LiveTvError::InvalidConfig(
            "XMLTV URL is too long".to_owned(),
        ));
    }
    let url = reqwest::Url::parse(value.trim())
        .map_err(|_| LiveTvError::InvalidConfig("XMLTV URL is not a valid URL".to_owned()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(LiveTvError::InvalidConfig(
            "XMLTV URL must be http or https".to_owned(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LiveTvError::InvalidConfig(
            "XMLTV URL must not carry a username or password".to_owned(),
        ));
    }
    let Some(host) = url.host_str().filter(|host| !host.is_empty()) else {
        return Err(LiveTvError::InvalidConfig(
            "XMLTV URL must name a host".to_owned(),
        ));
    };
    // A literal is decided here and now. A name cannot be — what it resolves
    // to is only known at fetch time, which is where `resolve_permitted_addrs`
    // decides it.
    if let Some(address) = host_ip_literal(host) {
        validate_guide_fetch_addr(address)?;
    }
    Ok(url)
}

/// The host as an address, when it is one. `Url` normalises every IPv4
/// spelling to the canonical dotted form and brackets IPv6, so this sees the
/// same destination the connection would.
fn host_ip_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .ok()
}

/// Where an operator-supplied guide URL is allowed to point.
///
/// This is an admin-only setting, but "admin of plurx" must not widen into
/// "read any HTTP service this node can reach". A LAN address is the whole
/// point — the XMLTV document usually lives on another node — so private
/// space stays open, and what closes is the machine's own view of itself:
/// loopback, where plurx's unauthenticated internal listeners live, and
/// link-local, which carries the cloud metadata endpoint.
fn validate_guide_fetch_addr(address: IpAddr) -> Result<(), LiveTvError> {
    let refused = match address {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Link-local (fe80::/10), and the IPv4-mapped form of anything
                // refused above: a mapped 127.0.0.1 is loopback in a different
                // notation, not a different destination.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| validate_guide_fetch_addr(IpAddr::V4(mapped)).is_err())
        }
    };
    if refused {
        return Err(LiveTvError::InvalidConfig(
            "XMLTV URL must not point at this machine or at link-local space; use a LAN or public address".to_owned(),
        ));
    }
    Ok(())
}

/// Resolve the URL's host and refuse the fetch unless **every** answer is
/// permitted, then hand the checked addresses back so the request is pinned to
/// them. Checking and then letting reqwest resolve again would leave the gap
/// this closes: a name that answers with a LAN address for the check and
/// loopback for the connection.
async fn resolve_permitted_addrs(url: &reqwest::Url) -> Result<Vec<SocketAddr>, LiveTvError> {
    let host = url.host_str().unwrap_or_default().to_owned();
    if host.is_empty() || host_ip_literal(&host).is_some() {
        return Ok(Vec::new());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let resolved = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| LiveTvError::DeviceUnavailable("the XMLTV host did not resolve".to_owned()))?
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return Err(LiveTvError::DeviceUnavailable(
            "the XMLTV host did not resolve".to_owned(),
        ));
    }
    for candidate in &resolved {
        validate_guide_fetch_addr(candidate.ip())?;
    }
    Ok(resolved)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hd: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) audio_codec: Option<String>,
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
    pub(crate) source_serving_generation: u64,
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
    pub(crate) target_node_id: String,
    pub(crate) request_nonce: String,
    /// Cancel sessions strictly older than this generation.  A delayed drain
    /// can therefore never terminate a newer generation.
    pub(crate) drain_before_generation: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvDrainAck {
    pub(crate) owner_node_id: String,
    pub(crate) target_node_id: String,
    pub(crate) request_nonce: String,
    pub(crate) drained_before_generation: i64,
    pub(crate) drained: usize,
    pub(crate) signature: String,
}

impl LiveTvDrainAck {
    pub(crate) fn signing_payload(&self) -> Result<Vec<u8>, LiveTvError> {
        serde_json::to_vec(&(
            self.owner_node_id.as_str(),
            self.target_node_id.as_str(),
            self.request_nonce.as_str(),
            self.drained_before_generation,
            self.drained,
        ))
        .map_err(|error| LiveTvError::InvalidResponse(error.to_string()))
    }
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
    pub(crate) signal: Option<LiveTvSignalStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvSignalStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strength_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) quality_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) symbol_quality_percent: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LiveTvActivity {
    pub(crate) channel_number: String,
    pub(crate) channel_name: String,
    pub(crate) user: String,
    pub(crate) owner_node_id: String,
    pub(crate) encoder: String,
    pub(crate) age_seconds: u64,
    pub(crate) output_height: u16,
    pub(crate) state: String,
    /// What is on the channel this viewer is watching, when a guide is
    /// configured and has an answer. Bounded like every other row field; the
    /// Activity page shows it after the channel name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) programme_title: Option<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct LiveTvRequestKey {
    source_node_id: String,
    source_serving_generation: u64,
    user_id: i64,
    request_id: String,
}

impl From<&LiveTvStartRequest> for LiveTvRequestKey {
    fn from(request: &LiveTvStartRequest) -> Self {
        Self {
            source_node_id: request.source_node_id.clone(),
            source_serving_generation: request.source_serving_generation,
            user_id: request.user_id,
            request_id: request.request_id.clone(),
        }
    }
}

struct LiveTvSessionState {
    phase: LiveTvSessionPhase,
    startup: Option<Result<LiveTvProvisional, LiveTvError>>,
    activated: bool,
    provisional_at: Option<tokio::time::Instant>,
    last_touch: tokio::time::Instant,
    last_progress: tokio::time::Instant,
    media_sequence: u64,
    publication: Option<ScratchInventory>,
    encoder: String,
    error: Option<String>,
    terminal_error: Option<LiveTvError>,
    cleanup: Option<Result<(), LiveTvError>>,
}

struct LiveTvSession {
    capability: String,
    activation_token: String,
    request: LiveTvStartRequest,
    channel: LiveTvChannel,
    output: LiveTvOutput,
    device_ipv4: Option<Ipv4Addr>,
    owner_serving_generation: u64,
    started: tokio::time::Instant,
    directory: PathBuf,
    cancel: CancellationToken,
    changed: tokio::sync::Notify,
    startup_waiters: AtomicUsize,
    worker: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    process: tokio::sync::Mutex<Option<LiveTvProcess>>,
    decoder_unavailable: Arc<AtomicBool>,
    /// Bytes the tuner has delivered into the graph. Shared with the pump so
    /// the startup decision can tell a slow channel from an absent one.
    tuner_bytes: Arc<AtomicU64>,
    resource_admission: Arc<tokio::sync::Semaphore>,
    signal_cache: tokio::sync::Mutex<Option<(tokio::time::Instant, Option<LiveTvSignalStatus>)>>,
    state: StdMutex<LiveTvSessionState>,
}

struct LiveTvProcess {
    child: tokio::process::Child,
    // Retain the encoder reservation until exit is positively confirmed.
    _admission: crate::transcode::LiveAdmission,
    stderr: Option<tokio::task::JoinHandle<()>>,
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
            signal: None,
            error: state.error.clone(),
        }
    }
}

#[derive(Default)]
struct LiveTvRegistry {
    closing: bool,
    min_generation: i64,
    sessions: HashMap<String, Arc<LiveTvSession>>,
    requests: HashMap<LiveTvRequestKey, String>,
    terminals: HashMap<String, LiveTvTerminalTombstone>,
}

#[derive(Clone)]
struct LiveTvTerminalTombstone {
    error: LiveTvError,
    request: LiveTvStartRequest,
    expires_at: tokio::time::Instant,
}

impl LiveTvRegistry {
    fn prune_terminals(&mut self) {
        let now = tokio::time::Instant::now();
        self.terminals
            .retain(|_, tombstone| tombstone.expires_at > now);
    }

    fn request_session(
        &mut self,
        request: &LiveTvStartRequest,
    ) -> Result<Option<Arc<LiveTvSession>>, LiveTvError> {
        self.prune_terminals();
        let key = LiveTvRequestKey::from(request);
        if let Some(tombstone) = self
            .terminals
            .values()
            .find(|entry| LiveTvRequestKey::from(&entry.request) == key)
        {
            if tombstone.request != *request {
                return Err(LiveTvError::Conflict(
                    "the live-TV request id was replayed with different fields".into(),
                ));
            }
            return Err(tombstone.error.clone());
        }
        let Some(capability) = self.requests.get(&key) else {
            return Ok(None);
        };
        let session = self.sessions.get(capability).cloned().ok_or_else(|| {
            LiveTvError::Conflict("live-TV request recovery state is inconsistent".into())
        })?;
        if session.request != *request {
            return Err(LiveTvError::Conflict(
                "the live-TV request id was replayed with different fields".into(),
            ));
        }
        Ok(Some(session))
    }
}

#[derive(Default)]
pub(crate) struct LiveTvMetrics {
    registry: Arc<StdMutex<LiveTvRegistry>>,
    projection: StdMutex<LiveTvMetricsProjection>,
    starts_created: AtomicU64,
    starts_recovered: AtomicU64,
    starts_failed: AtomicU64,
    ended: AtomicU64,
    relay_bytes: Arc<AtomicU64>,
    /// Guide refresh outcomes by source, and a projection of the cache the
    /// same shape as the device one: a value plus when it was observed, so a
    /// stalled refresh loop reports staleness instead of a frozen number.
    guide_refreshes: StdMutex<BTreeMap<(&'static str, &'static str), u64>>,
    guide: StdMutex<Option<(tokio::time::Instant, usize)>>,
}

#[derive(Default)]
struct LiveTvMetricsProjection {
    config: Option<(i64, bool, tokio::time::Instant)>,
    device: Option<(i64, tokio::time::Instant, usize, usize)>,
}

impl LiveTvMetrics {
    fn observe_guide_refresh(&self, source: GuideSource, outcome: &'static str) {
        let mut counters = self
            .guide_refreshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *counters.entry((source.as_str(), outcome)).or_default() += 1;
    }

    fn observe_guide(&self, programmes: usize) {
        let mut projection = self
            .guide
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *projection = Some((tokio::time::Instant::now(), programmes));
    }

    /// The three guide series. `plurx_live_tv_guide_age_seconds` is the one an
    /// operator actually alerts on: a refresh loop that has stopped shows up
    /// here as a climbing age long before anyone notices an empty grid.
    fn guide_prometheus(&self) -> String {
        let counters = self
            .guide_refreshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let projection = *self
            .guide
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out = String::from(
            "# HELP plurx_live_tv_guide_refresh_total Programme-guide refresh attempts by source and outcome.\n\
             # TYPE plurx_live_tv_guide_refresh_total counter\n",
        );
        if counters.is_empty() {
            out.push_str(
                "plurx_live_tv_guide_refresh_total{source=\"off\",outcome=\"skipped\"} 0\n",
            );
        }
        for ((source, outcome), value) in &counters {
            out.push_str(&format!(
                "plurx_live_tv_guide_refresh_total{{source=\"{source}\",outcome=\"{outcome}\"}} {value}\n"
            ));
        }
        let (age, programmes) = match projection {
            Some((observed, programmes)) => (
                tokio::time::Instant::now()
                    .duration_since(observed)
                    .as_secs()
                    .to_string(),
                programmes.to_string(),
            ),
            None => ("NaN".to_owned(), "NaN".to_owned()),
        };
        out.push_str(&format!(
            "# HELP plurx_live_tv_guide_age_seconds Seconds since the cached programme guide was fetched.\n\
             # TYPE plurx_live_tv_guide_age_seconds gauge\n\
             plurx_live_tv_guide_age_seconds {age}\n\
             # HELP plurx_live_tv_guide_programmes Programme rows in the owner's guide cache.\n\
             # TYPE plurx_live_tv_guide_programmes gauge\n\
             plurx_live_tv_guide_programmes {programmes}\n"
        ));
        out
    }
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

/// The owner's in-memory programme cache. Same shape as `SnapshotCache` — a
/// mutex around the state, a bounded admission semaphore for forced
/// refreshes, generation equality as the only invalidation rule — with one
/// difference: the guide has a refresh *loop*, so a serving read never
/// triggers a fetch. `GET /live-tv/guide` answers from whatever is here, which
/// is what makes it always fast and always answerable.
struct GuideCache {
    state: tokio::sync::Mutex<GuideCacheState>,
    forced_admission: tokio::sync::Semaphore,
}

#[derive(Default)]
struct GuideCacheState {
    cached: Option<CachedGuide>,
    /// Kept separately from the cache so a refresh that failed while a good
    /// cache is still warm reports the failure without discarding the answer.
    /// Scoped to the generation it happened under: an error from the previous
    /// configuration is not a fact about the current one.
    last_error: Option<(i64, String)>,
}

impl GuideCacheState {
    fn error_for(&self, generation: i64) -> Option<String> {
        self.last_error
            .as_ref()
            .filter(|(recorded, _)| *recorded == generation)
            .map(|(_, message)| message.clone())
    }
}

/// Programme titles by guide number, each row a `(start, end, title)` span.
/// The activity path reads this synchronously; it is the only part of the
/// guide that a non-async caller needs.
type GuideTitleIndex = BTreeMap<String, Vec<(i64, i64, String)>>;

#[derive(Clone)]
struct CachedGuide {
    generation: i64,
    observed: tokio::time::Instant,
    fetched_at: i64,
    guide: LiveTvGuide,
}

impl Default for GuideCache {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(GuideCacheState::default()),
            forced_admission: tokio::sync::Semaphore::new(MAX_FORCED_REFRESH_CALLERS),
        }
    }
}

impl GuideCache {
    /// What a reader gets. Never fetches: a cache older than the stale window
    /// is dropped rather than served, because a six-hour-old grid pretending
    /// to be current is worse than an empty one.
    async fn read(&self, config: &LiveTvConfig, window: GuideWindow) -> LiveTvGuide {
        let state = self.state.lock().await;
        let now = tokio::time::Instant::now();
        let Some(cached) = state
            .cached
            .as_ref()
            .filter(|cached| cached.generation == config.generation)
            .filter(|cached| now.duration_since(cached.observed) <= guide::GUIDE_STALE_TTL)
        else {
            return LiveTvGuide::unavailable(
                config.guide_source,
                window,
                state.error_for(config.generation),
            );
        };
        let age = now.duration_since(cached.observed);
        let mut guide = cached.guide.clipped(&window);
        guide.age_seconds = age.as_secs();
        guide.fetched_at = Some(cached.fetched_at);
        guide.freshness = if age <= guide::GUIDE_REFRESH_INTERVAL {
            GuideFreshness::Fresh
        } else {
            GuideFreshness::Stale
        };
        guide.refresh_error = state.error_for(config.generation);
        guide
    }

    /// A completed refresh. A failure keeps the previous cache and records the
    /// error; only a success replaces the content.
    async fn store(&self, generation: i64, result: &Result<LiveTvGuide, LiveTvError>) {
        let mut state = self.state.lock().await;
        match result {
            Ok(guide) => {
                state.cached = Some(CachedGuide {
                    generation,
                    observed: tokio::time::Instant::now(),
                    fetched_at: unix_seconds(),
                    guide: guide.clone(),
                });
                state.last_error = None;
            }
            Err(error) => {
                state.last_error = Some((generation, guide::sanitize_refresh_error(error)))
            }
        }
    }

    /// A settings change discards the cache: the lineup it was matched
    /// against, and possibly the source itself, just changed.
    async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        state.cached = None;
        state.last_error = None;
    }

    async fn cached_generation(&self) -> Option<i64> {
        self.state
            .lock()
            .await
            .cached
            .as_ref()
            .map(|c| c.generation)
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
    guide_cache: GuideCache,
    /// Titles only, keyed by guide number, refreshed with the cache. The
    /// activity path is synchronous and must stay that way — every one of its
    /// four callers is on a request path — so it reads this rather than
    /// awaiting the guide mutex.
    guide_titles: StdMutex<Arc<GuideTitleIndex>>,
    /// A non-owner ingress's short memory of the owner's answer. Bounds
    /// fan-out on a page every viewer opens at once; it is not a second cache
    /// in any meaningful sense, because it holds exactly one generation and
    /// expires in a minute.
    relayed_guide: tokio::sync::Mutex<Option<(i64, tokio::time::Instant, LiveTvGuide)>>,
    graph_cache: tokio::sync::Mutex<Option<CachedGraphProbe>>,
    registry: Arc<StdMutex<LiveTvRegistry>>,
    scratch_claims: StdMutex<HashSet<PathBuf>>,
    scratch_sweep_gate: tokio::sync::Mutex<()>,
    metrics: Arc<LiveTvMetrics>,
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
        let metrics = Arc::new(LiveTvMetrics::default());
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
            guide_cache: GuideCache::default(),
            guide_titles: StdMutex::new(Arc::new(BTreeMap::new())),
            relayed_guide: tokio::sync::Mutex::new(None),
            graph_cache: tokio::sync::Mutex::new(None),
            registry: Arc::clone(&metrics.registry),
            scratch_claims: StdMutex::new(HashSet::new()),
            scratch_sweep_gate: tokio::sync::Mutex::new(()),
            metrics,
        })
    }

    pub(crate) async fn config(&self) -> Result<LiveTvConfig, LiveTvError> {
        let snapshot = self.store.settings_snapshot().await.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        let config = LiveTvConfig::from_snapshot(&snapshot, &self.node_id);
        self.observe_config(&config);
        Ok(config)
    }

    pub(crate) fn observe_config(&self, config: &LiveTvConfig) {
        let mut projection = self
            .metrics
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if projection
            .config
            .is_none_or(|(generation, _, _)| generation <= config.generation)
        {
            projection.config = Some((
                config.generation,
                config.enabled,
                tokio::time::Instant::now(),
            ));
        }
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
        let snapshot = match self
            .cache
            .get_or_refresh(config.generation, force, || {
                self.fetch_snapshot(address, config)
            })
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.metrics
                    .projection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .device = None;
                return Err(error);
            }
        };
        if snapshot.freshness != SnapshotFreshness::Fresh {
            self.metrics
                .projection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .device = None;
        }
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
        self.metrics
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .device = Some((
            config.generation,
            tokio::time::Instant::now(),
            channels.iter().filter(|channel| !channel.drm).count(),
            channels.iter().filter(|channel| channel.drm).count(),
        ));
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
        // Serialize the first directory publication against the orphan scan.
        // Once created, the retained claim is enough to protect this probe.
        let scratch_creation = self.scratch_sweep_gate.lock().await;
        let _scratch_claim = self.claim_scratch(probe_dir.clone());
        if let Err(error) = tokio::fs::create_dir_all(&probe_dir).await {
            return (
                false,
                format!("cannot create live-TV probe scratch: {error}"),
            );
        }
        drop(scratch_creation);
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
        let result = self.start_local_inner(request).await;
        if result.is_err() {
            self.metrics.starts_failed.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn start_local_inner(
        self: &Arc<Self>,
        request: LiveTvStartRequest,
    ) -> Result<LiveTvProvisional, LiveTvError> {
        let started = tokio::time::Instant::now();
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
        let snapshot = tokio::time::timeout_at(
            started + STARTUP_TIMEOUT,
            self.local_snapshot(&config, true, false),
        )
        .await
        .map_err(|_| {
            LiveTvError::StartupTimeout(
                "the fresh tuner lineup exceeded the live-TV startup deadline".into(),
            )
        })??;
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
            device_ipv4: Some(address),
            owner_serving_generation: serving_generation,
            started,
            directory,
            cancel: CancellationToken::new(),
            changed: tokio::sync::Notify::new(),
            startup_waiters: AtomicUsize::new(0),
            worker: StdMutex::new(None),
            process: tokio::sync::Mutex::new(None),
            decoder_unavailable: Arc::new(AtomicBool::new(false)),
            tuner_bytes: Arc::new(AtomicU64::new(0)),
            resource_admission: Arc::new(tokio::sync::Semaphore::new(LOCAL_RESOURCE_CONCURRENCY)),
            signal_cache: tokio::sync::Mutex::new(None),
            state: StdMutex::new(LiveTvSessionState {
                phase: LiveTvSessionPhase::Starting,
                startup: None,
                activated: false,
                provisional_at: None,
                last_touch: now,
                last_progress: now,
                media_sequence: 0,
                publication: None,
                encoder: "pending".to_owned(),
                error: None,
                terminal_error: None,
                cleanup: None,
            }),
        });

        let raced_session = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if registry.closing || request.config_generation < registry.min_generation {
                return Err(LiveTvError::Conflict(
                    "the live-TV start was fenced by a drain".into(),
                ));
            }
            // A concurrent identical request may have won while the lineup
            // refresh was in flight. Recover it without consuming capacity.
            if let Some(existing) = registry.request_session(&request)? {
                Some(existing)
            } else {
                if registry.sessions.len() + registry.terminals.len() >= MAX_TERMINAL_TOMBSTONES {
                    return Err(LiveTvError::Capacity(
                        "live-TV request recovery history is full; retry after one minute".into(),
                    ));
                }
                if registry.sessions.len() >= usize::from(config.max_sessions) {
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
        let worker_session = Arc::clone(&session);
        let worker = tokio::spawn(async move {
            run_live_session(manager, worker_session, config, stream_url).await;
        });
        *session
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        self.metrics.starts_created.fetch_add(1, Ordering::Relaxed);
        wait_for_startup(session, false).await
    }

    fn session_for_request(
        &self,
        _key: &LiveTvRequestKey,
        request: &LiveTvStartRequest,
    ) -> Result<Option<Arc<LiveTvSession>>, LiveTvError> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request_session(request)
    }

    pub(crate) async fn activate_local(
        &self,
        request: &LiveTvActivateRequest,
        source_node_id: &str,
    ) -> Result<LiveTvActivated, LiveTvError> {
        if request.expected_owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "the activation request names a different tuner owner".into(),
            ));
        }
        validate_capability_owner(&request.capability, &self.node_id)?;
        let session = self.session(&request.capability)?;
        if session.request.source_node_id != source_node_id {
            return Err(LiveTvError::Conflict(
                "the activation signer does not own this live-TV start".into(),
            ));
        }
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
        if session.request.source_serving_generation != request.source_serving_generation {
            return Err(LiveTvError::Conflict(
                "the live-TV source serving generation changed before activation".into(),
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
            if let Some(error) = state.terminal_error.clone() {
                return Err(error);
            }
            if session.cancel.is_cancelled()
                || !state.activated
                || state.phase != LiveTvSessionPhase::Active
            {
                return Err(LiveTvError::CapabilityExpired(
                    "the live-TV capability is not active".into(),
                ));
            }
            state.last_touch = tokio::time::Instant::now();
        }
        match request {
            LiveTvResourceRequest::Playlist { .. } => {
                let bytes = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .publication
                    .as_ref()
                    .map(|publication| publication.playlist.clone())
                    .ok_or_else(|| {
                        LiveTvError::CapabilityExpired(
                            "the live-TV playlist is not published".into(),
                        )
                    })?;
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(Body::from(bytes))
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Segment { sequence, .. } => {
                let permit = Arc::clone(&session.resource_admission)
                    .try_acquire_owned()
                    .map_err(|_| {
                        LiveTvError::StreamFailed(
                            "too many concurrent live-TV segment responses; retry shortly".into(),
                        )
                    })?;
                let (name, expected_len) = {
                    let state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state
                        .publication
                        .as_ref()
                        .and_then(|publication| publication.segments.get(&sequence))
                        .cloned()
                        .ok_or_else(|| {
                            LiveTvError::CapabilityExpired(
                                "the requested live segment is outside the current window".into(),
                            )
                        })?
                };
                let path = session.directory.join(&name);
                let file = tokio::select! {
                    biased;
                    _ = session.cancel.cancelled() => return Err(LiveTvError::CapabilityExpired("the live-TV session stopped".into())),
                    result = tokio::time::timeout(LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT, open_bounded_segment(&path, expected_len)) => {
                        result.map_err(|_| LiveTvError::StreamFailed("opening the live-TV segment timed out".into()))??
                    }
                };
                let body = bounded_segment_body(file, expected_len, session.cancel.clone(), permit);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "video/mp2t")
                    .header(header::CACHE_CONTROL, "no-store")
                    .header(header::CONTENT_LENGTH, expected_len)
                    .body(body)
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Status { .. } => {
                let mut status = session.status(&self.node_id);
                status.signal = self.session_signal(&session).await;
                let body = serde_json::to_vec(&status)
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

    async fn session_signal(&self, session: &LiveTvSession) -> Option<LiveTvSignalStatus> {
        let mut cache = session.signal_cache.lock().await;
        let now = tokio::time::Instant::now();
        if let Some((observed, signal)) = cache.as_ref() {
            let ttl = if signal.is_some() {
                TUNER_STATUS_TTL
            } else {
                TUNER_STATUS_UNAVAILABLE_TTL
            };
            if now.duration_since(*observed) <= ttl {
                return signal.clone();
            }
        }
        let signal = async {
            let address = session.device_ipv4?;
            let client = self.client.as_ref().ok()?;
            let url = pinned_url(address, 80, "/status.json").ok()?;
            let body = fetch_bounded(client, url, MAX_TUNER_STATUS_BYTES, TUNER_STATUS_TIMEOUT)
                .await
                .ok()?;
            let rows = serde_json::from_slice::<Vec<serde_json::Value>>(&body).ok()?;
            tuner_signal_for_channel(rows, &session.channel.guide_number)
        }
        .await;
        *cache = Some((now, signal.clone()));
        signal
    }

    pub(crate) async fn stop_local(&self, capability: &str) -> Result<(), LiveTvError> {
        validate_capability_owner(capability, &self.node_id)?;
        let session = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(capability)
            .cloned();
        let Some(session) = session else {
            return Ok(());
        };
        self.cancel_and_wait(vec![session]).await.map(|_| ())
    }

    pub(crate) async fn drain_before(
        &self,
        drain_before_generation: i64,
    ) -> Result<usize, LiveTvError> {
        let sessions = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.min_generation = registry.min_generation.max(drain_before_generation);
            registry
                .sessions
                .values()
                .filter(|session| session.request.config_generation < drain_before_generation)
                .cloned()
                .collect::<Vec<_>>()
        };
        self.cancel_and_wait(sessions).await
    }

    pub(crate) async fn shutdown(&self) -> Result<usize, LiveTvError> {
        let sessions = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.closing = true;
            registry.sessions.values().cloned().collect::<Vec<_>>()
        };
        self.cancel_and_wait(sessions).await
    }

    async fn cancel_and_wait(
        &self,
        sessions: Vec<Arc<LiveTvSession>>,
    ) -> Result<usize, LiveTvError> {
        let count = sessions.len();
        for session in &sessions {
            session.cancel.cancel();
        }
        let deadline = tokio::time::Instant::now() + SESSION_DRAIN_TIMEOUT;
        for session in &sessions {
            let worker = session
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(worker) = worker {
                tokio::time::timeout_at(deadline, worker)
                    .await
                    .map_err(|_| {
                        LiveTvError::StreamFailed(
                            "live-TV worker did not stop before the drain deadline".into(),
                        )
                    })?
                    .map_err(|error| {
                        LiveTvError::StreamFailed(format!(
                            "live-TV worker failed during drain: {error}"
                        ))
                    })?;
            }
        }
        for session in sessions {
            let failed_cleanup = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cleanup
                .as_ref()
                .is_some_and(Result::is_err);
            if failed_cleanup {
                let cleanup = cleanup_session(&session).await;
                session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .cleanup = Some(cleanup.clone());
                cleanup?;
                self.retire_session(&session);
            }
            loop {
                let notified = session.changed.notified();
                if let Some(Err(error)) = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .cleanup
                    .clone()
                {
                    return Err(error);
                }
                let present = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sessions
                    .contains_key(&session.capability);
                if !present {
                    break;
                }
                tokio::time::timeout_at(deadline, notified)
                    .await
                    .map_err(|_| {
                        LiveTvError::StreamFailed(
                            "live-TV cleanup was not confirmed before the drain deadline".into(),
                        )
                    })?;
            }
        }
        Ok(count)
    }

    fn retire_session(&self, session: &LiveTvSession) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry.sessions.remove(&session.capability).is_none() {
            return;
        }
        registry
            .requests
            .retain(|_, capability| capability != &session.capability);
        registry.prune_terminals();
        let error = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal_error
            .clone()
            .unwrap_or_else(|| {
                LiveTvError::CapabilityExpired(
                    "the live-TV session ended; start a new Watch request".into(),
                )
            });
        registry.terminals.insert(
            session.capability.clone(),
            LiveTvTerminalTombstone {
                error,
                request: session.request.clone(),
                expires_at: tokio::time::Instant::now() + TERMINAL_TOMBSTONE_TTL,
            },
        );
        self.metrics.ended.fetch_add(1, Ordering::Relaxed);
        session.changed.notify_waiters();
    }

    pub(crate) fn activities(&self) -> Vec<LiveTvActivity> {
        let now = tokio::time::Instant::now();
        let instant = unix_seconds();
        let titles = Arc::clone(
            &self
                .guide_titles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
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
                    programme_title: titles
                        .get(&session.channel.guide_number)
                        .and_then(|rows| {
                            rows.iter()
                                .find(|(start, end, _)| *start <= instant && instant < *end)
                        })
                        .map(|(_, _, title)| title.clone()),
                }
            })
            .collect()
    }

    fn claim_scratch(&self, path: PathBuf) -> ScratchClaim<'_> {
        self.scratch_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.clone());
        ScratchClaim {
            claims: &self.scratch_claims,
            path,
        }
    }

    pub(crate) async fn sweep_orphan_scratch(&self) -> usize {
        // A session registers before creating its directory. Holding this gate
        // across the registry/claim snapshot and directory walk ensures a new
        // owner cannot publish a directory after our snapshot and have that
        // directory mistaken for crash garbage.
        let _sweep = self.scratch_sweep_gate.lock().await;
        let mut owned = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .values()
            .map(|session| session.directory.clone())
            .collect::<HashSet<_>>();
        owned.extend(
            self.scratch_claims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned(),
        );
        let Ok(mut entries) = tokio::fs::read_dir(&self.scratch_root).await else {
            return 0;
        };
        let mut removed = 0;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if owned.contains(&path) || !entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => removed += 1,
                Err(error) => tracing::warn!(
                    kind = %error.kind(),
                    "Live TV orphan scratch sweep failed"
                ),
            }
        }
        removed
    }

    pub(crate) async fn scratch_sweep_loop(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            let removed = self.sweep_orphan_scratch().await;
            if removed > 0 {
                tracing::info!(removed, "swept orphaned Live TV scratch directories");
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(SCRATCH_SWEEP_INTERVAL) => {}
            }
        }
    }

    pub(crate) fn metrics_handle(&self) -> Arc<LiveTvMetrics> {
        Arc::clone(&self.metrics)
    }

    pub(crate) async fn metrics_loop(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = self.config() => {}
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    // ---- programme guide --------------------------------------------------

    /// Serve the cached guide. This never fetches and never fails: a guide
    /// that is off, empty, stale or erroring is a *rendered* state, so the
    /// caller always has something to show under the channel names.
    pub(crate) async fn local_guide(
        &self,
        config: &LiveTvConfig,
        window: GuideWindow,
    ) -> LiveTvGuide {
        if config.owner_node_id != self.node_id || config.guide_source == GuideSource::Off {
            return LiveTvGuide::unavailable(config.guide_source, window, None);
        }
        self.guide_cache.read(config, window).await
    }

    /// Run one refresh now. Used by the loop and by the Developer card's
    /// button; `force` only bounds admission, since the loop's own cadence is
    /// what normally decides when a fetch happens.
    pub(crate) async fn refresh_guide(
        &self,
        config: &LiveTvConfig,
        force: bool,
    ) -> Result<LiveTvGuide, LiveTvError> {
        if config.owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "this node is not the configured HDHomeRun owner".to_owned(),
            ));
        }
        if !config.guide_fetches() {
            return Err(LiveTvError::InvalidConfig(
                "no programme guide source is configured".to_owned(),
            ));
        }
        let _permit = if force {
            Some(
                self.guide_cache
                    .forced_admission
                    .try_acquire()
                    .map_err(|_| {
                        LiveTvError::DeviceUnavailable(
                            "a guide refresh is already running; retry shortly".to_owned(),
                        )
                    })?,
            )
        } else {
            None
        };
        // The cache is matched against a lineup, so a refresh needs one — but
        // the *cached* one, and only the cached one. Asking `local_snapshot`
        // for it hits the device whenever the 30 s lineup TTL has expired,
        // which at a 20-minute cadence is every single time; an XMLTV guide
        // then failed outright whenever the tuner was switched off, having
        // never reached the XMLTV URL. A guide refresh must never be a reason
        // to talk to the tuner, and now it cannot be.
        let channels = self.cached_lineup(config).await;
        // A cold lineup is not a guide with no programmes — it is a refresh
        // that cannot be matched yet. Fetching anyway produces an empty guide
        // and *stores* it, replacing a good cache with nothing and reporting
        // `matched_channels: 0` as if the source were at fault. Refusing here
        // keeps whatever is cached; the loop comes back in a minute.
        if channels.is_empty() {
            self.metrics
                .observe_guide_refresh(config.guide_source, "cold_lineup");
            return Err(LiveTvError::DeviceUnavailable(
                "the channel lineup has not been read yet on this node; open Live TV once, or wait a minute, and the guide fills in"
                    .to_owned(),
            ));
        }
        let result = self.fetch_guide(config, &channels).await;
        let outcome = if result.is_ok() { "ok" } else { "error" };
        self.metrics
            .observe_guide_refresh(config.guide_source, outcome);
        self.guide_cache.store(config.generation, &result).await;
        // A failed refresh keeps the previous titles for as long as the cache
        // behind them is still served.
        if let Ok(guide) = &result {
            self.metrics.observe_guide(guide.total_programmes());
            self.publish_guide_titles(guide);
        }
        result
    }

    /// The lineup the guide is matched against, read from the snapshot cache
    /// without ever refreshing it. An empty answer is a refresh that has to
    /// wait, not a guide with no channels.
    async fn cached_lineup(&self, config: &LiveTvConfig) -> Vec<LiveTvChannel> {
        let state = self.cache.state.lock().await;
        state
            .snapshot
            .as_ref()
            .filter(|cached| cached.generation == config.generation)
            .map(|cached| cached.snapshot.channels.clone())
            .unwrap_or_default()
    }

    async fn fetch_guide(
        &self,
        config: &LiveTvConfig,
        lineup: &[LiveTvChannel],
    ) -> Result<LiveTvGuide, LiveTvError> {
        let client = self.guide_client()?;
        let window = guide::refresh_window(config.guide_hours);
        let channels = match config.guide_source {
            GuideSource::Off => Vec::new(),
            GuideSource::HdHomeRun => {
                let address = config.device_ipv4.ok_or_else(|| {
                    LiveTvError::InvalidConfig(
                        "an HDHomeRun address is required for the HDHomeRun guide".to_owned(),
                    )
                })?;
                self.fetch_hdhomerun_guide(&client, address, lineup, &window)
                    .await?
            }
            GuideSource::Xmltv => {
                let url = validate_xmltv_url(&config.xmltv_url)?;
                let pinned = resolve_permitted_addrs(&url).await?;
                let client = if pinned.is_empty() {
                    client.clone()
                } else {
                    self.pinned_guide_client(&url, &pinned)?
                };
                let body = fetch_bounded(
                    &client,
                    url,
                    guide::GUIDE_MAX_DOCUMENT_BYTES,
                    guide::GUIDE_FETCH_TIMEOUT,
                )
                .await?;
                guide::parse_xmltv(&guide::decompress_if_gzip(body)?, lineup)?
            }
        };
        let matched = channels.len();
        Ok(LiveTvGuide {
            source: config.guide_source.as_str().to_owned(),
            freshness: GuideFreshness::Fresh,
            age_seconds: 0,
            fetched_at: Some(unix_seconds()),
            window,
            refresh_error: None,
            matched_channels: matched,
            lineup_channels: lineup.len(),
            channels,
        })
    }

    async fn fetch_hdhomerun_guide(
        &self,
        client: &reqwest::Client,
        address: Ipv4Addr,
        lineup: &[LiveTvChannel],
        window: &GuideWindow,
    ) -> Result<Vec<LiveTvGuideChannel>, LiveTvError> {
        let by_number = lineup
            .iter()
            .map(|channel| (channel.guide_number.clone(), channel))
            .collect::<BTreeMap<_, _>>();
        // Read the credential, use it, drop it. It is a local binding inside
        // this function and appears in nothing this function returns.
        let device_auth = guide::read_device_auth(client, address).await?;
        let bulk = fetch_bounded(
            client,
            guide::guide_request_url(&device_auth, None, None)?,
            guide::GUIDE_MAX_DOCUMENT_BYTES,
            guide::GUIDE_FETCH_TIMEOUT,
        )
        .await?;
        let mut channels = guide::parse_hdhomerun_guide(&bulk, &by_number)?;

        // The free tier answers a few hours to the bulk call. Extend the
        // channels that fall short until the request budget is spent — an
        // empty or repeated answer means that channel has no more data and
        // stops it, so a lineup of stubborn channels cannot spin here.
        let mut budget = guide::GUIDE_MAX_EXTENSION_REQUESTS;
        let mut index = 0;
        while index < channels.len() && budget > 0 {
            let Some(last_end) = channels[index].programmes.last().map(|p| p.end) else {
                index += 1;
                continue;
            };
            if last_end >= window.end {
                index += 1;
                continue;
            }
            budget -= 1;
            let number = channels[index].guide_number.clone();
            let url = guide::guide_request_url(&device_auth, Some(&number), Some(last_end))?;
            let page = match fetch_bounded(
                client,
                url,
                guide::GUIDE_MAX_DOCUMENT_BYTES,
                guide::GUIDE_FETCH_TIMEOUT,
            )
            .await
            {
                Ok(page) => page,
                // A failed extension is not a failed refresh: the bulk answer
                // is already useful and the grid simply ends earlier.
                Err(_) => {
                    index += 1;
                    continue;
                }
            };
            let extra = guide::parse_hdhomerun_guide(&page, &by_number)?;
            let Some(extra) = extra
                .into_iter()
                .find(|candidate| candidate.guide_number == number)
                .filter(|candidate| !candidate.programmes.is_empty())
            else {
                index += 1;
                continue;
            };
            let before = channels[index].programmes.len();
            guide::merge_channel(&mut channels[index], extra);
            if channels[index].programmes.len() == before {
                index += 1;
            }
        }
        Ok(channels)
    }

    /// An ingress's memory of the owner's answer, if it is still current.
    pub(crate) async fn relayed_guide(&self, generation: i64) -> Option<LiveTvGuide> {
        let cached = self.relayed_guide.lock().await;
        let (cached_generation, observed, guide) = cached.as_ref()?;
        (*cached_generation == generation
            && tokio::time::Instant::now().duration_since(*observed) <= RELAY_GUIDE_MEMORY)
            .then(|| guide.clone())
    }

    pub(crate) async fn remember_relayed_guide(&self, generation: i64, guide: LiveTvGuide) {
        *self.relayed_guide.lock().await = Some((generation, tokio::time::Instant::now(), guide));
    }

    fn publish_guide_titles(&self, guide: &LiveTvGuide) {
        let index = guide
            .channels
            .iter()
            .map(|channel| {
                (
                    channel.guide_number.clone(),
                    channel
                        .programmes
                        .iter()
                        .map(|p| (p.start, p.end, p.title.clone()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        *self
            .guide_titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(index);
    }

    fn clear_guide_titles(&self) {
        *self
            .guide_titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(BTreeMap::new());
    }

    /// A dedicated client: the guide's deadline is its own, and unlike the
    /// tuner client it talks to a *public* host, which is where an unrefused
    /// redirect would otherwise carry the credential somewhere else.
    /// The guide client, but able to reach the host only at the addresses
    /// `resolve_permitted_addrs` approved. Redirects are already refused, so
    /// this is the last hop as well as the first.
    fn pinned_guide_client(
        &self,
        url: &reqwest::Url,
        addrs: &[SocketAddr],
    ) -> Result<reqwest::Client, LiveTvError> {
        let host = url.host_str().unwrap_or_default().to_owned();
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(guide::GUIDE_FETCH_TIMEOUT)
            .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
            .resolve_to_addrs(&host, addrs)
            .build()
            .map_err(|error| {
                LiveTvError::DeviceUnavailable(format!("building guide HTTP client: {error}"))
            })
    }

    fn guide_client(&self) -> Result<reqwest::Client, LiveTvError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(guide::GUIDE_FETCH_TIMEOUT)
            .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                LiveTvError::DeviceUnavailable(format!("building guide HTTP client: {error}"))
            })
    }

    /// The refresh loop. It owns the cadence and the invalidation; every
    /// serving read is a cache hit by construction. It runs only on the owner
    /// and only while a source is configured, and it re-reads settings each
    /// tick so turning the guide on or off takes effect without a restart.
    pub(crate) async fn guide_refresh_loop(self: Arc<Self>, shutdown: CancellationToken) {
        let mut delay = guide::GUIDE_REFRESH_INTERVAL;
        loop {
            if let Ok(config) = self.config().await {
                let ours = config.owner_node_id == self.node_id;
                let stale_generation = self
                    .guide_cache
                    .cached_generation()
                    .await
                    .is_some_and(|generation| generation != config.generation);
                if stale_generation || !ours || !config.guide_fetches() {
                    self.guide_cache.invalidate().await;
                    self.clear_guide_titles();
                }
                if ours && config.guide_fetches() && self.serving.admit().is_some() {
                    match self.refresh_guide(&config, false).await {
                        Ok(_) => delay = guide::GUIDE_REFRESH_INTERVAL,
                        Err(error) => {
                            // Only a cold lineup comes back sooner. Every other
                            // failure is the source or the network, and retrying
                            // either a minute apart helps nobody.
                            delay = if self.cached_lineup(&config).await.is_empty() {
                                guide::GUIDE_COLD_LINEUP_RETRY
                            } else {
                                guide::GUIDE_REFRESH_INTERVAL
                            };
                            tracing::warn!(
                                source = config.guide_source.as_str(),
                                code = error.code(),
                                "programme guide refresh failed on the tuner owner"
                            );
                        }
                    }
                } else {
                    delay = guide::GUIDE_REFRESH_INTERVAL;
                    self.metrics
                        .observe_guide_refresh(config.guide_source, "skipped");
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }
}

impl LiveTvMetrics {
    pub(crate) fn prometheus(&self) -> String {
        let projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = tokio::time::Instant::now();
        let config = projection
            .config
            .filter(|(_, _, observed)| now.duration_since(*observed) <= Duration::from_secs(3));
        let enabled = match config {
            Some((_, true, _)) => "1",
            Some((_, false, _)) => "0",
            None => "NaN",
        };
        let device = projection.device.filter(|(generation, observed, _, _)| {
            config.is_some_and(|(current, enabled, _)| enabled && current == *generation)
                && now.duration_since(*observed) <= SNAPSHOT_TTL
        });
        let device_ready = device.is_some();
        let (_, _, ready_channels, drm_channels) = device.unwrap_or((0, now, 0, 0));
        drop(projection);
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
            enabled,
            u8::from(device_ready),
            ready_channels,
            drm_channels,
            self.starts_created.load(Ordering::Acquire),
            self.starts_recovered.load(Ordering::Acquire),
            self.starts_failed.load(Ordering::Acquire),
            self.ended.load(Ordering::Acquire),
            self.relay_bytes.load(Ordering::Acquire),
        ) + &self.guide_prometheus()
    }
}

impl LiveTvManager {
    pub(crate) fn relay_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.metrics.relay_bytes)
    }

    fn session(&self, capability: &str) -> Result<Arc<LiveTvSession>, LiveTvError> {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = registry.sessions.get(capability).cloned() {
            return Ok(session);
        }
        let now = tokio::time::Instant::now();
        registry
            .terminals
            .retain(|_, tombstone| tombstone.expires_at > now);
        if let Some(tombstone) = registry.terminals.get(capability) {
            return Err(tombstone.error.clone());
        }
        Err(LiveTvError::CapabilityExpired(
            "live-TV capability expired".into(),
        ))
    }
}

struct ScratchClaim<'a> {
    claims: &'a StdMutex<HashSet<PathBuf>>,
    path: PathBuf,
}

impl Drop for ScratchClaim<'_> {
    fn drop(&mut self) {
        self.claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
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

#[derive(Clone, Debug)]
struct ScratchInventory {
    playlist: Vec<u8>,
    media_sequence: u64,
    segments: HashMap<u64, (String, u64)>,
}

async fn wait_for_startup(
    session: Arc<LiveTvSession>,
    recovered: bool,
) -> Result<LiveTvProvisional, LiveTvError> {
    struct StartupWaiter {
        session: Arc<LiveTvSession>,
    }

    impl Drop for StartupWaiter {
        fn drop(&mut self) {
            let previous = self.session.startup_waiters.fetch_sub(1, Ordering::AcqRel);
            let unpublished = self
                .session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .startup
                .is_none();
            if previous == 1 && unpublished {
                // The final HTTP waiter disappeared before a provisional
                // capability was delivered.  Cancellation owns cleanup; an
                // abandoned exchange must not leave a tuner running until the
                // longer provisional timeout.
                self.session.cancel.cancel();
            }
        }
    }

    session.startup_waiters.fetch_add(1, Ordering::AcqRel);
    let _waiter = StartupWaiter {
        session: Arc::clone(&session),
    };
    loop {
        // Registered before the state is read, so a publication between the two
        // cannot be missed.
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
        let now = tokio::time::Instant::now();
        // The same decision the producer makes, plus the two seconds that let
        // the producer's own verdict reach this waiter first -- a caller should
        // hear why a start failed, not just that it timed out.
        if let Some(reason) = startup_overdue(
            session.started,
            now + Duration::from_secs(2),
            session.tuner_bytes.load(Ordering::Acquire),
        ) {
            return Err(LiveTvError::StartupTimeout(reason));
        }
        // Waiting in slices rather than to the deadline: the budget can grow
        // while this request is parked, and a wait pinned to the budget in
        // force when it began would refuse a channel that started feeding a
        // moment later.
        let _ = tokio::time::timeout(STARTUP_BUDGET_REVIEW, notified).await;
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
    if !config.admission_ready() {
        return Err(LiveTvError::OwnerUnavailable(
            "the prior tuner owner has not acknowledged cleanup; use Developer recovery only after physically stopping it"
                .into(),
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
    // Join the child and its bounded stderr reader before classifying startup
    // failure; otherwise EOF can beat the decoder diagnostic to the waiter.
    let cleanup = cleanup_session(&session).await;
    let result =
        classify_live_source_error(result, session.decoder_unavailable.load(Ordering::Acquire));
    {
        let error = result.as_ref().err().cloned().unwrap_or_else(|| {
            LiveTvError::CapabilityExpired("the live-TV session stopped".into())
        });
        let mut state = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.startup.is_none() {
            state.startup = Some(Err(error.clone()));
        }
        state.phase = LiveTvSessionPhase::Failed;
        state.error = Some(sanitize_error(&error.to_string()));
        state.terminal_error = Some(error.clone());
        drop(state);
        session.changed.notify_waiters();
    }
    session
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cleanup = Some(cleanup.clone());
    session.changed.notify_waiters();
    if let Some(manager) = manager.upgrade() {
        if let Err(error) = cleanup {
            tracing::error!(
                kind = %error,
                "Live TV session cleanup could not remove scratch before its deadline"
            );
            return;
        }
        manager.retire_session(&session);
    }
}

fn classify_live_source_error(
    result: Result<(), LiveTvError>,
    decoder_unavailable: bool,
) -> Result<(), LiveTvError> {
    if decoder_unavailable
        && matches!(
            result,
            Err(LiveTvError::StreamFailed(_) | LiveTvError::StartupTimeout(_))
        )
    {
        Err(LiveTvError::CodecUnsupported("The tuner owner's FFmpeg could not detect or decode the required video and audio. ATSC 3.0 may require HEVC and AC-4 support; try an ATSC 1.0 channel or a decoder-capable FFmpeg build.".into()))
    } else {
        result
    }
}

async fn remove_session_directory(path: &Path) -> Result<(), LiveTvError> {
    let deadline = tokio::time::Instant::now() + SESSION_DRAIN_TIMEOUT;
    loop {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if tokio::time::Instant::now() < deadline => {
                tracing::warn!(kind = %error.kind(), "retrying Live TV scratch cleanup");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(LiveTvError::StreamFailed(format!(
                    "removing live-TV scratch: {error}"
                )))
            }
        }
    }
}

async fn cleanup_session(session: &LiveTvSession) -> Result<(), LiveTvError> {
    // The mutex also serializes retries after a failed physical cleanup.
    let mut process = session.process.lock().await;
    if let Some(process) = process.as_mut() {
        let _ = process.child.start_kill();
        tokio::time::timeout(SESSION_DRAIN_TIMEOUT, process.child.wait())
            .await
            .map_err(|_| {
                LiveTvError::StreamFailed(
                    "FFmpeg exit was not confirmed before the cleanup deadline".into(),
                )
            })?
            .map_err(|error| {
                LiveTvError::StreamFailed(format!("reaping live-TV FFmpeg: {error}"))
            })?;
        if let Some(mut stderr) = process.stderr.take() {
            if tokio::time::timeout(Duration::from_millis(250), &mut stderr)
                .await
                .is_err()
            {
                stderr.abort();
                let _ = stderr.await;
            }
        }
    }
    // A wait failure above retains both the child and the encoder reservation.
    process.take();
    remove_session_directory(&session.directory).await
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
    let admission = tokio::select! {
        biased;
        _ = session.cancel.cancelled() => {
            return Err(LiveTvError::CapabilityExpired(
                "the live-TV start was cancelled before admission".into(),
            ));
        }
        admission = owner.transcode.admit_live_tv(config.output_height, ADMISSION_WAIT) => {
            admission.map_err(LiveTvError::Capacity)?
        }
    };
    {
        let mut state = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.encoder = admission.encoder.label().to_owned();
    }
    let transcode_plan = LiveTvTranscodePlan::new(
        &owner.system,
        admission.encoder,
        admission.software_threads(),
        config.output_height,
    )?;
    tokio::select! {
        biased;
        _ = session.cancel.cancelled() => {
            return Err(LiveTvError::CapabilityExpired(
                "the live-TV start was cancelled before its owner fence".into(),
            ));
        }
        result = ensure_session_fence(&owner, session) => result?,
    }
    let scratch_creation = tokio::select! {
        biased;
        _ = session.cancel.cancelled() => {
            return Err(LiveTvError::CapabilityExpired(
                "the live-TV start was cancelled before scratch creation".into(),
            ));
        }
        guard = owner.scratch_sweep_gate.lock() => guard,
    };
    // Filesystem work may already be running on Tokio's blocking pool when
    // cancelled. Join directory creation before cleanup can remove its result.
    tokio::fs::create_dir_all(&session.directory)
        .await
        .map_err(|error| LiveTvError::StreamFailed(format!("creating live-TV scratch: {error}")))?;
    drop(scratch_creation);

    let client = owner.client.as_ref().map_err(|_| {
        LiveTvError::DeviceUnavailable("the HDHomeRun HTTP client is unavailable".into())
    })?;
    // Headers only. No byte can have been delivered before the response
    // arrives, so this is the short budget by construction.
    let startup_deadline = session.started + STARTUP_TIMEOUT;
    let response = tokio::select! {
        biased;
        _ = session.cancel.cancelled() => {
            return Err(LiveTvError::CapabilityExpired(
                "the live-TV start was cancelled before tuner headers".into(),
            ));
        }
        response = open_tuner_stream(client, stream_url, startup_deadline) => response?,
    };
    let mut child = spawn_live_ffmpeg(&owner.system, &transcode_plan, &session.directory)?;
    let stdin = child.stdin.take();
    let stderr = child.stderr.take().map(|stderr| {
        tokio::spawn(capture_live_stderr(
            stderr,
            Arc::clone(&session.decoder_unavailable),
        ))
    });
    let Some(stdin) = stdin else {
        *session.process.lock().await = Some(LiveTvProcess {
            child,
            _admission: admission,
            stderr,
        });
        return Err(LiveTvError::StreamFailed(
            "live-TV FFmpeg did not expose stdin".into(),
        ));
    };
    let pump = tokio::spawn(pump_tuner_stream(
        response,
        stdin,
        session.cancel.clone(),
        Arc::clone(&session.tuner_bytes),
    ));
    let serving = owner.serving.clone();
    drop(owner);

    let mut tick = tokio::time::interval(SESSION_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0u64;
    let mut published = false;
    let observe = async {
        loop {
            tick.tick().await;
            if session.cancel.is_cancelled() {
                break Ok(());
            }
            if let Some(status) = child.try_wait().map_err(|error| {
                LiveTvError::StreamFailed(format!("waiting for FFmpeg: {error}"))
            })? {
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
                        if inventory.media_sequence > state.media_sequence
                            || state.publication.is_none()
                        {
                            state.last_progress = now;
                        }
                        state.media_sequence = inventory.media_sequence;
                        // Manifest bytes and their complete current/deletion-lag
                        // inventory cross the lock together. A client can never
                        // receive a playlist from one sample and authorization
                        // from another.
                        state.publication = Some(inventory);
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
                        state.provisional_at = Some(now);
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
            if !published {
                if let Some(reason) = startup_overdue(
                    session.started,
                    now,
                    session.tuner_bytes.load(Ordering::Acquire),
                ) {
                    break Err(LiveTvError::StartupTimeout(reason));
                }
            }
            {
                let state = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if published && provisional_expired(&state, now) {
                    break Err(LiveTvError::CapabilityExpired(
                        "the provisional live-TV start was not activated".into(),
                    ));
                }
                if state.activated
                    && now.duration_since(state.last_touch) >= CAPABILITY_IDLE_TIMEOUT
                {
                    break Err(LiveTvError::CapabilityExpired(
                        "the live-TV capability became idle".into(),
                    ));
                }
                if published && now.duration_since(state.last_progress) >= PRODUCER_PROGRESS_TIMEOUT
                {
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
        }
    };
    let authority_lost = async {
        loop {
            if !serving.is_current(session.owner_serving_generation) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    // No Store or filesystem wait may delay cancellation of the upstream and
    // child. Every observer exit, including `?`, flows through the join below.
    let terminal = tokio::select! {
        biased;
        _ = session.cancel.cancelled() => Ok(()),
        _ = authority_lost => Err(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.into(),
        )),
        result = observe => result,
    };

    session.cancel.cancel();
    pump.abort();
    let _ = pump.await;
    *session.process.lock().await = Some(LiveTvProcess {
        child,
        _admission: admission,
        stderr,
    });
    terminal
}

/// Whether a session that has not published a segment yet is out of time, and
/// what to tell the caller.
///
/// One decision, consulted by both the producer loop and the request waiting on
/// it, so the two can never disagree about whether a start has failed. It reads
/// the bytes the tuner has actually delivered because that is what separates
/// "this channel is slow to start" from "this channel is not there": the first
/// deserves the producer-progress budget, the second deserves a prompt answer.
fn startup_overdue(
    started: tokio::time::Instant,
    now: tokio::time::Instant,
    tuner_bytes: u64,
) -> Option<String> {
    if tuner_bytes == 0 {
        return (now.duration_since(started) >= STARTUP_TIMEOUT).then(|| {
            "the tuner sent no data before the start budget ran out; the channel may have \
             no signal on this device"
                .to_owned()
        });
    }
    (now.duration_since(started) >= STARTUP_FEEDING_TIMEOUT)
        .then(|| format!("the tuner sent {tuner_bytes} bytes but no complete live segment in time"))
}

fn provisional_expired(state: &LiveTvSessionState, now: tokio::time::Instant) -> bool {
    !state.activated
        && state
            .provisional_at
            .is_some_and(|published_at| now.duration_since(published_at) >= PROVISIONAL_TIMEOUT)
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
            tracing::warn!(
                kind = reqwest_error_kind(&error),
                "HDHomeRun stream request failed on the tuner owner"
            );
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

/// A tuner stream cannot expose codec/profile facts until after its HTTP body
/// is flowing. Freeze the complete route that is knowable before spawn and
/// state the remaining input contract explicitly: software auto-detection of
/// the first video stream. This keeps Live TV out of the movie-plan cache
/// namespace without leaving a command-construction bypass that re-infers the
/// encoder or decoder route.
#[derive(Debug, Clone)]
struct LiveTvTranscodePlan {
    encoder: Encoder,
    software_threads: Option<u32>,
    output_height: u16,
    video_map: &'static str,
    force_idr: bool,
}

impl LiveTvTranscodePlan {
    fn new(
        system: &SystemInfo,
        encoder: Encoder,
        software_threads: Option<u32>,
        output_height: u16,
    ) -> Result<Self, LiveTvError> {
        if system.ffmpeg.trim().is_empty() {
            return Err(LiveTvError::CodecUnsupported(
                "FFmpeg is not configured on the tuner owner".into(),
            ));
        }
        if output_height == 0 || software_threads == Some(0) {
            return Err(LiveTvError::CodecUnsupported(
                "the live-TV transcode execution contract is invalid".into(),
            ));
        }
        Ok(Self {
            encoder,
            software_threads,
            output_height,
            video_map: "0:v:0",
            force_idr: system.encoders.forced_idr.wanted_by(encoder),
        })
    }
}

fn spawn_live_ffmpeg(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
) -> Result<tokio::process::Child, LiveTvError> {
    let mut command = live_ffmpeg_command(system, plan, directory)?;
    command
        .spawn()
        .map_err(|error| LiveTvError::CodecUnsupported(format!("starting live-TV FFmpeg: {error}")))
}

fn live_ffmpeg_command(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
) -> Result<tokio::process::Command, LiveTvError> {
    let playlist = directory.join("index.m3u8");
    let segments = directory.join("segment-%06d.ts");
    let mut command = tokio::process::Command::new(&system.ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-nostdin", "-y"])
        .args(plan.encoder.init_args())
        .args(["-hwaccel", "none"])
        .args(["-fflags", "+genpts+discardcorrupt"])
        .args(live_probe_args())
        .args([
            "-i",
            "pipe:0",
            "-map",
            plan.video_map,
            "-map",
            "0:a:0",
            "-sn",
            "-dn",
        ]);
    let filter = live_video_filter(plan.encoder, plan.output_height);
    command.args(["-vf", &filter]);
    command.args(plan.encoder.encode_args(
        if plan.output_height == 1080 {
            8_000
        } else {
            4_000
        },
        EffectiveRateControl::Vbr,
        plan.force_idr,
        plan.software_threads,
    ));
    command.args(LIVE_HLS_OUTPUT_ARGS);
    command
        .arg(segments)
        .arg(playlist)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

/// How long FFmpeg may look at a tuner before it has to start producing.
///
/// `-probesize` is a BYTE budget. A file delivers those bytes at disk speed; a
/// tuner delivers them at the broadcaster's rate, so on a live stream it is a
/// time budget in disguise. Measured on a real HDHomeRun FLEX 4K, running this
/// exact argument list against the strongest channel on the antenna (96-100%
/// signal, ~6.9 Mbps):
///
/// | probesize | first segment |
/// |-----------|---------------|
/// | 8192 KiB  | 7.06 s        |
/// | 2048 KiB  | 5.55 s        |
/// |  512 KiB  | 5.54 s        |
///
/// With the one-second startup HLS cadence in place, a second production
/// measurement at 1080p showed the remaining bounds were additive: 2 MiB / 5 s
/// took 3.33 s on ATSC 1.0 and 9.99 s on ATSC 3.0, while 512 KiB / 1 s took
/// 1.92 s and 6.47 s respectively. Both sources published a fetchable first
/// playlist, so use the smaller pair already covered by the original sweep.
fn live_probe_args() -> [&'static str; 4] {
    ["-probesize", "524288", "-analyzeduration", "1000000"]
}

fn live_video_filter(encoder: Encoder, height: u16) -> String {
    let mut filter =
        format!("bwdif=mode=send_frame:parity=auto:deint=interlaced,scale=-2:{height}");
    match encoder {
        // ATSC 3.0 HEVC Main 10 reaches this graph as P010. Uploading it
        // unchanged creates a 10-bit QSV surface, which h264_qsv refuses.
        // Convert to the live H.264 contract's 8-bit format before upload.
        Encoder::Qsv => {
            filter.push_str(",format=nv12,");
            filter.push_str(
                encoder
                    .filter_suffix()
                    .expect("QSV always has a hardware-upload suffix"),
            );
        }
        _ => match encoder.filter_suffix() {
            // VAAPI already converts to nv12 before its hardware upload.
            Some(suffix) => {
                filter.push(',');
                filter.push_str(suffix);
            }
            // Nothing else pins one, and the source's own format survives the
            // chain. Live TV's output is SDR 8-bit H.264 by contract -- the
            // software encoder pins `-profile:v high` -- so a 10-bit broadcast
            // reaching libx264 as yuv420p10le makes x264 refuse the profile
            // outright. Converting here keeps one output contract for every
            // client.
            None => filter.push_str(",format=yuv420p"),
        },
    }
    filter
}

async fn capture_live_stderr(
    mut stderr: impl tokio::io::AsyncRead + Unpin,
    decoder_unavailable: Arc<AtomicBool>,
) {
    // Drain forever without retaining an unbounded log or exposing device
    // metadata. A rolling 2 KiB window also recognizes split reads.
    let mut window = Vec::with_capacity(2048);
    let mut chunk = [0; 1024];
    while let Ok(read) = stderr.read(&mut chunk).await {
        if read == 0 {
            return;
        }
        window.extend_from_slice(&chunk[..read]);
        if window.len() > 2048 {
            window.drain(..window.len() - 2048);
        }
        let text = String::from_utf8_lossy(&window).to_ascii_lowercase();
        if text.contains("decoding requested, but no decoder found for:")
            || (text.contains("decoder (codec ") && text.contains(") not found for input stream"))
            // FFmpeg 8.1.2 can redact the requested map between the quotes in
            // this exact live-TV child. Both mapped streams are mandatory, so
            // every stream-map failure proves this profile cannot be produced
            // from the tuner input; it does not identify a decoder.
            || (text.contains("stream map") && text.contains("matches no streams"))
        {
            decoder_unavailable.store(true, Ordering::Release);
        }
    }
}

async fn pump_tuner_stream(
    response: reqwest::Response,
    mut stdin: tokio::process::ChildStdin,
    cancel: CancellationToken,
    delivered: Arc<AtomicU64>,
) -> Result<(), LiveTvError> {
    // One task owns both the response and stdin. Aborting and joining this
    // task therefore releases the actual tuner connection, not a detached
    // reader waiting behind a full channel. Backpressure retains one chunk.
    let mut stream = response.bytes_stream();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            next = tokio::time::timeout(TUNER_READ_TIMEOUT, stream.next()) => next,
        }
        .map_err(|_| {
            LiveTvError::StreamFailed("the HDHomeRun stream stopped producing bytes".into())
        })?;
        let Some(bytes) = next else {
            return Err(LiveTvError::StreamFailed(
                "the HDHomeRun stream ended".into(),
            ));
        };
        let bytes = bytes.map_err(|error| {
            tracing::warn!(
                kind = reqwest_error_kind(&error),
                "HDHomeRun stream body failed"
            );
            LiveTvError::StreamFailed("the HDHomeRun stream body failed".into())
        })?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(TUNER_READ_TIMEOUT, stdin.write_all(&bytes)) => {
                result.map_err(|_| LiveTvError::StreamFailed("FFmpeg input stalled".into()))?
                    .map_err(|error| LiveTvError::StreamFailed(format!("writing FFmpeg input: {error}")))?;
                // Counted after the write, so it means "reached the graph"
                // rather than "arrived in this process".
                delivered.fetch_add(bytes.len() as u64, Ordering::Release);
            }
        }
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
        let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(metadata) => metadata,
            // FFmpeg atomically renames temporary output and deletes the
            // oldest window entry while this bounded scan is walking. That
            // expected churn means this sample is incomplete, not terminal.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LiveTvError::StreamFailed(format!(
                    "checking live-TV scratch: {error}"
                )))
            }
        };
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
    for sequence in &parsed.segments {
        let Some((_, len)) = final_segments.get(sequence) else {
            return Ok(None);
        };
        if *len == 0 {
            return Ok(None);
        }
    }
    let unlisted = final_segments
        .keys()
        .filter(|sequence| !parsed.segments.contains(sequence))
        .count();
    if parsed.segments.len() > MAX_LISTED_SEGMENTS || unlisted > MAX_DELETION_LAG_SEGMENTS {
        return Err(LiveTvError::StreamFailed(
            "live-TV scratch exceeded its segment inventory budget".into(),
        ));
    }
    Ok(Some(ScratchInventory {
        playlist,
        media_sequence: parsed.media_sequence,
        // Include FFmpeg's one deletion-lag segment. A browser which received
        // the immediately previous manifest may still request it after the
        // current manifest is sampled; discarding it here turned a physically
        // present, valid segment into a false 410.
        segments: final_segments,
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
    let read = async {
        let file = open_regular_file(path).await?;
        let metadata = file
            .metadata()
            .await
            .map_err(|_| LiveTvError::CapabilityExpired("live-TV resource expired".into()))?;
        if !metadata.is_file() || metadata.len() > max {
            return Err(LiveTvError::StreamFailed(
                "live-TV resource exceeds its file budget".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(max + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| LiveTvError::CapabilityExpired("live-TV resource expired".into()))?;
        if bytes.len() as u64 > max {
            return Err(LiveTvError::StreamFailed(
                "live-TV resource exceeds its byte budget".into(),
            ));
        }
        Ok(bytes)
    };
    tokio::time::timeout(LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT, read)
        .await
        .map_err(|_| LiveTvError::StreamFailed("live-TV resource read timed out".into()))?
}

async fn open_bounded_segment(
    path: &Path,
    expected_len: u64,
) -> Result<tokio::fs::File, LiveTvError> {
    if expected_len > MAX_SESSION_BYTES {
        return Err(LiveTvError::StreamFailed(
            "live-TV segment exceeds its byte budget".into(),
        ));
    }

    let file = open_regular_file(path).await?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| LiveTvError::CapabilityExpired("live segment expired".into()))?;
    if !metadata.is_file() || metadata.len() != expected_len {
        return Err(LiveTvError::StreamFailed(
            "live segment failed its opened-file inventory check".into(),
        ));
    }
    Ok(file)
}

async fn open_regular_file(path: &Path) -> Result<tokio::fs::File, LiveTvError> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
        })
        .await
        .map_err(|error| LiveTvError::StreamFailed(format!("opening live segment: {error}")))?
        .map(tokio::fs::File::from_std)
        .map_err(|_| LiveTvError::CapabilityExpired("live segment expired".into()))?
    };
    #[cfg(not(unix))]
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .await
        .map_err(|_| LiveTvError::CapabilityExpired("live segment expired".into()))?;

    Ok(file)
}

struct LocalResourceState {
    receiver: tokio::sync::mpsc::Receiver<Result<bytes::Bytes, io::Error>>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    error: Option<io::Error>,
    finished: bool,
    last_progress: tokio::time::Instant,
    waker: Option<std::task::Waker>,
}

impl LocalResourceState {
    fn fail(&mut self, error: io::Error) {
        if self.finished {
            return;
        }
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
        self.permit.take();
        self.error = Some(error);
        self.finished = true;
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

struct LocalResourceStream {
    state: Arc<StdMutex<LocalResourceState>>,
    cancel: CancellationToken,
}

impl futures_util::Stream for LocalResourceStream {
    type Item = Result<bytes::Bytes, io::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = state.error.take() {
            return std::task::Poll::Ready(Some(Err(error)));
        }
        if state.finished {
            return std::task::Poll::Ready(None);
        }
        state.waker = Some(cx.waker().clone());
        match state.receiver.poll_recv(cx) {
            std::task::Poll::Ready(Some(item)) => {
                state.last_progress = tokio::time::Instant::now();
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                state.finished = true;
                state.permit.take();
                self.cancel.cancel();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for LocalResourceStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn bounded_segment_body(
    mut file: tokio::fs::File,
    expected_len: u64,
    cancel: CancellationToken,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Body {
    // A child cancellation token prevents one slow response from stopping the
    // viewing session. Its independent watchdog releases even an unpolled
    // body's queue and admission permit, not just the producer task.
    let cancel = cancel.child_token();
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<bytes::Bytes, io::Error>>(PUMP_CHANNEL_CAPACITY);
    let deadline = tokio::time::Instant::now() + LOCAL_RESOURCE_TOTAL_TIMEOUT;
    let state = Arc::new(StdMutex::new(LocalResourceState {
        receiver,
        permit: Some(permit),
        error: None,
        finished: false,
        last_progress: tokio::time::Instant::now(),
        waker: None,
    }));
    let watchdog_state = Arc::clone(&state);
    let watchdog_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let progress_deadline = {
                let state = watchdog_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.finished {
                    return;
                }
                deadline.min(state.last_progress + LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT)
            };
            tokio::select! {
                biased;
                _ = watchdog_cancel.cancelled() => {
                    watchdog_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                        .fail(io::Error::new(io::ErrorKind::Interrupted, "live-TV response cancelled"));
                    return;
                }
                _ = tokio::time::sleep_until(progress_deadline) => {}
            }
            let mut state = watchdog_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = tokio::time::Instant::now();
            if now >= deadline || now >= state.last_progress + LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT {
                state.fail(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "live-TV response deadline exceeded",
                ));
                watchdog_cancel.cancel();
                return;
            }
        }
    });
    let producer_cancel = cancel.clone();
    tokio::spawn(async move {
        let cancel = producer_cancel;
        let mut sent = 0_u64;
        let mut buffer = vec![0_u8; LOCAL_RESOURCE_CHUNK_BYTES];
        loop {
            let remaining = expected_len.saturating_sub(sent);
            if remaining == 0 {
                return;
            }
            let read_len = buffer
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let read = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = tokio::time::timeout_at(deadline, file.read(&mut buffer[..read_len])) => {
                    match result {
                        Ok(Ok(read)) => read,
                        Ok(Err(error)) => {
                            let _ = sender.try_send(Err(error));
                            return;
                        }
                        Err(_) => {
                            let _ = sender.try_send(Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "live-TV segment exceeded its total response deadline",
                            )));
                            return;
                        }
                    }
                }
            };
            if read == 0 {
                let _ = sender.try_send(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "live-TV segment changed while streaming",
                )));
                return;
            }
            sent = sent.saturating_add(read as u64);
            let chunk = bytes::Bytes::copy_from_slice(&buffer[..read]);
            let progress_deadline =
                deadline.min(tokio::time::Instant::now() + LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT);
            let delivered = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = tokio::time::timeout_at(progress_deadline, sender.send(Ok(chunk))) => result,
            };
            if !matches!(delivered, Ok(Ok(()))) {
                return;
            }
        }
    });
    Body::from_stream(LocalResourceStream { state, cancel })
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
    video_codec: Option<String>,
    audio_codec: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TunerStatusDocument {
    resource: Option<String>,
    vct_number: Option<String>,
    #[serde(rename = "TargetIP")]
    target_ip: Option<String>,
    signal_strength_percent: Option<u16>,
    signal_quality_percent: Option<u16>,
    symbol_quality_percent: Option<u16>,
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

fn optional_lineup_marker(value: Option<&serde_json::Value>) -> Option<bool> {
    match value {
        Some(serde_json::Value::Bool(value)) => Some(*value),
        Some(serde_json::Value::Number(value)) if value.as_u64() == Some(0) => Some(false),
        Some(serde_json::Value::Number(value)) if value.as_u64() == Some(1) => Some(true),
        _ => None,
    }
}

fn channel_format(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_CHANNEL_FORMAT_BYTES
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
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
        let hd = optional_lineup_marker(raw_row.get("HD"));
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
            hd,
            video_codec: channel_format(row.video_codec.as_deref()),
            audio_codec: channel_format(row.audio_codec.as_deref()),
        });
    }
    channels.sort_by(|left, right| guide_order(&left.guide_number, &right.guide_number));
    Ok(channels)
}

fn tuner_signal_for_channel(
    rows: Vec<serde_json::Value>,
    guide_number: &str,
) -> Option<LiveTvSignalStatus> {
    let percent = |value: Option<u16>| value.filter(|value| *value <= 100).map(|value| value as u8);
    rows.into_iter()
        .filter_map(|row| serde_json::from_value::<TunerStatusDocument>(row).ok())
        .find_map(|row| {
            let busy = row
                .target_ip
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            let channel_matches = row.vct_number.as_deref() == Some(guide_number);
            // Resource is not displayed, but requiring its short documented
            // tuner identity rejects a malformed row before any readings are
            // accepted into the public capability response.
            let resource_valid = row.resource.as_deref().is_some_and(|value| {
                !value.is_empty() && value.len() <= 32 && !value.chars().any(char::is_control)
            });
            if !busy || !channel_matches || !resource_valid {
                return None;
            }
            let signal = LiveTvSignalStatus {
                strength_percent: percent(row.signal_strength_percent),
                quality_percent: percent(row.signal_quality_percent),
                symbol_quality_percent: percent(row.symbol_quality_percent),
            };
            (signal.strength_percent.is_some()
                || signal.quality_percent.is_some()
                || signal.symbol_quality_percent.is_some())
            .then_some(signal)
        })
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
    let body = fetch_bounded(client, url, MAX_DOCUMENT_BYTES, DOCUMENT_TIMEOUT).await?;
    serde_json::from_slice(&body)
        .map_err(|_| LiveTvError::InvalidResponse("HDHomeRun returned invalid JSON".to_owned()))
}

/// One bounded GET: a single deadline covering headers and body, a declared
/// `Content-Length` refused before a byte is read, and the same cap enforced
/// while accumulating chunks so a lying header buys nothing. The guide fetches
/// a much larger document than the lineup does and needs a longer deadline,
/// which is the whole reason the bound and the deadline are parameters rather
/// than the two constants this started as.
pub(crate) async fn fetch_bounded(
    client: &reqwest::Client,
    url: reqwest::Url,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Vec<u8>, LiveTvError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut response = tokio::time::timeout_at(deadline, client.get(url).send())
        .await
        .map_err(|_| LiveTvError::DeviceUnavailable("HDHomeRun request timed out".to_owned()))?
        .map_err(|error| {
            tracing::warn!(
                kind = reqwest_error_kind(&error),
                "HDHomeRun document request failed on the tuner owner"
            );
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
        .is_some_and(|length| length > max_bytes as u64)
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
                tracing::warn!(
                    kind = reqwest_error_kind(&error),
                    "HDHomeRun response body failed on the tuner owner"
                );
                LiveTvError::DeviceUnavailable("HDHomeRun response body failed".to_owned())
            })?;
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(LiveTvError::InvalidResponse(
                "HDHomeRun document is too large".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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
    let filter = live_video_filter(encoder, height);
    command.args(["-vf", &filter]);
    command.args(encoder.encode_args(
        if height == 1080 { 8_000 } else { 4_000 },
        EffectiveRateControl::Vbr,
        system.encoders.forced_idr.wanted_by(encoder),
        (encoder == Encoder::Software).then_some(2),
    ));
    command.args(LIVE_HLS_OUTPUT_ARGS);
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

fn reqwest_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

pub(crate) fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::guide::LiveTvProgramme;
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

    fn test_manager(root: &Path) -> Arc<LiveTvManager> {
        test_manager_with_system(root, SystemInfo::default())
    }

    fn test_manager_with_system(root: &Path, system: SystemInfo) -> Arc<LiveTvManager> {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::{EncoderCaps, Pipeline};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let transcode = Arc::new(crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            root.join("finite"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        LiveTvManager::new(
            store,
            Arc::new(system),
            transcode,
            crate::serving_fence::ServingAuthority::always_ready(),
            "node-a".into(),
            root.join("live-tv"),
        )
    }

    fn test_session(directory: PathBuf, generation: i64) -> Arc<LiveTvSession> {
        let now = tokio::time::Instant::now();
        Arc::new(LiveTvSession {
            capability: format!(
                "ltv1.{}.{}",
                base64url_encode(b"node-a"),
                uuid::Uuid::new_v4()
            ),
            activation_token: uuid::Uuid::new_v4().to_string(),
            request: LiveTvStartRequest {
                expected_owner_node_id: "node-a".into(),
                source_node_id: "node-a".into(),
                user_id: 1,
                user_name: "viewer".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                channel_id: "7.1".into(),
                config_generation: generation,
                source_serving_generation: 0,
            },
            channel: LiveTvChannel {
                id: "7.1".into(),
                guide_number: "7.1".into(),
                guide_name: "Test".into(),
                favorite: false,
                drm: false,
                support: LiveTvChannelSupport::Ready,
                hd: None,
                video_codec: None,
                audio_codec: None,
            },
            output: LiveTvOutput {
                container: "hls".into(),
                video: "h264".into(),
                audio: "aac".into(),
                height: 720,
            },
            device_ipv4: None,
            owner_serving_generation: 0,
            started: now,
            directory,
            cancel: CancellationToken::new(),
            changed: tokio::sync::Notify::new(),
            startup_waiters: AtomicUsize::new(0),
            worker: StdMutex::new(None),
            resource_admission: Arc::new(tokio::sync::Semaphore::new(LOCAL_RESOURCE_CONCURRENCY)),
            process: tokio::sync::Mutex::new(None),
            decoder_unavailable: Arc::new(AtomicBool::new(false)),
            tuner_bytes: Arc::new(AtomicU64::new(0)),
            state: StdMutex::new(LiveTvSessionState {
                phase: LiveTvSessionPhase::Active,
                startup: None,
                activated: true,
                provisional_at: Some(now),
                last_touch: now,
                last_progress: now,
                media_sequence: 0,
                publication: None,
                encoder: "software".into(),
                error: None,
                terminal_error: None,
                cleanup: None,
            }),
            signal_cache: tokio::sync::Mutex::new(None),
        })
    }

    async fn seed_test_config(manager: &LiveTvManager) {
        manager
            .store
            .put_settings(&[
                (keys::LIVE_TV_ENABLED, "1"),
                (keys::LIVE_TV_DEVICE_IPV4, "192.168.1.20"),
                (keys::LIVE_TV_OWNER_NODE_ID, "node-a"),
                (keys::LIVE_TV_MAX_SESSIONS, "2"),
                (keys::LIVE_TV_OUTPUT_HEIGHT, "720"),
                (keys::LIVE_TV_CONFIG_GENERATION, "1"),
            ])
            .await
            .expect("seed live-TV settings");
    }

    #[tokio::test]
    async fn live_tv_sweeper_preserves_registered_scratch_and_removes_only_orphans() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        tokio::fs::create_dir_all(&manager.scratch_root)
            .await
            .expect("Live TV root");
        let active_path = manager.scratch_root.join("live-tv-active");
        tokio::fs::create_dir(&active_path)
            .await
            .expect("active scratch");
        tokio::fs::write(active_path.join("index.m3u8"), b"active")
            .await
            .expect("active playlist");
        let session = test_session(active_path.clone(), 1);
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .insert(session.capability.clone(), session);
        let orphan = manager.scratch_root.join("live-tv-orphan");
        tokio::fs::create_dir(&orphan)
            .await
            .expect("orphan scratch");

        assert_eq!(manager.sweep_orphan_scratch().await, 1);
        assert!(active_path.join("index.m3u8").is_file());
        assert!(!orphan.exists());
    }

    #[tokio::test]
    async fn drain_acknowledges_only_after_registry_and_scratch_cleanup() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        tokio::fs::create_dir_all(&manager.scratch_root)
            .await
            .expect("Live TV root");
        let path = manager.scratch_root.join("live-tv-drain");
        tokio::fs::create_dir(&path).await.expect("session scratch");
        let session = test_session(path.clone(), 1);
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .insert(session.capability.clone(), Arc::clone(&session));
        let task_manager = Arc::clone(&manager);
        let task_session = Arc::clone(&session);
        tokio::spawn(async move {
            task_session.cancel.cancelled().await;
            tokio::fs::remove_dir_all(&task_session.directory)
                .await
                .expect("physical scratch cleanup");
            task_manager
                .registry
                .lock()
                .expect("registry")
                .sessions
                .remove(&task_session.capability);
            task_session.changed.notify_waiters();
        });

        assert_eq!(manager.drain_before(2).await.expect("confirmed drain"), 1);
        assert!(!path.exists());
        assert!(manager.activities().is_empty());
    }

    #[tokio::test]
    async fn activation_is_bound_to_the_starting_voter_and_serving_generation() {
        let root = crate::test_tempdir().expect("activation root");
        let manager = test_manager(root.path());
        seed_test_config(&manager).await;
        let session = test_session(manager.scratch_root.join("live-tv-activation"), 1);
        {
            let mut state = session.state.lock().expect("session state");
            state.phase = LiveTvSessionPhase::Provisional;
            state.activated = false;
        }
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .insert(session.capability.clone(), Arc::clone(&session));
        let mut request = LiveTvActivateRequest {
            expected_owner_node_id: "node-a".into(),
            capability: session.capability.clone(),
            activation_token: session.activation_token.clone(),
            config_generation: 1,
            source_serving_generation: 0,
        };

        assert!(matches!(
            manager.activate_local(&request, "node-b").await,
            Err(LiveTvError::Conflict(_))
        ));
        request.source_serving_generation = 1;
        assert!(matches!(
            manager.activate_local(&request, "node-a").await,
            Err(LiveTvError::Conflict(_))
        ));
        request.source_serving_generation = 0;
        assert!(
            manager
                .activate_local(&request, "node-a")
                .await
                .expect("matching activation")
                .live
        );

        let mut other_generation = session.request.clone();
        other_generation.source_serving_generation = 1;
        assert_ne!(
            LiveTvRequestKey::from(&session.request),
            LiveTvRequestKey::from(&other_generation),
            "a request replay cannot cross a source serving generation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicUsize;

        let root = crate::test_tempdir().expect("lifecycle root");
        let ffmpeg = root.path().join("fake-ffmpeg");
        std::fs::write(
            &ffmpeg,
            r#"#!/bin/sh
for output do playlist="$output"; done
directory=${playlist%/*}
printf 'transport-stream' > "$directory/segment-000001.ts"
printf '#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:4.000,\nsegment-000001.ts\n' > "$playlist"
exec /bin/cat >/dev/null
"#,
        )
        .expect("fake FFmpeg");
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755))
            .expect("executable fake FFmpeg");
        let system = SystemInfo {
            ffmpeg: ffmpeg.to_string_lossy().into_owned(),
            ..SystemInfo::default()
        };
        let mut manager = test_manager_with_system(root.path(), system);
        seed_test_config(&manager).await;

        let get_count = Arc::new(AtomicUsize::new(0));
        let observed_get_count = Arc::clone(&get_count);
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake tuner");
        let address = listener.local_addr().expect("fake tuner address");
        // Test-only transport injection, not a production URL override: run
        // the actual pinned-address discovery/lineup/start implementation via
        // a loopback HTTP proxy fixture.
        Arc::get_mut(&mut manager)
            .expect("unique test manager")
            .client = Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .proxy(reqwest::Proxy::all(format!("http://{address}")).expect("fixture proxy"))
            .build()
            .expect("fixture client"));
        let tuner = tokio::spawn(async move {
            let mut tuner_stream = None;
            for expected_path in ["/discover.json", "/lineup.json", "/auto/v7.1"] {
                let (mut stream, _) = listener.accept().await.expect("accept tuner request");
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).await.expect("read tuner GET");
                assert!(String::from_utf8_lossy(&request[..read]).contains(expected_path));
                let document = match expected_path {
                    "/discover.json" => Some(
                        r#"{"FriendlyName":"Fixture","ModelNumber":"HDFX-4K","FirmwareVersion":"fixture","DeviceID":"10ABCDEF","TunerCount":4}"#,
                    ),
                    "/lineup.json" => Some(
                        r#"[{"GuideNumber":"7.1","GuideName":"Test","URL":"http://ignored.local:5004/auto/v7.1"}]"#,
                    ),
                    _ => None,
                };
                if let Some(document) = document {
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("fixture document");
                } else {
                    observed_get_count.fetch_add(1, Ordering::Relaxed);
                    tuner_stream = Some(stream);
                }
            }
            let mut stream = tuner_stream.expect("one tuner stream");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("write tuner headers");
            loop {
                if stream.write_all(b"4\r\ndata\r\n").await.is_err() {
                    let _ = closed_tx.send(());
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let request = test_session(root.path().join("unused"), 1).request.clone();
        let provisional = manager
            .start_local(request.clone())
            .await
            .expect("first HLS publication");
        let recovered = manager
            .start_local(request.clone())
            .await
            .expect("same request recovers without GET");
        assert_eq!(recovered.capability, provisional.capability);
        assert_eq!(recovered.outcome, LiveTvStartOutcome::Recovered);
        let session = manager
            .session(&provisional.capability)
            .expect("registered session");
        assert_eq!(provisional.outcome, LiveTvStartOutcome::Created);
        let activated = manager
            .activate_local(
                &LiveTvActivateRequest {
                    expected_owner_node_id: "node-a".into(),
                    capability: provisional.capability.clone(),
                    activation_token: provisional.activation_token,
                    config_generation: 1,
                    source_serving_generation: 0,
                },
                "node-a",
            )
            .await
            .expect("activate live TV");
        assert!(activated.live);

        let playlist = manager
            .resource_local(LiveTvResourceRequest::Playlist {
                capability: provisional.capability.clone(),
            })
            .await
            .expect("serve playlist");
        let playlist = axum::body::to_bytes(playlist.into_body(), MAX_PLAYLIST_BYTES as usize)
            .await
            .expect("playlist bytes");
        assert!(playlist.ends_with(b"segment-000001.ts\n"));
        let segment = manager
            .resource_local(LiveTvResourceRequest::Segment {
                capability: provisional.capability.clone(),
                sequence: 1,
            })
            .await
            .expect("serve segment");
        let segment = axum::body::to_bytes(segment.into_body(), MAX_SEGMENT_BYTES as usize)
            .await
            .expect("segment bytes");
        assert_eq!(&segment[..], b"transport-stream");

        manager
            .stop_local(&provisional.capability)
            .await
            .expect("confirmed stop");
        assert!(
            matches!(
                manager.start_local(request).await,
                Err(LiveTvError::CapabilityExpired(_))
            ),
            "a delayed replay must not open another tuner after cleanup"
        );
        tokio::time::timeout(Duration::from_secs(2), closed_rx)
            .await
            .expect("tuner socket close deadline")
            .expect("tuner socket close signal");
        assert_eq!(get_count.load(Ordering::Relaxed), 1);
        assert!(manager.activities().is_empty());
        assert!(!session.directory.exists());
        tuner.await.expect("fake tuner");
    }

    #[test]
    fn live_filter_deinterlaces_interlaced_frames_only() {
        let filter = live_video_filter(Encoder::Software, 720);
        assert!(filter.contains("bwdif=mode=send_frame:parity=auto:deint=interlaced"));
        assert!(filter.ends_with("scale=-2:720,format=yuv420p"));
    }

    #[tokio::test]
    async fn live_tv_unpolled_segment_releases_queue_and_permit_at_deadline() {
        let root = crate::test_tempdir().expect("response root");
        let path = root.path().join("segment-000001.ts");
        tokio::fs::write(&path, vec![7; LOCAL_RESOURCE_CHUNK_BYTES * 8])
            .await
            .expect("segment");
        let file = open_bounded_segment(&path, (LOCAL_RESOURCE_CHUNK_BYTES * 8) as u64)
            .await
            .expect("bounded file");
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("response slot");
        let parent = CancellationToken::new();
        tokio::time::pause();
        let body = bounded_segment_body(
            file,
            (LOCAL_RESOURCE_CHUNK_BYTES * 8) as u64,
            parent.clone(),
            permit,
        );
        tokio::task::yield_now().await;
        assert_eq!(slots.available_permits(), 0);
        tokio::time::advance(LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            slots.available_permits(),
            1,
            "unpolled Body must not retain a slot"
        );
        assert!(
            !parent.is_cancelled(),
            "one response timeout must not stop the session"
        );
        assert!(axum::body::to_bytes(body, MAX_SEGMENT_BYTES as usize)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn live_tv_dropped_segment_releases_response_without_stopping_session() {
        let root = crate::test_tempdir().expect("response root");
        let path = root.path().join("segment-000001.ts");
        tokio::fs::write(&path, b"segment").await.expect("segment");
        let file = open_bounded_segment(&path, 7).await.expect("bounded file");
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&slots)
            .try_acquire_owned()
            .expect("response slot");
        let parent = CancellationToken::new();
        drop(bounded_segment_body(file, 7, parent.clone(), permit));
        tokio::task::yield_now().await;
        assert_eq!(slots.available_permits(), 1);
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn live_tv_startup_cancels_only_when_final_waiter_disappears() {
        let root = crate::test_tempdir().expect("waiter root");
        let session = test_session(root.path().join("session"), 1);
        let first = tokio::spawn(wait_for_startup(Arc::clone(&session), false));
        let second = tokio::spawn(wait_for_startup(Arc::clone(&session), true));
        tokio::task::yield_now().await;
        assert_eq!(session.startup_waiters.load(Ordering::Acquire), 2);
        first.abort();
        let _ = first.await;
        assert!(!session.cancel.is_cancelled());
        second.abort();
        let _ = second.await;
        assert!(session.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn live_tv_stale_drain_cannot_cancel_newer_sessions_or_lower_admission_floor() {
        let root = crate::test_tempdir().expect("drain root");
        let manager = test_manager(root.path());
        let session = test_session(root.path().join("newer"), 3);
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .insert(session.capability.clone(), Arc::clone(&session));
        assert_eq!(manager.drain_before(3).await.expect("drain"), 0);
        assert_eq!(manager.drain_before(2).await.expect("stale drain"), 0);
        assert!(!session.cancel.is_cancelled());
        assert_eq!(manager.registry.lock().expect("registry").min_generation, 3);
    }

    #[tokio::test]
    async fn live_tv_drain_and_shutdown_fence_real_starts_paused_before_insertion() {
        for shutdown in [false, true] {
            let root = crate::test_tempdir().expect("start race root");
            let mut manager = test_manager(root.path());
            seed_test_config(&manager).await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("fixture listener");
            let address = listener.local_addr().expect("fixture address");
            Arc::get_mut(&mut manager).expect("unique manager").client =
                Ok(reqwest::Client::builder()
                    .proxy(reqwest::Proxy::all(format!("http://{address}")).expect("fixture proxy"))
                    .build()
                    .expect("fixture client"));
            let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let fixture = tokio::spawn(async move {
                let (mut discover, _) = listener.accept().await.expect("discover request");
                let mut request = [0; 2048];
                assert!(discover.read(&mut request).await.expect("discover headers") > 0);
                let document = r#"{"FriendlyName":"Fixture","ModelNumber":"HDFX-4K","FirmwareVersion":"fixture","DeviceID":"10ABCDEF","TunerCount":4}"#;
                discover.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("discover response");
                drop(discover);
                let (mut lineup, _) = listener.accept().await.expect("lineup request");
                assert!(lineup.read(&mut request).await.expect("lineup headers") > 0);
                arrived_tx.send(()).expect("lineup paused");
                release_rx.await.expect("resume lineup");
                let document = r#"[{"GuideNumber":"7.1","GuideName":"Test","URL":"http://ignored.local:5004/auto/v7.1"}]"#;
                lineup.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{document}", document.len()).as_bytes()).await.expect("lineup response");
                drop(lineup);
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "a fenced delayed start must not open a tuner GET"
                );
            });
            let request = test_session(root.path().join("unused"), 1).request.clone();
            let starting_manager = Arc::clone(&manager);
            let starting = tokio::spawn(async move { starting_manager.start_local(request).await });
            tokio::time::timeout(Duration::from_secs(2), arrived_rx)
                .await
                .expect("lineup arrival deadline")
                .expect("lineup arrival");
            if shutdown {
                assert_eq!(manager.shutdown().await.expect("empty shutdown"), 0);
            } else {
                assert_eq!(manager.drain_before(2).await.expect("empty drain ACK"), 0);
            }
            release_tx.send(()).expect("release lineup");
            assert!(matches!(
                starting.await.expect("start task"),
                Err(LiveTvError::Conflict(_))
            ));
            assert!(manager.activities().is_empty());
            assert_eq!(manager.metrics.starts_created.load(Ordering::Acquire), 0);
            fixture.await.expect("fixture task");
        }
    }

    #[tokio::test]
    async fn live_tv_real_video_only_source_cannot_publish_the_aac_profile() {
        plurx_core::testfixtures::require_ffmpeg();
        let root = crate::test_tempdir().expect("audio-required root");
        let ffmpeg = plurx_core::testfixtures::ffmpeg();
        let source = tokio::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=25",
                "-t",
                "0.5",
                "-an",
                "-c:v",
                "mpeg2video",
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .expect("generate video-only MPEG-TS");
        assert!(
            source.status.success(),
            "{}",
            String::from_utf8_lossy(&source.stderr)
        );
        let system = SystemInfo {
            ffmpeg,
            ..SystemInfo::default()
        };
        let plan = LiveTvTranscodePlan::new(&system, Encoder::Software, Some(2), 720)
            .expect("prepare live-TV plan");
        let mut child =
            spawn_live_ffmpeg(&system, &plan, root.path()).expect("start exact production graph");
        let detected = Arc::new(AtomicBool::new(false));
        let stderr = tokio::spawn(capture_live_stderr(
            child.stderr.take().expect("stderr"),
            Arc::clone(&detected),
        ));
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(&source.stdout)
            .await
            .expect("one video-only source");
        drop(stdin);
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("FFmpeg exit deadline")
            .expect("FFmpeg exit");
        stderr.await.expect("bounded stderr collection");
        assert!(
            !status.success(),
            "the H.264/AAC profile must require audio"
        );
        assert!(
            !root.path().join("index.m3u8").exists(),
            "no successful HLS publication without audio"
        );
        assert!(matches!(
            classify_live_source_error(
                Err(LiveTvError::StreamFailed("child exited".into())),
                detected.load(Ordering::Acquire)
            ),
            Err(LiveTvError::CodecUnsupported(_))
        ));
    }

    #[tokio::test]
    async fn live_tv_decoder_diagnostic_capture_is_bounded_and_specific() {
        for (diagnostic, unsupported) in [
            ("Decoding requested, but no decoder found for: ac4", true),
            ("Decoder (codec ac4) not found for input stream #0:1", true),
            ("Stream map '0:a:0' matches no streams.", true),
            ("Stream map '' matches no streams.", true),
            ("Error opening output: no space left on device", false),
            ("Invalid data found when processing input", false),
        ] {
            let (mut writer, reader) = tokio::io::duplex(128);
            let detected = Arc::new(AtomicBool::new(false));
            let capture = tokio::spawn(capture_live_stderr(reader, Arc::clone(&detected)));
            writer
                .write_all(&vec![b'x'; 32 * 1024])
                .await
                .expect("long bounded preamble");
            for byte in diagnostic.as_bytes() {
                writer.write_all(&[*byte]).await.expect("split diagnostic");
                tokio::task::yield_now().await;
            }
            drop(writer);
            capture.await.expect("stderr capture");
            assert_eq!(
                detected.load(Ordering::Acquire),
                unsupported,
                "{diagnostic}"
            );
        }
    }

    #[tokio::test]
    async fn live_tv_cleanup_failure_retains_registry_and_can_be_retried() {
        let root = crate::test_tempdir().expect("cleanup root");
        let manager = test_manager(root.path());
        let path = root.path().join("not-a-directory");
        tokio::fs::write(&path, b"cannot remove as directory")
            .await
            .expect("cleanup obstruction");
        let session = test_session(path.clone(), 1);
        session.state.lock().expect("state").cleanup = Some(Err(LiveTvError::StreamFailed(
            "previous cleanup failed".into(),
        )));
        manager
            .registry
            .lock()
            .expect("registry")
            .sessions
            .insert(session.capability.clone(), Arc::clone(&session));
        assert!(manager.drain_before(2).await.is_err());
        assert_eq!(
            manager.activities().len(),
            1,
            "failed physical cleanup must not acknowledge disappearance"
        );
        tokio::fs::remove_file(&path)
            .await
            .expect("clear fixture obstruction");
        assert_eq!(manager.drain_before(2).await.expect("retry cleanup"), 1);
        assert!(manager.activities().is_empty());
        assert!(matches!(
            manager
                .session_for_request(&LiveTvRequestKey::from(&session.request), &session.request),
            Err(LiveTvError::CapabilityExpired(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn live_tv_metrics_expire_proofs_and_ignore_older_configuration() {
        let root = crate::test_tempdir().expect("metrics root");
        let manager = test_manager(root.path());
        let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        config.generation = 2;
        config.enabled = true;
        manager.observe_config(&config);
        manager
            .metrics
            .projection
            .lock()
            .expect("projection")
            .device = Some((2, tokio::time::Instant::now(), 8, 2));
        assert!(manager
            .metrics
            .prometheus()
            .contains("plurx_live_tv_device_ready 1\n"));
        let mut older = config.clone();
        older.generation = 1;
        older.enabled = false;
        manager.observe_config(&older);
        assert!(manager
            .metrics
            .prometheus()
            .contains("plurx_live_tv_enabled 1\n"));
        tokio::time::advance(Duration::from_secs(4)).await;
        let expired = manager.metrics.prometheus();
        assert!(expired.contains("plurx_live_tv_enabled NaN\n"));
        assert!(expired.contains("plurx_live_tv_device_ready 0\n"));
        manager.observe_config(&config);
        assert!(manager
            .metrics
            .prometheus()
            .contains("plurx_live_tv_device_ready 1\n"));
        tokio::time::advance(SNAPSHOT_TTL).await;
        manager.observe_config(&config);
        assert!(manager
            .metrics
            .prometheus()
            .contains("plurx_live_tv_device_ready 0\n"));
    }

    #[test]
    fn provisional_lease_starts_when_first_media_is_published() {
        let root = crate::test_temp_path(format!("live-tv-{}", uuid::Uuid::new_v4()));
        let session = test_session(root, 1);
        let published_at = tokio::time::Instant::now();
        let mut state = session.state.lock().expect("state");
        state.activated = false;
        state.provisional_at = Some(published_at);
        assert!(!provisional_expired(
            &state,
            published_at + PROVISIONAL_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(provisional_expired(
            &state,
            published_at + PROVISIONAL_TIMEOUT
        ));
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
              {"GuideNumber":"7.1","GuideName":"WABC","Tags":"favorite","HD":1,
               "VideoCodec":"HEVC","AudioCodec":"AC4"},
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
        assert_eq!(channels[0].hd, Some(true));
        assert_eq!(channels[0].video_codec.as_deref(), Some("HEVC"));
        assert_eq!(channels[0].audio_codec.as_deref(), Some("AC4"));
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
    fn tuner_status_reports_only_the_busy_matching_channels_bounded_signal() {
        let rows = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[
              {"Resource":"tuner0","VctNumber":"7.1","TargetIP":"",
               "SignalStrengthPercent":99,"SignalQualityPercent":99,"SymbolQualityPercent":99},
              {"Resource":"tuner1","VctNumber":"9.1","TargetIP":"192.168.4.2",
               "SignalStrengthPercent":100,"SignalQualityPercent":100,"SymbolQualityPercent":100},
              {"Resource":"tuner2","VctNumber":"7.1","TargetIP":"172.18.0.2",
               "SignalStrengthPercent":96,"SignalQualityPercent":89,"SymbolQualityPercent":100}
            ]"#,
        )
        .expect("status rows");
        assert_eq!(
            tuner_signal_for_channel(rows, "7.1"),
            Some(LiveTvSignalStatus {
                strength_percent: Some(96),
                quality_percent: Some(89),
                symbol_quality_percent: Some(100),
            })
        );

        let invalid = serde_json::from_str::<Vec<serde_json::Value>>(
            r#"[{"Resource":"tuner0","VctNumber":"7.1","TargetIP":"192.168.4.2",
                  "SignalStrengthPercent":101,"SignalQualityPercent":999}]"#,
        )
        .expect("invalid percentages");
        assert_eq!(tuner_signal_for_channel(invalid, "7.1"), None);
    }

    /// A 10-bit broadcast has to reach the encoder as 8-bit.
    ///
    /// Found on a real HDHomeRun FLEX 4K: every ATSC 3.0 channel is HEVC Main
    /// 10, the live chain was `bwdif,scale` with no pixel-format conversion,
    /// and the software encoder pins `-profile:v high`. x264 answers "high
    /// profile doesn't support a bit depth of 10" and FFmpeg exits before it
    /// publishes anything, so those channels were listed playable and never
    /// started.
    ///
    /// The conversion must happen before a hardware upload: VAAPI's suffix
    /// already does that, while QSV needs the same nv12 conversion inserted
    /// ahead of its qsv-surface upload.
    /// The probe budget is a measurement, so it does not get to drift back.
    ///
    /// A byte budget spent on a stream arriving at the broadcaster's rate is a
    /// time budget, and the old 8 MiB was about 23 s of wall time on a
    /// 2.8 Mbps mux -- longer than the entire startup budget. The first sweep
    /// established 512 KiB as the smallest exercised byte bound; the later
    /// 1080p sweep paired it with a one-second stream-time bound and published
    /// both ATSC 1.0 and AC-4 ATSC 3.0 successfully.
    #[test]
    fn the_live_probe_budget_is_the_measured_one() {
        let args = live_probe_args();
        assert_eq!(
            args,
            ["-probesize", "524288", "-analyzeduration", "1000000"],
            "the live probe budget was measured, not guessed: {args:?}"
        );
        let probesize: u64 = args[1].parse().expect("probesize is a byte count");
        assert!(
            probesize <= 2 * 1024 * 1024,
            "every byte above the measured 2 MiB is startup latency on a live \
             tuner, not spare detection budget"
        );
        assert!(
            probesize >= 512 * 1024,
            "512 KiB was the smallest value measured; below it nothing was \
             won and detection has less to work with"
        );
    }

    /// Live TV has no pre-body codec facts, so its plan freezes the explicit
    /// software-auto input contract instead of reusing a movie cache plan.
    #[test]
    fn live_tv_software_hls_argument_baseline_is_stable() {
        let system = SystemInfo {
            ffmpeg: "/fixture/ffmpeg".to_owned(),
            ..SystemInfo::default()
        };
        let plan = LiveTvTranscodePlan::new(&system, Encoder::Software, Some(2), 720)
            .expect("prepare live-TV plan");
        let command = live_ffmpeg_command(&system, &plan, Path::new("/fixture/live"))
            .expect("build live-TV FFmpeg command");
        let actual = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let expected = [
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostdin",
            "-y",
            "-hwaccel",
            "none",
            "-fflags",
            "+genpts+discardcorrupt",
            "-probesize",
            "524288",
            "-analyzeduration",
            "1000000",
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-sn",
            "-dn",
            "-vf",
            "bwdif=mode=send_frame:parity=auto:deint=interlaced,scale=-2:720,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-b:v",
            "4000k",
            "-maxrate",
            "6000k",
            "-bufsize",
            "8000k",
            "-profile:v",
            "high",
            "-threads",
            "2",
            "-force_key_frames",
            "expr:gte(t,n_forced*1)",
            "-g",
            "120",
            "-keyint_min",
            "1",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ac",
            "2",
            "-f",
            "hls",
            "-hls_init_time",
            "1",
            "-hls_time",
            "4",
            "-hls_list_size",
            "6",
            "-hls_delete_threshold",
            "1",
            "-hls_flags",
            "delete_segments+temp_file+independent_segments+omit_endlist",
            "-hls_segment_filename",
            "/fixture/live/segment-%06d.ts",
            "/fixture/live/index.m3u8",
        ];
        assert_eq!(actual, expected);
        assert!(
            actual
                .windows(2)
                .any(|arguments| arguments == ["-hwaccel", "none"]),
            "the live-TV plan must state its software decode contract"
        );
    }

    /// Startup should not make the viewer wait for the normal four-second
    /// segment cadence. Keep the short initial cadence, steady cadence, and
    /// live window explicit here so tuning cannot silently regress while the
    /// runtime and graph-probe commands continue to share one argument list.
    #[test]
    fn live_hls_publishes_short_startup_segments_before_steady_cadence() {
        let value_after = |flag: &str| {
            LIVE_HLS_OUTPUT_ARGS
                .windows(2)
                .find_map(|pair| (pair[0] == flag).then_some(pair[1]))
                .unwrap_or_else(|| panic!("missing {flag} from live HLS arguments"))
        };

        assert_eq!(value_after("-hls_init_time"), "1");
        assert_eq!(value_after("-hls_time"), "4");
        assert_eq!(value_after("-hls_list_size"), "6");
        assert_eq!(value_after("-force_key_frames"), "expr:gte(t,n_forced*1)");
    }

    #[test]
    fn the_live_chain_delivers_eight_bit_to_an_eight_bit_profile() {
        for encoder in [Encoder::Software, Encoder::Nvenc] {
            let filter = live_video_filter(encoder, 720);
            assert!(
                filter.ends_with(",format=yuv420p"),
                "{encoder:?} pins an 8-bit profile and gets whatever the \
                 broadcaster sent: {filter}"
            );
        }
        for encoder in [Encoder::Vaapi, Encoder::Qsv] {
            let filter = live_video_filter(encoder, 720);
            let suffix = encoder
                .filter_suffix()
                .expect("a hardware upload path pins its own format");
            if encoder == Encoder::Qsv {
                assert!(
                    filter.ends_with(&format!("format=nv12,{suffix}")),
                    "QSV must convert 10-bit ATSC 3.0 to the 8-bit H.264 contract: {filter}"
                );
            } else {
                assert!(
                    filter.ends_with(suffix),
                    "{encoder:?} already uploads in its own format; a trailing \
                     system-memory conversion would break the upload: {filter}"
                );
            }
            assert!(
                !filter.ends_with(",format=yuv420p"),
                "{encoder:?} must not get the software conversion: {filter}"
            );
        }
    }

    /// A slow channel and an absent one get different budgets.
    ///
    /// This is the exact defect a real HDHomeRun found. An ATSC 3.0 channel
    /// published its first segment at 18.1 s while the ATSC 1.0 channel on the
    /// same device and host took 7.04 s, so a flat 15 s budget refused a
    /// channel the lineup had already called playable. Raising the constant for
    /// everyone would have been the wrong fix: two ATSC 3.0 channels on that
    /// antenna deliver no bytes at all, and those should still fail promptly.
    ///
    /// Every row below is a decision the unfixed implementation got wrong or
    /// right for the wrong reason: it compared `now` to one deadline and never
    /// looked at the tuner at all.
    #[test]
    fn a_tuner_that_is_feeding_earns_longer_than_one_that_is_silent() {
        let started = tokio::time::Instant::now();
        let at = |seconds: u64| started + Duration::from_secs(seconds);

        // Silent: prompt, and the message says the channel may have no signal
        // rather than blaming the segment that never came.
        assert!(startup_overdue(started, at(14), 0).is_none());
        let silent = startup_overdue(started, at(15), 0).expect("silent tuner is overdue at 15s");
        assert!(
            silent.contains("no data"),
            "a silent tuner must say so, not blame a missing segment: {silent}"
        );

        // Feeding: the 18.1 s real measurement is INSIDE the budget. This is
        // the assertion the unfixed implementation fails.
        assert!(
            startup_overdue(started, at(16), 1).is_none(),
            "one byte from the tuner must buy more than the silent budget"
        );
        assert!(
            startup_overdue(started, at(19), 4_000_000).is_none(),
            "18.1s is what a real ATSC 3.0 channel needed; refusing it is the defect"
        );

        // But feeding is not forever, and the eventual refusal is specific
        // enough to tell a stalled producer from an absent channel.
        let stalled = startup_overdue(started, at(30), 4_000_000)
            .expect("a feeding tuner that never publishes is overdue eventually");
        assert!(
            stalled.contains("4000000") && stalled.contains("no complete live segment"),
            "a stalled producer must name what arrived: {stalled}"
        );

        // The two budgets are ordered, whatever the constants become.
        assert!(
            STARTUP_FEEDING_TIMEOUT > STARTUP_TIMEOUT,
            "a feeding tuner must never get less time than a silent one"
        );
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

    #[tokio::test]
    async fn ten_live_windows_publish_manifest_and_bounded_deletion_lag_atomically() {
        let temp = crate::test_tempdir().expect("scratch root");
        let directory = temp.path();
        for first in 1_u64..=10 {
            let playlist = live_playlist(first, MAX_LISTED_SEGMENTS);
            tokio::fs::write(directory.join("index.m3u8"), &playlist)
                .await
                .expect("playlist");
            for sequence in first - 1..first + MAX_LISTED_SEGMENTS as u64 {
                tokio::fs::write(
                    directory.join(format!("segment-{sequence:06}.ts")),
                    format!("window-{first}-segment-{sequence}"),
                )
                .await
                .expect("segment");
            }
            if first > 1 {
                let _ =
                    tokio::fs::remove_file(directory.join(format!("segment-{:06}.ts", first - 2)))
                        .await;
            }
            let publication = inspect_scratch(directory)
                .await
                .expect("valid scratch")
                .expect("published window");
            assert_eq!(publication.playlist, playlist);
            assert_eq!(publication.media_sequence, first);
            assert_eq!(publication.segments.len(), MAX_LISTED_SEGMENTS + 1);
            assert!(publication.segments.contains_key(&(first - 1)));
        }
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

    // ---- programme guide --------------------------------------------------

    fn guide_lineup() -> Vec<LiveTvChannel> {
        vec![
            LiveTvChannel {
                id: "7.1".into(),
                guide_number: "7.1".into(),
                guide_name: "WABC".into(),
                favorite: false,
                drm: false,
                support: LiveTvChannelSupport::Ready,
                hd: None,
                video_codec: None,
                audio_codec: None,
            },
            LiveTvChannel {
                id: "4.1".into(),
                guide_number: "4.1".into(),
                guide_name: "WNBC".into(),
                favorite: false,
                drm: false,
                support: LiveTvChannelSupport::Ready,
                hd: None,
                video_codec: None,
                audio_codec: None,
            },
        ]
    }

    fn guide_by_number(lineup: &[LiveTvChannel]) -> BTreeMap<String, &LiveTvChannel> {
        lineup
            .iter()
            .map(|channel| (channel.guide_number.clone(), channel))
            .collect()
    }

    #[test]
    fn hdhomerun_guide_maps_the_documented_shape_and_drops_channels_the_lineup_does_not_carry() {
        let lineup = guide_lineup();
        let body = br#"[
          {"GuideNumber":"7.1","GuideName":"WABC","Affiliate":"ABC",
           "ImageURL":"https://example.invalid/abc.png",
           "Guide":[{"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat",
                     "EpisodeTitle":"Pier 40","EpisodeNumber":"S03E14","Synopsis":"A rebuild.",
                     "OriginalAirdate":1789000800,"ImageURL":"https://example.invalid/e.png",
                     "Filter":["News"]}]},
          {"GuideNumber":"99.9","GuideName":"Nowhere","Guide":[
                    {"StartTime":1789000800,"EndTime":1789002600,"Title":"Not in the lineup"}]}
        ]"#;
        let channels =
            guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
        assert_eq!(
            channels.len(),
            1,
            "a guide row without a lineup row is not a channel"
        );
        let channel = &channels[0];
        assert_eq!(channel.id, "7.1");
        assert_eq!(channel.affiliate.as_deref(), Some("ABC"));
        let programme = &channel.programmes[0];
        assert_eq!(programme.title, "City Beat");
        assert_eq!(programme.episode.as_deref(), Some("S3E14"));
        assert_eq!(programme.original_air_date.as_deref(), Some("2026-09-10"));
        assert_eq!(programme.filters, vec!["News".to_owned()]);
    }

    #[test]
    fn guide_rows_are_bounded_because_the_guide_host_is_untrusted_input() {
        let lineup = guide_lineup();
        let long = "x".repeat(4096);
        let body = format!(
            r#"[{{"GuideNumber":"7.1","Affiliate":"{long}","ImageURL":"http://example.invalid/i.png",
               "Guide":[
                 {{"StartTime":1789000800,"EndTime":1789002600,"Title":"{long}","Synopsis":"{long}",
                   "EpisodeNumber":"garbage","ImageURL":"http://plain.invalid/x.png",
                   "Filter":["a","b","c","d","e","f","g","h","i","j"]}},
                 {{"StartTime":1789002600,"EndTime":1789002600,"Title":"zero length"}},
                 {{"StartTime":1789002600,"EndTime":1789004400}}
               ]}}]"#
        );
        let channels = guide::parse_hdhomerun_guide(body.as_bytes(), &guide_by_number(&lineup))
            .expect("guide document");
        let channel = &channels[0];
        assert_eq!(channel.affiliate.as_deref().map(str::len), Some(32));
        // Artwork is passed through to clients, so a non-https URL must not be.
        assert_eq!(channel.image_url, None);
        assert_eq!(
            channel.programmes.len(),
            1,
            "a zero-length and a title-less row are dropped"
        );
        let programme = &channel.programmes[0];
        assert_eq!(programme.title.len(), 256);
        assert_eq!(programme.synopsis.as_deref().map(str::len), Some(1024));
        assert_eq!(
            programme.episode, None,
            "an unparseable episode number is dropped, not guessed"
        );
        assert_eq!(programme.image_url, None);
        assert_eq!(programme.filters.len(), 8);
    }

    #[test]
    fn overlapping_guide_rows_are_normalised_so_what_is_on_now_is_never_ambiguous() {
        let rows = vec![
            LiveTvProgramme {
                start: 200,
                end: 400,
                title: "Second".into(),
                episode_title: None,
                episode: None,
                synopsis: None,
                image_url: None,
                original_air_date: None,
                filters: Vec::new(),
            },
            LiveTvProgramme {
                start: 100,
                end: 300,
                title: "First".into(),
                episode_title: None,
                episode: None,
                synopsis: None,
                image_url: None,
                original_air_date: None,
                filters: Vec::new(),
            },
            LiveTvProgramme {
                start: 120,
                end: 260,
                title: "Swallowed".into(),
                episode_title: None,
                episode: None,
                synopsis: None,
                image_url: None,
                original_air_date: None,
                filters: Vec::new(),
            },
        ];
        let normalised = guide::normalise_programmes(rows);
        assert_eq!(
            normalised
                .iter()
                .map(|p| (p.start, p.end, p.title.as_str()))
                .collect::<Vec<_>>(),
            vec![(100, 300, "First"), (300, 400, "Second")],
            "a row entirely inside another disappears rather than overlapping it"
        );
    }

    #[test]
    fn the_guide_host_allowlist_pins_scheme_port_userinfo_and_host() {
        let approved =
            |raw: &str| guide::approved_guide_url(&reqwest::Url::parse(raw).expect("url"));
        assert!(approved("https://api.hdhomerun.com/api/guide"));
        assert!(approved("https://my.hdhomerun.com/api/guide.php"));
        assert!(!approved("http://api.hdhomerun.com/api/guide"));
        assert!(!approved("https://api.hdhomerun.com:8443/api/guide"));
        assert!(!approved("https://user:pass@api.hdhomerun.com/api/guide"));
        assert!(!approved(
            "https://api.hdhomerun.com.evil.invalid/api/guide"
        ));
        assert!(!approved("https://192.168.1.20/api/guide"));
    }

    #[test]
    fn a_guide_request_url_carries_the_credential_and_nothing_else_leaves_the_allowlist() {
        let url = guide::guide_request_url("secret-auth", Some("7.1"), Some(1789000800))
            .expect("guide url");
        assert_eq!(url.host_str(), Some("api.hdhomerun.com"));
        let pairs = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect::<Vec<_>>();
        assert!(pairs.contains(&("DeviceAuth".to_owned(), "secret-auth".to_owned())));
        assert!(pairs.contains(&("Channel".to_owned(), "7.1".to_owned())));
        assert!(pairs.contains(&("Start".to_owned(), "1789000800".to_owned())));
    }

    #[test]
    fn xmltv_matches_by_display_name_then_lcn_then_callsign_and_drops_the_rest() {
        let lineup = guide_lineup();
        let document = br#"<?xml version="1.0"?>
        <tv>
          <channel id="a"><display-name>7.1</display-name></channel>
          <channel id="b"><display-name>NBC New York</display-name><lcn>4.1</lcn></channel>
          <channel id="c"><display-name>Not in the lineup</display-name></channel>
          <programme start="20260907200000 -0400" stop="20260907203000 -0400" channel="a">
            <title>City Beat</title><sub-title>Pier 40</sub-title><desc>A rebuild.</desc>
            <episode-num system="xmltv_ns">2.13.</episode-num>
            <category>News</category><date>20260907</date>
          </programme>
          <programme start="20260907200000 -0400" stop="20260907210000 -0400" channel="b">
            <title>The Long Game</title>
          </programme>
          <programme start="20260907200000 -0400" stop="20260907210000 -0400" channel="c">
            <title>Nobody watches this</title>
          </programme>
        </tv>"#;
        let channels = guide::parse_xmltv(document, &lineup).expect("xmltv");
        assert_eq!(channels.len(), 2, "an unmatched XMLTV channel is dropped");
        let abc = channels.iter().find(|c| c.id == "7.1").expect("7.1");
        assert_eq!(abc.programmes[0].title, "City Beat");
        assert_eq!(abc.programmes[0].episode.as_deref(), Some("S3E14"));
        assert_eq!(
            abc.programmes[0].original_air_date.as_deref(),
            Some("2026-09-07")
        );
        assert_eq!(abc.programmes[0].filters, vec!["News".to_owned()]);
        // 20:00 EDT on 2026-09-07 is 2026-09-08T00:00:00Z.
        assert_eq!(abc.programmes[0].start, 1788825600);
        let nbc = channels.iter().find(|c| c.id == "4.1").expect("4.1");
        assert_eq!(nbc.programmes[0].title, "The Long Game");
    }

    #[test]
    fn an_xmltv_time_without_an_offset_is_an_error_for_that_row_and_never_a_guess() {
        assert_eq!(
            guide::parse_xmltv_time("20260907200000 -0400"),
            Some(1788825600)
        );
        assert_eq!(
            guide::parse_xmltv_time("20260908000000 +0000"),
            Some(1788825600)
        );
        assert_eq!(guide::parse_xmltv_time("20260907200000"), None);
        assert_eq!(guide::parse_xmltv_time("nonsense +0000"), None);
        assert_eq!(guide::parse_xmltv_time("20261307200000 +0000"), None);
    }

    #[test]
    fn xmltv_accepts_a_gzipped_document_by_sniffing_it_and_refuses_a_bomb() {
        use std::io::Write as _;
        let plain = b"<?xml version=\"1.0\"?><tv></tv>";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(plain).expect("gzip");
        let gzipped = encoder.finish().expect("gzip finish");
        assert_eq!(
            guide::decompress_if_gzip(gzipped).expect("inflate"),
            plain.to_vec()
        );
        assert_eq!(
            guide::decompress_if_gzip(plain.to_vec()).expect("passthrough"),
            plain.to_vec()
        );
        let mut bomb = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        bomb.write_all(&vec![b'a'; guide::GUIDE_MAX_DOCUMENT_BYTES + 4096])
            .expect("gzip bomb");
        let bomb = bomb.finish().expect("gzip bomb finish");
        assert!(bomb.len() < 64 * 1024, "the bomb must be small on the wire");
        assert!(
            guide::decompress_if_gzip(bomb).is_err(),
            "a small compressed body must not become an unbounded allocation"
        );
    }

    #[test]
    fn episode_numbers_normalise_to_one_spelling_across_both_sources() {
        assert_eq!(
            guide::normalise_episode(Some("S03E14".into())).as_deref(),
            Some("S3E14")
        );
        assert_eq!(
            guide::normalise_episode(Some("s3e14".into())).as_deref(),
            Some("S3E14")
        );
        assert_eq!(
            guide::normalise_episode(Some("E07".into())).as_deref(),
            Some("E7")
        );
        assert_eq!(guide::normalise_episode(Some("14".into())), None);
        assert_eq!(guide::normalise_episode(Some("SxxEyy".into())), None);
        assert_eq!(guide::xmltv_ns_episode("2.13.").as_deref(), Some("S3E14"));
        assert_eq!(
            guide::xmltv_ns_episode("2.13/26.").as_deref(),
            Some("S3E14")
        );
        assert_eq!(guide::xmltv_ns_episode(".4.").as_deref(), Some("E5"));
        assert_eq!(guide::xmltv_ns_episode("nonsense"), None);
    }

    fn guide_with(channels: usize, programmes_each: usize, title_bytes: usize) -> LiveTvGuide {
        LiveTvGuide {
            source: "hdhomerun".into(),
            freshness: GuideFreshness::Fresh,
            age_seconds: 0,
            fetched_at: Some(1789000000),
            window: GuideWindow {
                start: 0,
                end: i64::from(u32::MAX),
            },
            refresh_error: None,
            matched_channels: channels,
            lineup_channels: channels,
            channels: (0..channels)
                .map(|index| LiveTvGuideChannel {
                    id: format!("{index}.1"),
                    guide_number: format!("{index}.1"),
                    affiliate: None,
                    image_url: None,
                    programmes: (0..programmes_each)
                        .map(|slot| LiveTvProgramme {
                            start: (slot as i64) * 1800,
                            end: (slot as i64 + 1) * 1800,
                            title: "t".repeat(title_bytes),
                            episode_title: None,
                            episode: None,
                            synopsis: None,
                            image_url: None,
                            original_air_date: None,
                            filters: Vec::new(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn an_oversized_guide_loses_its_furthest_programmes_and_never_a_channel() {
        let guide = guide_with(512, 40, 200);
        let raw = serde_json::to_vec(&guide).expect("guide json");
        assert!(
            raw.len() > guide::MAX_GUIDE_RESPONSE_BYTES,
            "the fixture must actually exceed the cap ({} bytes)",
            raw.len()
        );
        let clipped = guide.clipped(&GuideWindow {
            start: 0,
            end: i64::from(u32::MAX),
        });
        assert_eq!(
            clipped.channels.len(),
            512,
            "a channel must never be dropped"
        );
        assert!(
            serde_json::to_vec(&clipped).expect("clipped json").len()
                <= guide::MAX_GUIDE_RESPONSE_BYTES
        );
        assert!(
            clipped.total_programmes() < guide.total_programmes(),
            "something has to have been dropped"
        );
    }

    #[test]
    fn clipping_keeps_only_programmes_that_touch_the_window() {
        let guide = guide_with(1, 6, 8);
        let clipped = guide.clipped(&GuideWindow {
            start: 1800,
            end: 5400,
        });
        let spans = clipped.channels[0]
            .programmes
            .iter()
            .map(|p| (p.start, p.end))
            .collect::<Vec<_>>();
        assert_eq!(spans, vec![(1800, 3600), (3600, 5400)]);
        assert_eq!(clipped.window.start, 1800);
    }

    #[tokio::test]
    async fn the_guide_cache_serves_fresh_then_stale_then_nothing_and_a_generation_change_empties_it(
    ) {
        tokio::time::pause();
        let cache = GuideCache::default();
        let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
        config.generation = 4;
        config.guide_source = GuideSource::HdHomeRun;
        let window = GuideWindow {
            start: 0,
            end: i64::from(u32::MAX),
        };

        assert_eq!(
            cache.read(&config, window.clone()).await.freshness,
            GuideFreshness::Unavailable,
            "an empty cache is unavailable, not an error"
        );

        cache.store(4, &Ok(guide_with(2, 2, 8))).await;
        assert_eq!(
            cache.read(&config, window.clone()).await.freshness,
            GuideFreshness::Fresh
        );

        tokio::time::advance(guide::GUIDE_REFRESH_INTERVAL + Duration::from_secs(1)).await;
        assert_eq!(
            cache.read(&config, window.clone()).await.freshness,
            GuideFreshness::Stale,
            "past one refresh interval the answer is stale, and still served"
        );

        // A refresh that fails keeps the content and reports the reason.
        cache
            .store(
                4,
                &Err(LiveTvError::DeviceUnavailable("the tuner is asleep".into())),
            )
            .await;
        let after_failure = cache.read(&config, window.clone()).await;
        assert_eq!(after_failure.freshness, GuideFreshness::Stale);
        assert_eq!(
            after_failure.channels.len(),
            2,
            "a failed refresh keeps the cache"
        );
        assert!(after_failure.refresh_error.is_some());

        tokio::time::advance(guide::GUIDE_STALE_TTL).await;
        assert_eq!(
            cache.read(&config, window.clone()).await.freshness,
            GuideFreshness::Unavailable,
            "past the stale window a confidently wrong grid is worse than none"
        );

        cache.store(4, &Ok(guide_with(2, 2, 8))).await;
        config.generation = 5;
        assert_eq!(
            cache.read(&config, window).await.freshness,
            GuideFreshness::Unavailable,
            "a settings generation change invalidates the cache it was matched against"
        );
    }

    #[test]
    fn guide_settings_are_validated_structurally_and_never_on_whether_the_source_answers() {
        let base = |source: GuideSource, url: &str, hours: u8| {
            let mut config = LiveTvConfig::from_snapshot(&BTreeMap::new(), "node-a");
            config.guide_source = source;
            config.xmltv_url = url.to_owned();
            config.guide_hours = hours;
            config
        };
        assert!(base(GuideSource::Off, "", 24).validate_guide().is_ok());
        assert!(base(GuideSource::HdHomeRun, "", 24)
            .validate_guide()
            .is_ok());
        assert!(base(GuideSource::Xmltv, "", 24).validate_guide().is_err());
        assert!(base(GuideSource::Xmltv, "https://x.invalid/g.xml", 24)
            .validate_guide()
            .is_ok());
        // An unreachable host is not a validation failure: it is a thing the
        // Developer card says and the operator decides about.
        assert!(base(GuideSource::Xmltv, "http://192.0.2.1/g.xml", 4)
            .validate_guide()
            .is_ok());
        assert!(base(GuideSource::Xmltv, "file:///etc/passwd", 24)
            .validate_guide()
            .is_err());
        assert!(
            base(GuideSource::Xmltv, "https://user:pw@x.invalid/g.xml", 24)
                .validate_guide()
                .is_err()
        );
        assert!(base(GuideSource::HdHomeRun, "", 3)
            .validate_guide()
            .is_err());
        assert!(base(GuideSource::HdHomeRun, "", 73)
            .validate_guide()
            .is_err());
    }

    #[test]
    fn xmltv_text_survives_entities_and_cdata_because_real_grabbers_emit_both() {
        let lineup = guide_lineup();
        let body = br#"<tv>
          <channel id="c1"><display-name>7.1</display-name></channel>
          <programme start="20260907200000 -0400" stop="20260907203000 -0400" channel="c1">
            <title>Tom &amp; Jerry</title>
            <sub-title><![CDATA[Cat & Mouse: "the chase"]]></sub-title>
            <desc>Fast &lt;and&gt; loud &#8212; very.</desc>
            <episode-num system="onscreen">S00E05</episode-num>
          </programme>
        </tv>"#;
        let channels = guide::parse_xmltv(body, &lineup).expect("xmltv document");
        let row = &channels[0].programmes[0];
        assert_eq!(
            row.title, "Tom & Jerry",
            "an entity is its own event; reading only text events truncated the title at it"
        );
        assert_eq!(
            row.episode_title.as_deref(),
            Some("Cat & Mouse: \"the chase\""),
            "CDATA is character data too, and was being dropped entirely"
        );
        assert_eq!(row.synopsis.as_deref(), Some("Fast <and> loud — very."));
        assert_eq!(
            row.episode.as_deref(),
            Some("S0E5"),
            "S00E05 is how every source spells a special; trimming zeros first dropped it"
        );
    }

    #[test]
    fn text_bounds_are_the_bytes_the_contract_states_not_characters() {
        // The contract's limits are byte limits, and the guide host is
        // untrusted: 256 CJK characters is 768 bytes.
        let wide = "\u{6f22}".repeat(400);
        let bounded = guide::bounded_text(Some(wide), 256).expect("some text survives");
        assert!(bounded.len() <= 256, "bounded by bytes, not characters");
        assert!(
            bounded.chars().all(|c| c == '\u{6f22}'),
            "and never split mid-character"
        );
    }

    #[test]
    fn a_guide_far_over_the_cap_is_trimmed_rather_than_emptied() {
        // Headroom that scaled with the overshoot asked to drop nine times the
        // cap's worth of rows and returned an empty grid — the one outcome the
        // clip exists to prevent.
        let huge = guide_with(200, 120, 700);
        let clipped = huge.clipped(&GuideWindow {
            start: 0,
            end: i64::from(u32::MAX),
        });
        let encoded = serde_json::to_vec(&clipped).expect("json").len();
        assert!(encoded <= guide::MAX_GUIDE_RESPONSE_BYTES);
        assert_eq!(
            clipped.channels.len(),
            200,
            "channels are never dropped, only the furthest-out programmes"
        );
        assert!(
            clipped.total_programmes() > 0,
            "a guide well over the cap still answers with a readable grid"
        );
    }

    #[test]
    fn an_xmltv_url_may_reach_the_lan_but_not_this_machine_or_link_local_space() {
        // The LAN is the point: the plan's own acceptance puts the XMLTV
        // document on another node.
        for allowed in [
            "http://192.168.4.7:8080/guide.xml",
            "http://10.1.2.3/xmltv.gz",
            "https://guide.example.com/xmltv.xml",
            "http://[2001:db8::5]/g.xml",
        ] {
            assert!(
                validate_xmltv_url(allowed).is_ok(),
                "{allowed} is a destination an operator legitimately has"
            );
        }
        // Admin of plurx must not widen into reading this node's own
        // unauthenticated internal listeners, or cloud metadata.
        for refused in [
            "http://127.0.0.1:3000/_internal/v1/live-tv/snapshot",
            "http://127.1/x.xml",
            "http://0.0.0.0/x.xml",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/x.xml",
            "http://[::ffff:127.0.0.1]/x.xml",
            "http://[fe80::1]/x.xml",
            "http://255.255.255.255/x.xml",
            "http://239.1.1.1/x.xml",
        ] {
            assert!(
                validate_xmltv_url(refused).is_err(),
                "{refused} points at this machine or at link-local space"
            );
        }
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_loopback_is_refused_at_fetch_time() {
        // The literal check cannot see through a name, so the fetch resolves
        // first and refuses on the answer.
        let url = validate_xmltv_url("http://localhost:8080/guide.xml").expect("a name parses");
        let error = resolve_permitted_addrs(&url)
            .await
            .expect_err("localhost is this machine");
        assert!(matches!(error, LiveTvError::InvalidConfig(_)));

        // A literal needs no resolution and pins nothing.
        let literal = validate_xmltv_url("http://192.168.4.7/guide.xml").expect("a literal parses");
        assert!(resolve_permitted_addrs(&literal)
            .await
            .expect("a permitted literal")
            .is_empty());
    }

    #[tokio::test]
    async fn a_refresh_with_no_cached_lineup_refuses_rather_than_storing_an_empty_guide() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        seed_test_config(manager.as_ref()).await;
        let mut config = manager.config().await.expect("config");
        config.guide_source = GuideSource::Xmltv;
        config.xmltv_url = "http://192.168.4.7/guide.xml".to_owned();
        config.owner_node_id = manager.node_id.clone();

        // A good guide is cached and the lineup cache is cold — the state a
        // freshly restarted owner is in before anyone opens Live TV.
        manager
            .guide_cache
            .store(config.generation, &Ok(guide_with(2, 2, 8)))
            .await;

        let error = manager
            .refresh_guide(&config, false)
            .await
            .expect_err("a cold lineup is not something to fetch against");
        assert!(
            format!("{error}").contains("lineup"),
            "the operator is told what is missing, not that the source failed"
        );

        // And crucially the good guide survived: fetching anyway matched
        // nothing and stored the empty result over it.
        let window = GuideWindow {
            start: 0,
            end: i64::from(u32::MAX),
        };
        let served = manager.guide_cache.read(&config, window).await;
        assert_eq!(served.channels.len(), 2);
        assert_eq!(served.matched_channels, 2);
    }

    #[tokio::test]
    async fn a_guide_never_carries_deviceauth_into_anything_that_leaves_the_owner() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        seed_test_config(manager.as_ref()).await;
        let mut config = manager.config().await.expect("config");
        config.guide_source = GuideSource::HdHomeRun;

        let secret = "SECRET-DEVICE-AUTH-VALUE";

        // The credential is genuinely in flight on the real request path.
        // Asserting that first is what makes every absence below mean
        // something: a test that only searched a guide the secret was never
        // put into would pass with every credential precaution deleted.
        let request = guide::guide_request_url(secret, Some("7.1"), Some(1_789_000_800))
            .expect("guide request url");
        assert!(
            request.as_str().contains(secret),
            "the credential really is a query parameter of the outbound fetch"
        );

        // And the guide under test is what that credentialed fetch produced,
        // parsed by the shipping parser, rather than a hand-built value.
        let lineup = guide_lineup();
        let body = br#"[
          {"GuideNumber":"7.1","GuideName":"WABC","Affiliate":"ABC",
           "Guide":[{"StartTime":1789000800,"EndTime":1789002600,"Title":"City Beat",
                     "Synopsis":"A rebuild."}]}
        ]"#;
        let channels =
            guide::parse_hdhomerun_guide(body, &guide_by_number(&lineup)).expect("guide document");
        let mut guide = guide_with(0, 0, 0);
        guide.matched_channels = channels.len();
        guide.lineup_channels = lineup.len();
        guide.channels = channels;
        manager
            .guide_cache
            .store(config.generation, &Ok(guide))
            .await;

        let served = manager
            .local_guide(
                &config,
                GuideWindow {
                    start: 0,
                    end: i64::from(u32::MAX),
                },
            )
            .await;
        let encoded = serde_json::to_string(&served).expect("guide json");
        assert!(!encoded.contains(secret));
        assert!(!encoded.contains("DeviceAuth"));

        // And the credential is not reachable from the settings tuple either:
        // it is read from the device at refresh time and never written.
        let settings = manager.store.settings_snapshot().await.expect("settings");
        assert!(settings
            .values()
            .all(|value| !value.contains(secret) && !value.contains("DeviceAuth")));

        // The relay is the one path that copies a guide off this node, so it
        // must be clean for the same reason — asserted on the value the relay
        // actually holds, not on a copy of it.
        manager
            .remember_relayed_guide(config.generation, served.clone())
            .await;
        let relayed = manager
            .relayed_guide(config.generation)
            .await
            .expect("relayed guide");
        let relayed_json = serde_json::to_string(&relayed).expect("relay json");
        assert!(!relayed_json.contains(secret));
        assert!(!relayed_json.contains("DeviceAuth"));

        // Failure copy is the other way a URL escapes. Every message the fetch
        // path can produce is a fixed sentence, so a failed refresh recorded
        // against this generation says nothing about the request it made.
        manager
            .guide_cache
            .store(
                config.generation,
                &Err(LiveTvError::DeviceUnavailable(
                    "HDHomeRun device request failed".to_owned(),
                )),
            )
            .await;
        let after_failure = manager
            .local_guide(
                &config,
                GuideWindow {
                    start: 0,
                    end: i64::from(u32::MAX),
                },
            )
            .await;
        let failure_json = serde_json::to_string(&after_failure).expect("failure json");
        assert!(!failure_json.contains(secret));
        assert!(!failure_json.contains("DeviceAuth"));
        assert!(!failure_json.contains("api.hdhomerun.com"));
    }

    #[tokio::test]
    async fn a_refresh_publishes_the_programme_title_the_activity_page_shows() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        let now = unix_seconds();
        let mut guide = guide_with(1, 1, 8);
        guide.channels[0].guide_number = "7.1".into();
        guide.channels[0].programmes[0].start = now - 60;
        guide.channels[0].programmes[0].end = now + 600;
        guide.channels[0].programmes[0].title = "City Beat".into();
        manager.publish_guide_titles(&guide);
        let titles = Arc::clone(
            &manager
                .guide_titles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let row = titles.get("7.1").expect("channel titles");
        assert!(row
            .iter()
            .any(|(start, end, title)| *start <= now && now < *end && title == "City Beat"));
        manager.clear_guide_titles();
        assert!(manager
            .guide_titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
    }

    #[tokio::test]
    async fn a_refresh_is_refused_off_the_owner_and_with_no_source_configured() {
        let root = crate::test_tempdir().expect("scratch root");
        let manager = test_manager(root.path());
        seed_test_config(manager.as_ref()).await;
        let mut config = manager.config().await.expect("config");

        assert!(matches!(
            manager.refresh_guide(&config, false).await,
            Err(LiveTvError::InvalidConfig(_)),
        ));

        config.guide_source = GuideSource::HdHomeRun;
        config.owner_node_id = "node-b".into();
        assert!(matches!(
            manager.refresh_guide(&config, false).await,
            Err(LiveTvError::OwnerUnavailable(_))
        ));
    }

    #[test]
    fn the_air_date_conversion_is_a_calendar_day_and_rejects_impossible_instants() {
        assert_eq!(
            guide::unix_to_air_date(1789000800).as_deref(),
            Some("2026-09-10")
        );
        assert_eq!(guide::unix_to_air_date(0).as_deref(), Some("1970-01-01"));
        assert_eq!(guide::unix_to_air_date(-1), None);
    }
}
