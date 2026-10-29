//! HDHomeRun configuration, LAN trust boundary, and owner-side lineup cache.
//!
//! This module is always compiled. `live_tv.enabled` is an operator-controlled
//! runtime decision, never a Cargo feature or an environment-only escape hatch.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use plurx_core::store::{keys, Store};
use plurx_core::transcode::{
    EffectiveRateControl, Encoder, MacosProcessingAvailability, MacosProcessingContext,
    MacosProcessingGraph, MacosProcessingIdentity,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::live_tv_delivery::{
    LiveDeinterlaceOutput, LiveDeliveryPlan, LiveExecutionSupport, LivePackaging,
    LivePlaybackRequest, LiveQualityPolicy, LiveSourceFacts, LiveTrackAction,
};
use crate::settings_changes::SettingsChangeSource;
use crate::state::SystemInfo;

pub(crate) mod dvr;
mod resource;
pub(crate) use resource::now_ms as resource_now_ms;
pub(crate) mod guide;

pub(crate) mod schedule;
#[cfg(all(test, target_os = "macos"))]
mod videotoolbox_tests;
pub(crate) mod webhook;

pub(crate) use guide::{
    GuideFreshness, GuideWindow, LiveTvGuide, LiveTvGuideChannel, LiveTvProgramme,
};

pub(crate) const SNAPSHOT_PATH: &str = "/_internal/v1/live-tv/snapshot";
pub(crate) const START_PATH: &str = "/_internal/v1/live-tv/start";
pub(crate) const START_V2_PATH: &str = "/_internal/v2/live-tv/start";
pub(crate) const ACTIVATE_PATH: &str = "/_internal/v1/live-tv/activate";
pub(crate) const RESOURCE_PATH: &str = "/_internal/v1/live-tv/resource";
pub(crate) const STOP_PATH: &str = "/_internal/v1/live-tv/stop";
pub(crate) const RETIRE_PATH: &str = "/_internal/v1/live-tv/retire";
pub(crate) const RESUME_PATH: &str = "/_internal/v1/live-tv/resume";
pub(crate) const START_STATE_PATH: &str = "/_internal/v1/live-tv/start-state";
pub(crate) const GUIDE_PATH: &str = "/_internal/v1/live-tv/guide";
pub(crate) const MAX_INTERNAL_BODY_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const _: () = assert!(MAX_SNAPSHOT_BYTES == guide::MAX_GUIDE_RESPONSE_BYTES);
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_CHANNELS: usize = 512;
const MAX_GUIDE_NUMBER_BYTES: usize = 32;
const MAX_GUIDE_NAME_BYTES: usize = 256;
const MAX_CHANNEL_FORMAT_BYTES: usize = 32;
const MAX_SOURCE_DESCRIPTOR_BYTES: usize = 64 * 1024;
const SOURCE_FORMAT_TTL: Duration = Duration::from_secs(20 * 60);
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
/// actually answers; a fortnight is what a subscription tier answers and what
/// a series recording rule needs to see in order to schedule anything worth
/// scheduling. The cache is durable now (`guide.json`), so the old
/// "as far as a *memory* cache is worth filling" ceiling of 72 no longer
/// describes the cost.
pub(crate) const DEFAULT_GUIDE_HOURS: u16 = 24;
pub(crate) const MIN_GUIDE_HOURS: u16 = 4;
pub(crate) const MAX_GUIDE_HOURS: u16 = 336;
/// One tick re-validates this much of the already-cached horizon, cycling
/// through it, so a provider's schedule change reaches the cache within a
/// fortnight of ticks (~5 hours) instead of never: a tail-only extension
/// never revisits what it already has.
const GUIDE_REVALIDATION_HOURS: i64 = 24;
const MAX_XMLTV_URL_BYTES: usize = 1024;
/// An ingress remembers the owner's answer for this long, so a click-storm on
/// a busy page is not a relay-storm on the owner. It is deliberately shorter
/// than the refresh interval: this bounds fan-out, it does not add staleness
/// the owner has not already accounted for.
const RELAY_GUIDE_MEMORY: Duration = Duration::from_secs(60);
/// An owner with nothing to serve is an owner about to have something: a
/// minute of repeating "unavailable" is how a client that opened Live TV
/// during a deploy ends up staring at an empty grid.
const RELAY_UNAVAILABLE_GUIDE_MEMORY: Duration = Duration::from_secs(10);
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
const SOURCE_PREFIX_BYTES: usize = 8 * 1024 * 1024;
const SOURCE_PREFIX_TIME: Duration = Duration::from_secs(3);
const SOURCE_PROBE_TIME: Duration = Duration::from_secs(2);
/// The budget of a Live TV probe nobody is waiting on: the readiness graph
/// probe, and the boot caption self-test's source probe, both of which run
/// beside the boot's background encoder qualification.
const READINESS_PROBE_TIME: Duration = Duration::from_secs(20);
const MAX_SOURCE_PROBE_JSON_BYTES: usize = 256 * 1024;
/// Uniform one-second segments, for the whole session. The short first
/// segment `-hls_init_time 1` used to cut is what keeps a start no slower
/// than the tuner; what it also did was let the cadence grow to 4 s once the
/// list filled, which left every client -- attached 1 s behind the edge --
/// waiting a whole segment at each jump. Keeping the cadence at 1 s removes
/// the jump. Encode routes already force a keyframe every second; copy routes
/// cut at the broadcast's own keyframes. The window is 24 entries: 24 s on an
/// encode route and 24 source GOPs on a copy route.
const LIVE_HLS_OUTPUT_ARGS: [&str; 8] = [
    "-f",
    "hls",
    "-hls_time",
    "1",
    "-hls_list_size",
    "24",
    "-hls_delete_threshold",
    "4",
];
/// The owner answers a start once the playlist lists this many segments and
/// this many target durations of media. Both values are enforced against the
/// playlist, never against the scratch directory's transient files.
const STARTUP_LISTED_SEGMENTS: usize = 2;
const STARTUP_LISTED_TARGET_DURATIONS: f64 = 2.0;
const HLS_DELETE_THRESHOLD: usize = 4;
pub(crate) const MAX_PLAYLIST_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_SEGMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SESSION_BYTES: u64 = MAX_SEGMENT_BYTES;
const MAX_LISTED_SEGMENTS: usize = 24;
/// The HTTP sink receives objects on separate connections: a segment and
/// playlist can overlap, unlike the legacy single-file writer. Bound its
/// unique upload temporaries to one window plus the init and playlist.
const MAX_UPLOAD_TEMPORARIES: usize = MAX_LISTED_SEGMENTS + 2;
/// hlsenc renames a finished segment before it rewrites the playlist, so for
/// an instant the scratch holds `threshold + 1` final files that the playlist
/// does not list. Deletion precedes the rewrite, so it is never `+ 2`.
const MAX_DELETION_LAG_SEGMENTS: usize = HLS_DELETE_THRESHOLD + 1;
const PUMP_CHANNEL_CAPACITY: usize = 2;
const LOCAL_RESOURCE_CHUNK_BYTES: usize = 64 * 1024;
const LOCAL_RESOURCE_CONCURRENCY: usize = 4;
const LOCAL_RESOURCE_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_RESOURCE_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const SESSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINAL_TOMBSTONE_TTL: Duration = Duration::from_secs(60);
const MAX_TERMINAL_TOMBSTONES: usize = 256;
/// How many retired request ids one viewer may hold at once, and for how long.
/// The same minute as a tombstone, because that is how long a replay of the
/// retired id could still be in flight.
const MAX_RETIRED_PER_USER: usize = 8;
const RETIRED_TTL: Duration = TERMINAL_TOMBSTONE_TTL;
/// A session of the requesting viewer whose last keepalive is older than this
/// is a stray — the app it belonged to is gone — and it is cancelled to admit
/// that viewer's new start when every slot is full. Three missed 5 s
/// heartbeats. Paul's ruling: a tuner this viewer may still be holding is
/// never a reason to refuse that same viewer.
///
/// A viewer whose phone slept, whose browser tab was closed without a stop, or
/// whose app was force-quit is holding a tuner nobody is watching. Refusing
/// them their own tuner is the worst of the three available answers, and it is
/// also what would make the recording-capacity dialog lie: a viewer told
/// "a recording has your tuner" while in fact holding it themselves would stop
/// a recording for nothing.
const STRAY_EVICTION_IDLE: Duration = Duration::from_secs(15);
const SCRATCH_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How often the orphan owner retries what a failed session cleanup left
/// behind: a child whose exit was not confirmed, or a scratch directory that
/// could not be removed. The same loop as the hourly scratch sweep.
const ORPHAN_RETRY: Duration = Duration::from_secs(5);
/// How many orphans the owner keeps. Past this the oldest *scratch-only*
/// orphan is dropped (its directory is left to the hourly sweep); an orphan
/// still holding an unconfirmed child is never dropped, because an encoder
/// slot held by a live process is a fact, not a leak.
const ORPHAN_MAX: usize = 64;
/// Where the owner records its own incarnation, beside the persisted guide.
const INCARNATION_FILE: &str = "incarnation.json";
const INCARNATION_VERSION: u32 = 1;
/// How often a node with anything live refreshes `last_seen_at`.
const INCARNATION_TOUCH: Duration = Duration::from_secs(30);
/// How recently the previous process must have been seen for an unknown
/// capability to be explained as "the owner restarted": the longest a
/// session could have gone unused before it ended anyway (unactivated for
/// `PROVISIONAL_TIMEOUT`, then idle for `CAPABILITY_IDLE_TIMEOUT`), plus one
/// `INCARNATION_TOUCH` of staleness in `last_seen_at` itself.
const INCARNATION_RESTART_WINDOW: Duration = Duration::from_secs(
    PROVISIONAL_TIMEOUT.as_secs() + CAPABILITY_IDLE_TIMEOUT.as_secs() + INCARNATION_TOUCH.as_secs(),
);

/// How often this node refreshes its one validated observation of the
/// replicated Live TV settings. Every live session validates against that
/// observation on its own one-second fence tick instead of doing the
/// consistent read itself, so the read rate is per node rather than per
/// session.
const FENCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);

/// The age at which an observation stops being *fresh*: one observation
/// interval plus one `STORE_TIMEOUT` (3 s, `store::hiqlite`), which is the
/// longest a single successful consistent read may take. An observation older
/// than this means a read failed or was retried, not merely that the tick has
/// not come round yet.
const FENCE_OBSERVATION_FRESH_MAX_AGE: Duration =
    Duration::from_secs(FENCE_OBSERVATION_INTERVAL.as_secs() + 3);

/// How long sessions may continue on the last validated observation while the
/// consistent read keeps failing.
///
/// Longer than one authority-read retry budget — two `STORE_TIMEOUT` attempts
/// 100 ms apart, or a 5 s quorum-recovery budget, so ~6.1 s at worst — so a
/// leader failover does not end healthy streams. The serving fence ends the
/// session independently on quorum loss that outlasts the session grace
/// (`LiveTvAuthority`), so this bound is never the only one.
///
/// Ten seconds is the repository owner's decision of 2026-09-23, recorded in
/// the architecture review work board. It is a policy bound sized against the
/// read's own budget, not against a measured fleet failover; that measurement
/// is still outstanding.
const FENCE_GRACE_MAX_AGE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTvConfig {
    pub(crate) enabled: bool,
    pub(crate) device_ipv4: Option<Ipv4Addr>,
    pub(crate) owner_node_id: String,
    pub(crate) max_sessions: u8,
    /// Zero means Original / no administrative ceiling.
    pub(crate) max_output_height: u16,
    /// Compatibility profile for old clients and old owners only.
    pub(crate) output_height: u16,
    pub(crate) deinterlace_output: LiveDeinterlaceOutput,
    pub(crate) generation: i64,
    /// The programme guide. Read-only information the tuner contract never
    /// depends on, which is why these three ride the same generation CAS but
    /// not the "disable before editing" rule.
    pub(crate) guide_source: GuideSource,
    pub(crate) xmltv_url: String,
    pub(crate) guide_hours: u16,
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
            owner_node_id: local_node_id.to_owned(),
            max_sessions: setting(keys::LIVE_TV_MAX_SESSIONS)
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
            max_output_height: setting(keys::LIVE_TV_MAX_OUTPUT_HEIGHT)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            output_height: setting(keys::LIVE_TV_OUTPUT_HEIGHT)
                .and_then(|value| value.parse().ok())
                .unwrap_or(720),
            deinterlace_output: setting(keys::LIVE_TV_DEINTERLACE_OUTPUT)
                .and_then(LiveDeinterlaceOutput::parse)
                .unwrap_or_default(),
            generation: setting(keys::LIVE_TV_CONFIG_GENERATION)
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
        if !matches!(self.max_output_height, 0 | 480 | 720 | 1080 | 2160) {
            return Err(LiveTvError::InvalidConfig(
                "live-TV maximum output height must be Original, 480, 720, 1080, or 2160"
                    .to_owned(),
            ));
        }
        if self.owner_node_id.trim().is_empty() || self.owner_node_id.len() > 256 {
            return Err(LiveTvError::InvalidConfig(
                "live-TV owner node is invalid".to_owned(),
            ));
        }
        if let Some(address) = self.device_ipv4 {
            validate_device_ipv4(address)?;
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
}

/// The recording half of the configuration.
///
/// Deliberately its own snapshot rather than fields on [`LiveTvConfig`]: none
/// of these changes a tuner tuple, so none of them rides the Live TV
/// generation CAS, and an operator must be able to change a padding default
/// without disabling Live TV and draining every viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DvrConfig {
    pub(crate) enabled: bool,
    /// Container path of the DVR root. Empty means unset, which is the one
    /// thing that genuinely stops a capture — there is nowhere to write.
    pub(crate) root: String,
    pub(crate) free_floor_gb: i64,
    /// Tuner slots recordings may never take, so a viewer is never locked out
    /// of their own television by their own schedule.
    pub(crate) tuner_reserve: u8,
    pub(crate) pad_start_s: i64,
    pub(crate) pad_end_s: i64,
    pub(crate) reminder_lead_s: i64,
    /// Empty means no webhook.
    pub(crate) webhook_url: String,
}

impl DvrConfig {
    pub(crate) fn from_snapshot(settings: &BTreeMap<String, String>) -> Self {
        let setting = |key: &str| settings.get(key).map(String::as_str);
        let number = |key: &str, default: i64, low: i64, high: i64| {
            setting(key)
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(default)
                .clamp(low, high)
        };
        Self {
            enabled: plurx_core::store::stored_switch(setting(keys::DVR_ENABLED), false),
            root: setting(keys::DVR_ROOT)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            free_floor_gb: number(
                keys::DVR_FREE_FLOOR_GB,
                plurx_core::dvr::DVR_DEFAULT_FREE_FLOOR_GB,
                0,
                1_000_000,
            ),
            tuner_reserve: number(
                keys::DVR_TUNER_RESERVE,
                i64::from(plurx_core::dvr::DVR_DEFAULT_TUNER_RESERVE),
                0,
                4,
            ) as u8,
            pad_start_s: number(
                keys::DVR_PAD_START_S,
                plurx_core::dvr::DVR_DEFAULT_PAD_START_S,
                0,
                plurx_core::dvr::DVR_PAD_MAX_S,
            ),
            pad_end_s: number(
                keys::DVR_PAD_END_S,
                plurx_core::dvr::DVR_DEFAULT_PAD_END_S,
                0,
                plurx_core::dvr::DVR_PAD_MAX_S,
            ),
            reminder_lead_s: number(
                keys::DVR_REMINDER_LEAD_S,
                plurx_core::dvr::DVR_DEFAULT_REMINDER_LEAD_S,
                0,
                plurx_core::dvr::DVR_LEAD_MAX_S,
            ),
            webhook_url: setting(keys::DVR_WEBHOOK_URL)
                .unwrap_or_default()
                .trim()
                .to_owned(),
        }
    }

    /// How many tuner slots recordings may hold at once. Never the whole set:
    /// a reserve that equalled `max_sessions` would mean no recording could
    /// ever start, and one that exceeded it would underflow.
    pub(crate) fn recording_slots(&self, max_sessions: u8) -> u8 {
        max_sessions.saturating_sub(self.tuner_reserve.min(max_sessions))
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_source_format"
    )]
    pub(crate) source_format: Option<LiveTvSourceFormat>,
}

/// Facts observed from FFmpeg's bounded description of the selected tuner
/// input. These describe the source only; Live TV's delivered H.264/AAC
/// rendition remains represented by [`LiveTvOutput`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvSourceFormat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_width: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_height: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_layout: Option<String>,
    pub(crate) observed_at: i64,
}

fn deserialize_source_format<'de, D>(
    deserializer: D,
) -> Result<Option<LiveTvSourceFormat>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let Some(observed_at) = object
        .get("observed_at")
        .and_then(serde_json::Value::as_i64)
    else {
        return Ok(None);
    };
    if observed_at <= 0 {
        return Ok(None);
    }
    let dimension = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .filter(|value| (1..=16_384).contains(value))
            .map(|value| value as u16)
    };
    let audio_channels = object
        .get("audio_channels")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| (1..=32).contains(value))
        .map(|value| value as u8);
    let scan = object
        .get("scan")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_scan);
    let audio_layout = object
        .get("audio_layout")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_audio_layout);
    Ok(Some(LiveTvSourceFormat {
        video_width: dimension("video_width"),
        video_height: dimension("video_height"),
        scan,
        audio_channels,
        audio_layout,
        observed_at,
    }))
}

fn normalize_scan(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "progressive" => Some("progressive".to_owned()),
        "interlaced" => Some("interlaced".to_owned()),
        _ => None,
    }
}

fn normalize_audio_layout(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > MAX_CHANNEL_FORMAT_BYTES
        || !normalized.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '+' | '-' | '_' | ' ')
        })
    {
        return None;
    }
    Some(normalized)
}

fn parse_source_dimensions(line: &str) -> (Option<u16>, Option<u16>, Option<String>) {
    let bytes = line.as_bytes();
    for x in 1..bytes.len().saturating_sub(1) {
        if bytes[x] != b'x' || !bytes[x - 1].is_ascii_digit() || !bytes[x + 1].is_ascii_digit() {
            continue;
        }
        let mut start = x - 1;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let mut end = x + 1;
        while end + 1 < bytes.len() && bytes[end + 1].is_ascii_digit() {
            end += 1;
        }
        let width = line[start..x].parse::<u16>().ok();
        let height = line[x + 1..=end].parse::<u16>().ok();
        if width.is_none_or(|value| value == 0 || value > 16_384)
            || height.is_none_or(|value| value == 0 || value > 16_384)
        {
            continue;
        }
        let suffix = bytes.get(end + 1).copied();
        let lower = line.to_ascii_lowercase();
        let scan = if suffix == Some(b'i')
            || lower.contains("top first")
            || lower.contains("bottom first")
            || lower.contains("interlaced")
        {
            Some("interlaced".to_owned())
        } else if suffix == Some(b'p') || lower.contains("progressive") {
            Some("progressive".to_owned())
        } else {
            None
        };
        return (width, height, scan);
    }
    (None, None, None)
}

fn parse_source_audio(line: &str) -> (Option<u8>, Option<String>) {
    let lower = line.to_ascii_lowercase();
    for token in lower.split(',').map(str::trim) {
        let word = token.split_whitespace().next().unwrap_or_default();
        if word == "mono" {
            return (Some(1), Some("mono".to_owned()));
        }
        if word == "stereo" {
            return (Some(2), Some("stereo".to_owned()));
        }
        let base = word.split('(').next().unwrap_or(word);
        if let Some((front, rear)) = base.split_once('.') {
            if let (Ok(front), Ok(rear)) = (front.parse::<u8>(), rear.parse::<u8>()) {
                if let Some(channels) = front
                    .checked_add(rear)
                    .filter(|value| (1..=32).contains(value))
                {
                    if let Some(layout) = normalize_audio_layout(base) {
                        return (Some(channels), Some(layout));
                    }
                }
            }
        }
        if let Some(value) = token.strip_suffix(" channels") {
            if let Ok(channels) = value.trim().parse::<u8>() {
                if (1..=32).contains(&channels) {
                    return (Some(channels), None);
                }
            }
        }
    }
    (None, None)
}

fn parse_live_source_format(descriptor: &[u8], observed_at: i64) -> Option<LiveTvSourceFormat> {
    let text = String::from_utf8_lossy(descriptor);
    let mut in_input = false;
    let mut video = (None, None, None);
    let mut audio = (None, None);
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Input #0") {
            in_input = true;
            continue;
        }
        if !in_input {
            continue;
        }
        if trimmed.starts_with("Stream mapping:") || trimmed.starts_with("Output #0") {
            break;
        }
        if video.0.is_none() && trimmed.starts_with("Stream #0:") && trimmed.contains(" Video:") {
            video = parse_source_dimensions(trimmed);
        }
        if audio.0.is_none() && trimmed.starts_with("Stream #0:") && trimmed.contains(" Audio:") {
            audio = parse_source_audio(trimmed);
        }
    }
    if video.0.is_none() && video.1.is_none() && audio.0.is_none() && audio.1.is_none() {
        return None;
    }
    Some(LiveTvSourceFormat {
        video_width: video.0,
        video_height: video.1,
        scan: video.2,
        audio_channels: audio.0,
        audio_layout: audio.1,
        observed_at,
    })
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
    /// Read-only transport negotiation; old owners omit it and retain v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) start_protocols: Vec<u8>,
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
    /// Every tuner plurx may use is held. The one refusal that names what is
    /// in the way (holders, watchable) — see `http::live_tv::capacity_error`.
    Capacity(String),
    /// The owner's video encoder pool refused the transcode this route needs.
    /// Not a tuner: a copy route on the same channel would have started. Kept
    /// apart from `Capacity` because the copy a viewer sees must not say
    /// "slots are busy" while every tuner sits idle.
    EncoderCapacity(String),
    TunerUnavailable(String),
    ChannelNotFound(String),
    DrmUnsupported(String),
    CodecUnsupported(String),
    StartupTimeout(String),
    SourceFormatChanged(String),
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
            | Self::EncoderCapacity(message)
            | Self::TunerUnavailable(message)
            | Self::ChannelNotFound(message)
            | Self::DrmUnsupported(message)
            | Self::CodecUnsupported(message)
            | Self::StartupTimeout(message)
            | Self::SourceFormatChanged(message)
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
            Self::EncoderCapacity(_) => "encoder_capacity",
            Self::TunerUnavailable(_) => "tuner_unavailable",
            Self::ChannelNotFound(_) => "channel_not_found",
            Self::DrmUnsupported(_) => "drm_unsupported",
            Self::CodecUnsupported(_) => "codec_unsupported",
            Self::StartupTimeout(_) => "startup_timeout",
            Self::SourceFormatChanged(_) => "source_format_changed",
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
    /// Set only after parsing the separately signed v2 internal shape. It is
    /// skipped in v1 JSON so old-owner signatures and bodies remain exact.
    #[serde(skip)]
    pub(crate) playback: Option<LivePlaybackRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvStartRequestV2 {
    pub(crate) expected_owner_node_id: String,
    pub(crate) source_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) user_name: String,
    pub(crate) request_id: String,
    pub(crate) channel_id: String,
    pub(crate) config_generation: i64,
    pub(crate) source_serving_generation: u64,
    pub(crate) playback: LivePlaybackRequest,
}

impl From<LiveTvStartRequestV2> for LiveTvStartRequest {
    fn from(request: LiveTvStartRequestV2) -> Self {
        Self {
            expected_owner_node_id: request.expected_owner_node_id,
            source_node_id: request.source_node_id,
            user_id: request.user_id,
            user_name: request.user_name,
            request_id: request.request_id,
            channel_id: request.channel_id,
            config_generation: request.config_generation,
            source_serving_generation: request.source_serving_generation,
            playback: Some(request.playback),
        }
    }
}

impl From<&LiveTvStartRequest> for LiveTvStartRequestV2 {
    fn from(request: &LiveTvStartRequest) -> Self {
        Self {
            expected_owner_node_id: request.expected_owner_node_id.clone(),
            source_node_id: request.source_node_id.clone(),
            user_id: request.user_id,
            user_name: request.user_name.clone(),
            request_id: request.request_id.clone(),
            channel_id: request.channel_id.clone(),
            config_generation: request.config_generation,
            source_serving_generation: request.source_serving_generation,
            playback: request.playback.clone().expect("v2 start has playback"),
        }
    }
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
    Init { capability: String },
    Fragment { capability: String, sequence: u64 },
    Status { capability: String },
    Keepalive { capability: String },
}

impl LiveTvResourceRequest {
    pub(crate) fn capability(&self) -> &str {
        match self {
            Self::Playlist { capability }
            | Self::Segment { capability, .. }
            | Self::Init { capability }
            | Self::Fragment { capability, .. }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery: Option<LiveDeliveryPlan>,
    pub(crate) config_generation: i64,
    pub(crate) owner_serving_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveTvActivated {
    pub(crate) session_id: String,
    pub(crate) playlist_url: String,
    pub(crate) channel: LiveTvChannel,
    pub(crate) output: LiveTvOutput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery: Option<LiveDeliveryPlan>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery: Option<LiveDeliveryPlan>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery: Option<LiveDeliveryPlan>,
    /// What is on the channel this viewer is watching, when a guide is
    /// configured and has an answer. Bounded like every other row field; the
    /// Activity page shows it after the channel name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) programme_title: Option<String>,
}

/// What a retire did. `Stopped` means a stream was running and is not any
/// more; `Ended` means the owner had already reaped it; `Retired` means there
/// was nothing, and now the id can never produce anything either.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTvRetireOutcome {
    Stopped,
    Ended,
    Retired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTvResumeOutcome {
    Live,
    Pending,
    Ended,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LiveTvResumeAnswer {
    pub(crate) outcome: LiveTvResumeOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<LiveTvActivated>,
}

impl LiveTvResumeAnswer {
    fn outcome(outcome: LiveTvResumeOutcome) -> Self {
        Self {
            outcome,
            session: None,
        }
    }
}

/// A poll of a start's fate. Deliberately never a capability: this route is a
/// status read, and a handle that produces a stream belongs on `resume`, which
/// is defined to hand back exactly one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LiveTvStartState {
    /// Owned rather than `&'static str` so an ingress can decode the owner's
    /// answer into the very same type it would have produced locally.
    pub(crate) state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvRetireRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) request_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvResumeRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) request_id: String,
}

/// Its own path rather than a flag on the resume body, because it is its own
/// operation: `resume` selects one session and cancels the others, touches
/// what it selects and retires an id it has never seen. A status read that
/// did any of that would fence a viewer's start the moment anyone looked at
/// it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTvStartStateRequest {
    pub(crate) expected_owner_node_id: String,
    pub(crate) user_id: i64,
    pub(crate) request_id: String,
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
            source_node_id: String::new(),
            source_serving_generation: 0,
            user_id: request.user_id,
            request_id: request.request_id.clone(),
        }
    }
}

fn same_start(a: &LiveTvStartRequest, b: &LiveTvStartRequest) -> bool {
    a.user_id == b.user_id
        && a.request_id == b.request_id
        && a.channel_id == b.channel_id
        && a.config_generation == b.config_generation
        && a.playback == b.playback
}

struct LiveTvSessionState {
    phase: LiveTvSessionPhase,
    startup: Option<Result<LiveTvProvisional, LiveTvError>>,
    activated: bool,
    provisional_at: Option<tokio::time::Instant>,
    last_touch: tokio::time::Instant,
    last_progress: tokio::time::Instant,
    newest_listed: Option<u64>,
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
    device_id: String,
    output: StdMutex<LiveTvOutput>,
    delivery: StdMutex<Option<LiveDeliveryPlan>>,
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
    /// Retained until the owned child and stderr reader have settled.
    source_format_changed: Arc<AtomicBool>,
    encoder_diagnostic: Arc<StdMutex<Option<&'static str>>>,
    source_format: Arc<StdMutex<Option<LiveTvSourceFormat>>>,
    source_format_expires_at: Arc<AtomicI64>,
    /// Bytes the tuner has delivered into the graph. Shared with the pump so
    /// the startup decision can tell a slow channel from an absent one.
    tuner_bytes: Arc<AtomicU64>,
    /// A worker may be restarted around the first publishable inventory. The
    /// qualification inventory counts the accepted session once, not once per
    /// worker that reaches the same publication boundary.
    codec_qualification_counted: AtomicBool,
    resource_admission: Arc<tokio::sync::Semaphore>,
    signal_cache: tokio::sync::Mutex<Option<(tokio::time::Instant, Option<LiveTvSignalStatus>)>>,
    state: StdMutex<LiveTvSessionState>,
    /// The shared tuner connection this viewer reads, fixed at admission
    /// (plan L-03 §3.1). The transport authorises nothing: this session's
    /// capability, activation and fences are its own.
    transport: StdMutex<Option<Arc<dvr::DvrTransport>>>,
    /// True while this session holds the seat it reserved on `transport` at
    /// admission and has neither attached nor given it back.
    seat_reserved: AtomicBool,
    consumer: StdMutex<Option<Arc<dvr::ViewerConsumer>>>,
}

struct LiveTvProcess {
    child: tokio::process::Child,
    sink: Option<crate::scratch_put::PutSink>,
    _job: crate::process_control::ChildJob,
    // Copy and audio-only conversion intentionally own no video permit.
    _admission: Option<crate::transcode::LiveAdmission>,
    stderr: Option<tokio::task::JoinHandle<()>>,
}

/// What a session whose cleanup failed still owns once it has left the
/// registry (plan L-02 §3.4). The session itself is retired at once, so it
/// stops counting against the tuner slots and against
/// `MAX_TERMINAL_TOMBSTONES`; this record keeps the child — and with it the
/// job and the encoder admission — and the scratch directory, until the
/// owner's retry loop has confirmed both gone. The directory is in
/// `scratch_claims` for as long as the record exists, so the hourly sweep
/// never removes a directory a still-alive FFmpeg may be writing into.
struct OrphanSession {
    capability: String,
    directory: PathBuf,
    /// Present while the child's exit is unconfirmed.
    process: Option<LiveTvProcess>,
    /// The removal of `directory` in flight, if any: at most one, on its own
    /// task, and never awaited under the `orphans` lock. A removal stalled on
    /// a hung mount is kept rather than replaced, so it pins one blocking
    /// thread and holds up nothing else.
    removal: Option<OrphanRemoval>,
    since: tokio::time::Instant,
    attempts: u32,
}

type OrphanRemoval = tokio::task::JoinHandle<io::Result<()>>;

/// Which orphans a drain must see gone before it may acknowledge.
enum OrphanScope {
    /// Only the orphans of the sessions being cancelled.
    Sessions,
    /// Every orphan (shutdown).
    All,
}

/// The incarnation file: who this process is, when it started, and when it
/// was last seen with something live.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedIncarnation {
    version: u32,
    incarnation: String,
    started_at: i64,
    last_seen_at: i64,
}

/// This owner's incarnation, and what the previous one left on disk.
///
/// Server-side only: a capability never carries it, so nothing a client
/// holds changes. It exists so an unknown capability can be answered
/// honestly — "the owner restarted" only when a previous process was seen
/// recently enough to have been serving it, and otherwise "not available",
/// never "expired", which the owner cannot know.
struct OwnerIncarnation {
    path: Option<PathBuf>,
    current: StdMutex<PersistedIncarnation>,
    /// `last_seen_at` of the previous process, if it left a readable file.
    prior_seen_at: Option<i64>,
    last_touch: StdMutex<tokio::time::Instant>,
}

impl OwnerIncarnation {
    fn adopt(path: Option<PathBuf>) -> Self {
        let prior_seen_at = path.as_deref().and_then(|path| {
            let bytes = std::fs::read(path).ok()?;
            let prior: PersistedIncarnation = serde_json::from_slice(&bytes).ok()?;
            (prior.version == INCARNATION_VERSION).then_some(prior.last_seen_at)
        });
        let now = unix_seconds();
        let incarnation = Self {
            path,
            current: StdMutex::new(PersistedIncarnation {
                version: INCARNATION_VERSION,
                incarnation: uuid::Uuid::new_v4().simple().to_string(),
                started_at: now,
                last_seen_at: now,
            }),
            prior_seen_at,
            last_touch: StdMutex::new(tokio::time::Instant::now()),
        };
        incarnation.write();
        incarnation
    }

    /// Record `last_seen_at = now`. Small and synchronous; the loop that
    /// calls it runs it on the blocking pool.
    fn write(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let current = {
            let mut current = self
                .current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            current.last_seen_at = unix_seconds();
            current.clone()
        };
        let written = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec(&current)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(error) = written {
            tracing::warn!(kind = %error.kind(), "recording the Live TV owner incarnation");
        }
    }

    /// Whether a touch is due: something is live and the last one is
    /// `INCARNATION_TOUCH` old.
    fn touch_due(&self, held: usize) -> bool {
        if held == 0 {
            return false;
        }
        let mut last = self
            .last_touch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.elapsed() < INCARNATION_TOUCH {
            return false;
        }
        *last = tokio::time::Instant::now();
        true
    }

    /// The sentence for a capability that is neither registered nor
    /// tombstoned here. The code stays `capability_expired` (410, retry now)
    /// whichever sentence it carries; only the words change.
    fn unknown_capability_message(&self, now_unix: i64) -> &'static str {
        let restarted = self.prior_seen_at.is_some_and(|seen| {
            now_unix.saturating_sub(seen) <= INCARNATION_RESTART_WINDOW.as_secs() as i64
        });
        if restarted {
            "the session server restarted; press Watch to start again"
        } else {
            "this live-TV session is not available on the session server; press Watch to start again"
        }
    }
}

impl LiveTvSession {
    fn transport(&self) -> Option<Arc<dvr::DvrTransport>> {
        self.transport
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Bind this session to the transport it was admitted to, holding the
    /// seat the admission reserved.
    fn hold_seat_on(&self, transport: Arc<dvr::DvrTransport>) {
        *self
            .transport
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(transport);
        self.seat_reserved.store(true, Ordering::Release);
    }

    fn release_seat(&self) {
        if self.seat_reserved.swap(false, Ordering::AcqRel) {
            if let Some(transport) = self.transport() {
                transport.warm_opener_done(&self.capability);
                transport.release_seat();
            }
        }
    }

    /// Attach this viewer's queue to its transport, then give back the seat
    /// that kept the transport from retiring while its FFmpeg was built.
    fn attach_consumer(&self, consumer: Arc<dvr::ViewerConsumer>) -> Result<(), LiveTvError> {
        let transport = self.transport().ok_or_else(|| {
            LiveTvError::StreamFailed("the live-TV session has no tuner connection".into())
        })?;
        let attached = transport.attach_viewer(Arc::clone(&consumer));
        *self
            .consumer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(consumer);
        self.release_seat();
        if attached {
            Ok(())
        } else {
            Err(transport.terminal_error().unwrap_or_else(|| {
                LiveTvError::StreamFailed(
                    "the shared tuner connection closed before this viewer attached".into(),
                )
            }))
        }
    }

    /// Detach on this session's own end: no reference is left on the
    /// transport, and an unused seat is returned.
    fn detach_from_transport(&self) {
        if let Some(transport) = self.transport() {
            transport.detach_viewer(&self.capability);
        }
        self.release_seat();
    }

    /// Whether cancelling this session would free a tuner: it is the only
    /// thing its transport feeds or is waiting to feed.
    fn eviction_frees_a_tuner(&self) -> bool {
        let Some(transport) = self.transport() else {
            return false;
        };
        if transport.is_closing() || !transport.live_sinks().is_empty() {
            return false;
        }
        let others = transport
            .live_viewers()
            .iter()
            .filter(|viewer| viewer.capability != self.capability)
            .count();
        let own_seat = usize::from(self.seat_reserved.load(Ordering::Acquire));
        others == 0 && transport.pending_seats() <= own_seat
    }

    fn record_codec_qualification_publication(
        &self,
        transcode: &crate::transcode::TranscodeManager,
        admission: Option<&crate::transcode::LiveAdmission>,
    ) {
        if self
            .codec_qualification_counted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Some(admission) = admission {
            // Live TV currently refuses routes that require a tone-map graph,
            // so it contributes an encoder start but cannot truthfully claim
            // one of the movie/VOD pipeline labels.
            transcode.record_codec_qualification_session(
                admission.encoder,
                plurx_core::transcode::OutputGrade::Sdr,
                None,
            );
        }
    }

    fn output(&self) -> LiveTvOutput {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn delivery(&self) -> Option<LiveDeliveryPlan> {
        self.delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_delivery(&self, delivery: LiveDeliveryPlan) {
        let output = LiveTvOutput {
            container: "hls".to_owned(),
            video: delivery.output.video_codec.clone(),
            audio: delivery.output.audio_codec.clone(),
            height: delivery.output.height,
        };
        *self
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = output;
        *self
            .delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(delivery);
    }

    /// The caption formats this viewer's player decodes, when it said.
    fn caption_decoders(&self) -> Option<Vec<String>> {
        self.request
            .playback
            .as_ref()
            .and_then(crate::live_tv_delivery::LivePlaybackRequest::caption_decoders)
    }

    fn channel_with_source_format(&self) -> LiveTvChannel {
        let mut channel = self.channel.clone();
        // `self.channel` can contain a cached observation copied from lineup
        // at tune time. It is only the immutable channel baseline; freshness
        // belongs to the separately expiring session observation below.
        channel.source_format = None;
        if unix_seconds() <= self.source_format_expires_at.load(Ordering::Acquire) {
            if let Some(format) = self
                .source_format
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                channel.source_format = Some(format);
            }
        }
        channel
    }

    fn status(&self, owner_node_id: &str) -> LiveTvSessionStatus {
        let now = tokio::time::Instant::now();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        LiveTvSessionStatus {
            state: session_phase_name(state.phase).to_owned(),
            channel: self.channel_with_source_format(),
            owner_node_id: owner_node_id.to_owned(),
            encoder: state.encoder.clone(),
            output_height: self.output().height,
            delivery: self.delivery(),
            media_sequence: state.media_sequence,
            age_seconds: now.duration_since(self.started).as_secs(),
            idle_seconds: now.duration_since(state.last_touch).as_secs(),
            signal: None,
            error: state.error.clone(),
        }
    }
}

/// What a viewer's start would join or open: the transport key and the
/// device identity a join must match.
struct TransportSeat<'a> {
    channel_id: &'a str,
    device_id: &'a str,
    address: Ipv4Addr,
    /// The authority's running floor when this viewer was admitted: a
    /// transport opened under any generation at or above it is still running.
    running_floor: u64,
}

enum ViewerAdmission {
    /// Take a seat on this channel's transport; no tuner slot.
    Join(Arc<dvr::DvrTransport>),
    /// Open a new transport; one tuner slot.
    Open,
    /// This transport is closing or about to; wait for its close, then
    /// decide once more.
    WaitFor(Arc<dvr::DvrTransport>),
    /// Cancel this viewer's own stray, whose transport it alone holds, wait
    /// for that transport to close, then decide once more.
    EvictStray(Arc<LiveTvSession>, Arc<dvr::DvrTransport>),
    Refuse(LiveTvError),
}

#[derive(Default)]
struct LiveTvRegistry {
    closing: bool,
    sessions: HashMap<String, Arc<LiveTvSession>>,
    /// One entry per channel being recorded, however many recordings share
    /// it. A transport is one tuner GET, so this — not the number of
    /// recordings — is what counts against the tuner limit.
    transports: HashMap<String, Arc<dvr::DvrTransport>>,
    requests: HashMap<LiveTvRequestKey, String>,
    terminals: HashMap<String, LiveTvTerminalTombstone>,
    /// `(user_id, request_id)` a viewer has retired, and when that fact
    /// expires. Deliberately **not** counted against `MAX_TERMINAL_TOMBSTONES`:
    /// any signed-in viewer could otherwise fill the shared cap with 256 retire
    /// calls and refuse every other viewer's start for a minute.
    /// The sequence number is the tie-break: two retires inside one clock tick
    /// have the same expiry, and `min_by_key` over a HashMap would then evict
    /// an arbitrary one — including the entry just inserted.
    retired: HashMap<(i64, String), (tokio::time::Instant, u64)>,
    next_retire_sequence: u64,
}

#[derive(Clone)]
struct LiveTvTerminalTombstone {
    error: LiveTvError,
    request: LiveTvStartRequest,
    expires_at: tokio::time::Instant,
}

impl LiveTvRegistry {
    /// Tuner slots in use: one per transport, however many viewers and
    /// recordings share it (plan L-03 §3.4). A viewer joining a transport
    /// holds no slot; sharing changes what a slot counts, never how many
    /// there are.
    fn held(&self) -> usize {
        self.transports.len()
    }

    /// Transports that count against the recording reserve.
    fn recording_transports(&self) -> usize {
        self.transports
            .values()
            .filter(|transport| transport.is_recording())
            .count()
    }

    /// Whether a *new* transport may be opened for a recording.
    ///
    /// Two conditions, both about occupancy rather than arrival order, so the
    /// answer does not depend on who got there first: nothing may exceed the
    /// tuner count, and recordings may never hold more than
    /// `max_sessions - reserve` of it. With four tuners and a reserve of one,
    /// "one viewer and three recordings" fits and "four recordings" does not,
    /// whichever order they arrived in.
    fn may_open_transport(&self, max_sessions: u8, reserve: u8) -> bool {
        Self::occupancy_admits(
            self.transports.len(),
            self.recording_transports(),
            max_sessions,
            reserve,
        )
    }

    /// Whether a recording may join a transport only viewers hold: it needs
    /// no tuner, but that transport becomes a recording transport.
    fn may_add_recording(&self, max_sessions: u8, reserve: u8) -> bool {
        self.recording_transports() < Self::recordable(max_sessions, reserve)
    }

    fn recordable(max_sessions: u8, reserve: u8) -> usize {
        usize::from(max_sessions).saturating_sub(usize::from(reserve.min(max_sessions)))
    }

    /// The two rules, over counts alone. Separated so the property can be
    /// asserted directly: "one viewer and three recordings" is a statement
    /// about occupancy, and building four real tuner sessions to check it
    /// would test the fixture rather than the rule.
    fn occupancy_admits(
        transports: usize,
        recording_transports: usize,
        max_sessions: u8,
        reserve: u8,
    ) -> bool {
        transports < usize::from(max_sessions)
            && recording_transports < Self::recordable(max_sessions, reserve)
    }

    /// How a viewer's start gets bytes: join the channel's transport, open a
    /// new one, wait for one that is closing, evict the viewer's own stray to
    /// free a tuner, or be refused (plan L-03 §3.4, as corrected in §2.4).
    ///
    /// A join needs the same channel, the same device (id and address) and a
    /// transport the owner's serving authority still keeps running (opened
    /// under the current generation, or before a loss that recovered inside
    /// the session grace). It does
    /// *not* need the same configuration generation: the decision recorded in
    /// §2.4 D7, since the transport carries raw tuner bytes and every other
    /// generation-scoped setting is applied by the joiner's own FFmpeg.
    fn viewer_admission(
        &self,
        seat: &TransportSeat<'_>,
        user_id: i64,
        now: tokio::time::Instant,
        max_sessions: u8,
    ) -> ViewerAdmission {
        if let Some(transport) = self.transports.get(seat.channel_id) {
            if transport.is_closing() {
                return ViewerAdmission::WaitFor(Arc::clone(transport));
            }
            // Stale or failed source facts do not stop a join: the joiner
            // re-probes the live edge before it plans (§2.4 D1, D5), so the
            // facts one start saw never decide every later viewer.
            let shareable = transport.same_tuner(seat.device_id, seat.address)
                && transport.owner_serving_generation >= seat.running_floor;
            if !shareable {
                // Never a second transport for one key. One nothing wants is
                // about to retire, so wait for it; one still in use is not
                // this viewer's to take.
                return if transport.wanted() {
                    ViewerAdmission::Refuse(LiveTvError::Capacity(
                        "this channel's tuner connection cannot be shared right now; try again \
                         when it ends"
                            .into(),
                    ))
                } else {
                    ViewerAdmission::WaitFor(Arc::clone(transport))
                };
            }
            if transport.viewer_seats() >= dvr::MAX_CONSUMERS_PER_TRANSPORT {
                return ViewerAdmission::Refuse(LiveTvError::Capacity(
                    "this channel already has as many viewers as one tuner connection feeds".into(),
                ));
            }
            return ViewerAdmission::Join(Arc::clone(transport));
        }
        if self.held() < usize::from(max_sessions) {
            return ViewerAdmission::Open;
        }
        // A transport on its way out still counts until it is removed. A
        // viewer who stopped one channel and started another must not be
        // refused for the moment in between: wait for that close instead.
        if let Some(leaving) = self
            .transports
            .values()
            .find(|transport| transport.is_closing() || !transport.wanted())
        {
            return ViewerAdmission::WaitFor(Arc::clone(leaving));
        }
        // Paul's ruling: a tuner this viewer may still be holding is never a
        // reason to refuse that same viewer. With sharing it only fires when
        // a new transport is needed, and only for a stray whose eviction
        // actually frees one (§2.4 D8).
        match self.stray_to_evict(user_id, now) {
            Some(stray) => {
                let transport = stray.transport();
                match transport {
                    Some(transport) => ViewerAdmission::EvictStray(stray, transport),
                    None => ViewerAdmission::Refuse(Self::full(max_sessions)),
                }
            }
            None => ViewerAdmission::Refuse(Self::full(max_sessions)),
        }
    }

    fn full(max_sessions: u8) -> LiveTvError {
        LiveTvError::Capacity(format!(
            "all {max_sessions} tuners plurx Live TV may use are in use"
        ))
    }

    fn prune_terminals(&mut self) {
        let now = tokio::time::Instant::now();
        self.terminals
            .retain(|_, tombstone| tombstone.expires_at > now);
        self.retired.retain(|_, (expires_at, _)| *expires_at > now);
    }

    /// Whether the recovery history has room for one more request. The start
    /// path asks this in the same lock hold as, and just after,
    /// `request_session`, which has already pruned expired tombstones.
    fn recovery_history_admits(&self) -> bool {
        self.sessions.len() + self.terminals.len() < MAX_TERMINAL_TOMBSTONES
    }

    /// Every session a viewer's public request id currently maps to. Usually
    /// zero or one — but `LiveTvRequestKey` includes the ingress's serving
    /// generation, so a fence blip between a POST and its replay legitimately
    /// produces two. Every public operation is defined over the whole set:
    /// one public identity ends up with one stream either way.
    fn sessions_for_public_request(
        &self,
        user_id: i64,
        request_id: &str,
    ) -> Vec<Arc<LiveTvSession>> {
        self.requests
            .iter()
            .filter(|(key, _)| key.user_id == user_id && key.request_id == request_id)
            .filter_map(|(_, capability)| self.sessions.get(capability).cloned())
            .collect()
    }

    fn tombstone_for_public_request(
        &self,
        user_id: i64,
        request_id: &str,
    ) -> Option<&LiveTvTerminalTombstone> {
        self.terminals.values().find(|entry| {
            entry.request.user_id == user_id && entry.request.request_id == request_id
        })
    }

    /// Record that this viewer will never use this request id again. Bounded
    /// per user so one viewer cannot grow the map without limit.
    fn retire(&mut self, user_id: i64, request_id: String, now: tokio::time::Instant) {
        let sequence = self.next_retire_sequence;
        self.next_retire_sequence = self.next_retire_sequence.saturating_add(1);
        self.retired
            .insert((user_id, request_id), (now + RETIRED_TTL, sequence));
        let mine = self
            .retired
            .iter()
            .filter(|((owner, _), _)| *owner == user_id)
            .count();
        for _ in MAX_RETIRED_PER_USER..mine {
            let Some(oldest) = self
                .retired
                .iter()
                .filter(|((owner, _), _)| *owner == user_id)
                .min_by_key(|(_, (expires_at, sequence))| (*expires_at, *sequence))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.retired.remove(&oldest);
        }
    }

    /// Which of this viewer's sessions, if any, to cancel so their new start
    /// can be admitted when every slot is full.
    ///
    /// Idle is only meaningful for a session that has a heartbeat to miss:
    /// `last_touch` is set at admission, and a starting or provisional session
    /// cannot yet be touched by a player, so neither is ever evicted for
    /// idleness — both are already bounded by the startup and provisional
    /// timeouts. Only a request id the viewer has retired evicts those.
    ///
    /// Another viewer's session is never a candidate, however idle. The ruling
    /// is that a viewer's own stray must not refuse them, not that anyone may
    /// take anyone's tuner.
    ///
    /// Nor is a stray that shares its transport with anyone else (another
    /// viewer, a recording, an admitted start): cancelling it would free no
    /// tuner and cost its viewer the stream for nothing (§2.4 D8).
    fn stray_to_evict(
        &self,
        user_id: i64,
        now: tokio::time::Instant,
    ) -> Option<Arc<LiveTvSession>> {
        self.sessions
            .values()
            .filter(|session| session.request.user_id == user_id && !session.cancel.is_cancelled())
            .filter(|session| session.eviction_frees_a_tuner())
            .filter(|session| {
                let idle_active = {
                    let state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.activated
                        && state.phase == LiveTvSessionPhase::Active
                        && now.duration_since(state.last_touch) >= STRAY_EVICTION_IDLE
                };
                idle_active || self.is_retired(user_id, &session.request.request_id)
            })
            .min_by_key(|session| {
                session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last_touch
            })
            .cloned()
    }

    /// Checks the expiry inline rather than trusting a prune to have run: a
    /// status read on an idle owner would otherwise keep answering "retired"
    /// hours past the minute this map documents.
    fn is_retired(&self, user_id: i64, request_id: &str) -> bool {
        self.retired
            .get(&(user_id, request_id.to_owned()))
            .is_some_and(|(expires_at, _)| *expires_at > tokio::time::Instant::now())
    }

    fn request_session(
        &mut self,
        request: &LiveTvStartRequest,
    ) -> Result<Option<Arc<LiveTvSession>>, LiveTvError> {
        self.prune_terminals();
        if self.is_retired(request.user_id, &request.request_id) {
            return Err(LiveTvError::Conflict(
                "the live-TV request id was retired".into(),
            ));
        }
        let key = LiveTvRequestKey::from(request);
        if let Some(tombstone) = self
            .terminals
            .values()
            .find(|entry| LiveTvRequestKey::from(&entry.request) == key)
        {
            if !same_start(&tombstone.request, request) {
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
        if !same_start(&session.request, request) {
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
    /// Ends broken out by `LiveTvError::code()`. A single counter said how many
    /// sessions stopped and nothing about why, which is the question an
    /// operator actually has.
    ends: StdMutex<BTreeMap<&'static str, u64>>,
    stray_evictions: AtomicU64,
    relay_bytes: Arc<AtomicU64>,
    /// Guide refresh outcomes by source, and a projection of the cache the
    /// same shape as the device one: a value plus when it was observed, so a
    /// stalled refresh loop reports staleness instead of a frozen number.
    guide_refreshes: StdMutex<BTreeMap<(&'static str, &'static str), u64>>,
    guide: StdMutex<Option<(tokio::time::Instant, Duration, usize)>>,
    /// Fixed-cardinality DVR sink failure reasons. A per-sink writer records
    /// exactly once when its attempt ends; recording ids never become labels.
    dvr_sink_failures: [AtomicU64; 3],
    /// Every consistent read of the replicated settings this subsystem makes,
    /// by the site that took it and by outcome, indexed
    /// `[SettingsReadSite][ok, failed]`. `observer` is the node's settings
    /// loop: one read per second while a tuner transport is held, one at boot
    /// and one per settings-generation change otherwise, and the only read
    /// the per-second session fence depends on. `fence` is the direct
    /// read the start fence and the pre-publication fence each take, so it
    /// moves with starts rather than with running sessions. `request` is every
    /// other caller of `LiveTvManager::config` — each public and internal
    /// Live TV handler (guide, lineup, status, start, stop, ...) and the guide
    /// refresh paths — so it scales with API traffic, not with viewers. The
    /// read rate the session fence costs is `observer` + `fence`; the node's
    /// whole Live TV settings read rate is the sum of all three.
    settings_reads: [[AtomicU64; 2]; 3],
    /// Session fence ticks by the state of the observation they validated
    /// against: fresh, in grace, expired. Indexed by `FenceFreshness`.
    fence_observations: [AtomicU64; 3],
    /// Whether the settings observer is parked on a live change source with
    /// its last reading still proved. While it is, the enabled gauge keeps
    /// that reading however old it is, because a settings change anywhere
    /// would have woken the observer; while it is not, the gauge goes stale
    /// three seconds after the last read, as it always did. So the gauge
    /// blanks when the bounded proof is lost, not when the node is idle.
    settings_watched: AtomicBool,
    /// Consumers a shared transport stopped feeding, indexed
    /// `[viewer, recording][backlog, fenced]`: four fixed series.
    consumer_evictions: [[AtomicU64; 2]; 2],
    /// Viewer plans by where their facts came from, indexed by
    /// `StartPlanSource`.
    start_plans: [AtomicU64; 4],
    /// Orphans the owner still holds, `[process, scratch]`: a record counts
    /// under `process` while its child is unconfirmed, and under `scratch`
    /// once only its directory is left. Both return to zero.
    orphans: [AtomicU64; 2],
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

    fn observe_session_end(&self, reason: &'static str) {
        *self
            .ends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(reason)
            .or_default() += 1;
    }

    fn observe_stray_eviction(&self) {
        self.stray_evictions.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn observe_consumer_eviction(&self, kind: &'static str, reason: &'static str) {
        let kind = match kind {
            "viewer" => 0,
            "recording" => 1,
            _ => return,
        };
        let reason = match reason {
            "backlog" => 0,
            "fenced" => 1,
            _ => return,
        };
        self.consumer_evictions[kind][reason].fetch_add(1, Ordering::Relaxed);
    }

    /// The shared-transport series (plan L-03 §5.1): how many tuner GETs
    /// this owner holds, how many consumers of each kind they feed, and how
    /// many consumers were cut off and why.
    fn transports_prometheus(registry: &LiveTvRegistry, evictions: &[[AtomicU64; 2]; 2]) -> String {
        let mut viewers = 0usize;
        let mut recordings = 0usize;
        for transport in registry.transports.values() {
            viewers += transport.live_viewers().len();
            recordings += transport.live_sinks().len();
        }
        let mut out = format!(
            "# HELP plurx_live_tv_transports Tuner connections this owner holds, however many viewers and recordings share each.\n\
             # TYPE plurx_live_tv_transports gauge\n\
             plurx_live_tv_transports {}\n\
             # HELP plurx_live_tv_transport_consumers Viewers and recordings attached to this owner's tuner connections.\n\
             # TYPE plurx_live_tv_transport_consumers gauge\n\
             plurx_live_tv_transport_consumers{{kind=\"viewer\"}} {viewers}\n\
             plurx_live_tv_transport_consumers{{kind=\"recording\"}} {recordings}\n\
             # HELP plurx_live_tv_consumer_evictions_total Consumers a shared tuner connection stopped feeding: backlog (its bounded queue filled) or fenced (the owner lost serving authority).\n\
             # TYPE plurx_live_tv_consumer_evictions_total counter\n",
            registry.transports.len(),
        );
        for (kind_index, kind) in ["viewer", "recording"].into_iter().enumerate() {
            for (reason_index, reason) in ["backlog", "fenced"].into_iter().enumerate() {
                out.push_str(&format!(
                    "plurx_live_tv_consumer_evictions_total{{kind=\"{kind}\",reason=\"{reason}\"}} {}\n",
                    evictions[kind_index][reason_index].load(Ordering::Acquire),
                ));
            }
        }
        out
    }

    fn set_orphans(&self, orphans: &[OrphanSession]) {
        let process = orphans
            .iter()
            .filter(|orphan| orphan.process.is_some())
            .count();
        self.orphans[0].store(process as u64, Ordering::Release);
        self.orphans[1].store((orphans.len() - process) as u64, Ordering::Release);
    }

    fn observe_start_plan(&self, source: StartPlanSource) {
        self.start_plans[source as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// `warm_disagreed` should be rare: when it is not, the facts cache
    /// outlives the mux format and its expiry is wrong.
    fn start_plans_prometheus(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_live_tv_start_plan_total Live TV viewer plans by where their source facts came from: cold (this tune's opening probe), joined (a shared transport's facts), warm_agreed (cached facts this tune's probe then re-proved), warm_disagreed (cached facts it contradicted; the start restarted cold).\n\
             # TYPE plurx_live_tv_start_plan_total counter\n",
        );
        for (index, label) in StartPlanSource::LABELS.into_iter().enumerate() {
            out.push_str(&format!(
                "plurx_live_tv_start_plan_total{{source=\"{label}\"}} {}\n",
                self.start_plans[index].load(Ordering::Acquire),
            ));
        }
        out
    }

    fn orphans_prometheus(&self) -> String {
        format!(
            "# HELP plurx_live_tv_orphans Ended Live TV sessions whose cleanup the owner is still retrying: process (FFmpeg exit unconfirmed; it keeps its encoder slot) or scratch (only the directory is left).\n\
             # TYPE plurx_live_tv_orphans gauge\n\
             plurx_live_tv_orphans{{kind=\"process\"}} {}\n\
             plurx_live_tv_orphans{{kind=\"scratch\"}} {}\n",
            self.orphans[0].load(Ordering::Acquire),
            self.orphans[1].load(Ordering::Acquire),
        )
    }

    fn observe_settings_read(&self, site: SettingsReadSite, ok: bool) {
        self.settings_reads[site as usize][usize::from(!ok)].fetch_add(1, Ordering::Relaxed);
    }

    /// The series an operator watches after this ships. `expired` must stay at
    /// zero outside a real settings-store outage; a `grace` that is never zero
    /// says the consistent read is failing often enough to matter even though
    /// nothing has stopped yet.
    fn fence_observations_prometheus(&self) -> String {
        format!(
            "# HELP plurx_live_tv_fence_observations_total Live TV session fence ticks by the state of the settings observation they validated against.\n\
             # TYPE plurx_live_tv_fence_observations_total counter\n\
             plurx_live_tv_fence_observations_total{{state=\"fresh\"}} {}\n\
             plurx_live_tv_fence_observations_total{{state=\"grace\"}} {}\n\
             plurx_live_tv_fence_observations_total{{state=\"expired\"}} {}\n",
            self.fence_observations[0].load(Ordering::Acquire),
            self.fence_observations[1].load(Ordering::Acquire),
            self.fence_observations[2].load(Ordering::Acquire),
        )
    }

    /// `site="observer"` is the one the session fence runs on and should read
    /// one per second per node; `site="fence"` moves with starts;
    /// `site="request"` is API traffic and has no fixed rate.
    fn settings_reads_prometheus(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_live_tv_settings_reads_total Consistent reads of the replicated Live TV settings, by site (observer: the node's loop the session fence validates against, 1 Hz while a tuner is held; fence: the direct start and pre-publication fences; request: Live TV API handlers and guide refresh) and outcome.\n\
             # TYPE plurx_live_tv_settings_reads_total counter\n",
        );
        for site in SettingsReadSite::ALL {
            for (index, outcome) in ["ok", "failed"].into_iter().enumerate() {
                out.push_str(&format!(
                    "plurx_live_tv_settings_reads_total{{site=\"{}\",outcome=\"{outcome}\"}} {}\n",
                    site.as_str(),
                    self.settings_reads[site as usize][index].load(Ordering::Acquire),
                ));
            }
        }
        out
    }

    fn observe_dvr_sink_failure(&self, reason: &'static str) {
        let index = match reason {
            "disk_write_failed" => 0,
            "disk_write_timeout" => 1,
            "disk_write_backlog" => 2,
            _ => return,
        };
        self.dvr_sink_failures[index].fetch_add(1, Ordering::Relaxed);
    }

    fn dvr_sink_failures_prometheus(&self) -> String {
        format!(
            "# HELP plurx_dvr_sink_failures_total DVR capture attempts ended by a sink-local write failure.\n\
             # TYPE plurx_dvr_sink_failures_total counter\n\
             plurx_dvr_sink_failures_total{{reason=\"disk_write_failed\"}} {}\n\
             plurx_dvr_sink_failures_total{{reason=\"disk_write_timeout\"}} {}\n\
             plurx_dvr_sink_failures_total{{reason=\"disk_write_backlog\"}} {}\n",
            self.dvr_sink_failures[0].load(Ordering::Acquire),
            self.dvr_sink_failures[1].load(Ordering::Acquire),
            self.dvr_sink_failures[2].load(Ordering::Acquire),
        )
    }

    /// One line per reason, and a zero line when nothing has ended yet so the
    /// series exists before the first failure rather than after it.
    fn session_ends_prometheus(&self) -> String {
        let ends = self
            .ends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut out = String::from(
            "# HELP plurx_live_tv_session_ends_total Live TV sessions that ended, by reason.\n\
             # TYPE plurx_live_tv_session_ends_total counter\n",
        );
        if ends.is_empty() {
            out.push_str("plurx_live_tv_session_ends_total{reason=\"released\"} 0\n");
        }
        for (reason, value) in &ends {
            out.push_str(&format!(
                "plurx_live_tv_session_ends_total{{reason=\"{reason}\"}} {value}\n"
            ));
        }
        out
    }

    fn observe_guide(&self, programmes: usize) {
        self.observe_guide_at(tokio::time::Instant::now(), Duration::ZERO, programmes);
    }

    /// The same observation for a guide this process did not fetch. The offset
    /// is the wall-clock age it already had, so the gauge reports the copy's
    /// real age rather than the moment it was adopted.
    fn observe_guide_at(
        &self,
        observed: tokio::time::Instant,
        age_offset: Duration,
        programmes: usize,
    ) {
        let mut projection = self
            .guide
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *projection = Some((observed, age_offset, programmes));
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
            Some((observed, age_offset, programmes)) => (
                (age_offset + tokio::time::Instant::now().duration_since(observed))
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
    /// The same `Notify` the guide refresh loop awaits. A lineup that fills for
    /// the first time — or for a new generation — is the event the loop was
    /// otherwise sleeping twenty minutes through. Signalled from here rather
    /// than from the manager because this is the only place that can tell a
    /// cold fill from a warm one.
    lineup_filled: Arc<tokio::sync::Notify>,
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self::with_wake(Arc::new(tokio::sync::Notify::new()))
    }
}

impl SnapshotCache {
    fn with_wake(lineup_filled: Arc<tokio::sync::Notify>) -> Self {
        Self {
            state: tokio::sync::Mutex::new(SnapshotCacheState::default()),
            forced_admission: tokio::sync::Semaphore::new(MAX_FORCED_REFRESH_CALLERS),
            lineup_filled,
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
                // Computed before the replacement: afterwards there is no way
                // to tell whether this generation had a lineup a moment ago.
                let was_cold = state
                    .snapshot
                    .as_ref()
                    .is_none_or(|cached| cached.generation != generation);
                state.snapshot = Some(CachedSnapshot {
                    generation,
                    observed,
                    snapshot: snapshot.clone(),
                });
                if was_cold {
                    self.lineup_filled.notify_one();
                }
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
/// mutex around the state, one admission permit shared by manual/background
/// refreshes and cloned into blocking parsers, and generation equality as the
/// invalidation rule — with one difference: the guide has a refresh *loop*, so
/// a serving read never triggers a fetch. `GET /live-tv/guide` answers from
/// whatever is here, which is what makes it always fast and always answerable.
struct GuideCache {
    state: tokio::sync::Mutex<GuideCacheState>,
    refresh_admission: Arc<tokio::sync::Semaphore>,
    refresh_cancel: StdMutex<Option<(LiveTvConfig, CancellationToken)>>,
    next_sequence: AtomicU64,
    /// Where the owner's copy of the last good guide lives, so a deploy does
    /// not start the grid from nothing. `None` in tests and on a node that was
    /// built without a store path.
    persist: Option<PathBuf>,
    /// Shared with `SnapshotCache`: a cold lineup that just filled, or a
    /// settings generation that just changed, wakes the refresh loop instead
    /// of leaving it asleep for twenty minutes.
    wake: Arc<tokio::sync::Notify>,
    /// The last generation `observe_config` saw. `i64::MIN` until the first
    /// observation, so construction does not itself look like a change.
    observed_generation: AtomicI64,
    /// Bumped by every `invalidate`. A persist that was already in flight when
    /// the cache was discarded checks this before renaming its temp file into
    /// place, so a settings save cannot be undone by a write that started
    /// before it — which would otherwise leave the discarded guide on disk for
    /// the next process to adopt.
    discard_epoch: Arc<AtomicU64>,
    /// Serialises the two file operations, so a remove and a rename cannot
    /// interleave inside the filesystem.
    persist_gate: Arc<StdMutex<()>>,
    /// The settings generation of the copy currently on disk, for the
    /// Developer panel. `i64::MIN` when there is none.
    persisted_generation: AtomicI64,
}

#[derive(Default)]
struct GuideCacheState {
    cached: Option<CachedGuide>,
    published_sequence: u64,
    /// Kept separately from the cache so a refresh that failed while a good
    /// cache is still warm reports the failure without discarding the answer.
    /// Scoped to the generation it happened under: an error from the previous
    /// configuration is not a fact about the current one.
    last_error: Option<(i64, String)>,
    /// When the refresh loop next intends to run, unix seconds. Served on
    /// every guide answer — including an unavailable one, which still tells a
    /// client when asking again is worthwhile.
    next_refresh_at: Option<i64>,
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
    /// The wall-clock age this copy already had when it was adopted, zero for
    /// a guide this process fetched. `tokio::time::Instant` cannot represent a
    /// moment before the clock started — under `tokio::time::pause()` that is
    /// near zero — so an hour-old copy loaded from disk keeps its hour here
    /// rather than silently reporting as fresh.
    age_offset: Duration,
    fetched_at: i64,
    guide: Arc<LiveTvGuide>,
}

/// One immutable owner-side guide snapshot. Refresh replaces the cached
/// `Arc`; a scheduler tick that already cloned it keeps a consistent view.
pub(crate) struct GuideView {
    pub(crate) guide: Arc<LiveTvGuide>,
    pub(crate) fetched_at: i64,
    pub(crate) age: Duration,
    pub(crate) freshness: GuideFreshness,
}

impl CachedGuide {
    /// The only age anyone may use. Every reader adds the adoption offset.
    fn age_at(&self, now: tokio::time::Instant) -> Duration {
        self.age_offset + now.duration_since(self.observed)
    }
}

/// The on-disk shape. `fetched_at` is wall-clock, which is what makes the age
/// meaningful across a restart; the in-memory `observed` instant is
/// reconstructed from it on load.
#[derive(Serialize, Deserialize)]
struct PersistedGuide {
    version: u32,
    generation: i64,
    fetched_at: i64,
    guide: LiveTvGuide,
}

/// Anything else on disk is ignored rather than migrated: the guide is a
/// cache, and one refresh interval replaces it.
const PERSISTED_GUIDE_VERSION: u32 = 1;
const PERSISTED_GUIDE_FILE: &str = "guide.json";

impl Default for GuideCache {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(GuideCacheState::default()),
            refresh_admission: Arc::new(tokio::sync::Semaphore::new(1)),
            refresh_cancel: StdMutex::new(None),
            next_sequence: AtomicU64::new(1),
            persist: None,
            wake: Arc::new(tokio::sync::Notify::new()),
            observed_generation: AtomicI64::new(i64::MIN),
            discard_epoch: Arc::new(AtomicU64::new(0)),
            persist_gate: Arc::new(StdMutex::new(())),
            persisted_generation: AtomicI64::new(i64::MIN),
        }
    }
}

impl GuideCache {
    /// A cache that keeps the owner's last good guide on disk. The directory
    /// is a child of an already-managed persistent cache root, so it needs no
    /// storage-census entry of its own.
    fn with_store(dir: PathBuf) -> Self {
        let path = dir.join(PERSISTED_GUIDE_FILE);
        let cached = Self::load_persisted(&path);
        let cache = Self {
            persist: Some(path),
            ..Self::default()
        };
        if let Some(cached) = cached {
            cache
                .persisted_generation
                .store(cached.generation, Ordering::Release);
            // Uncontended: nothing else holds this mutex before the manager is
            // published. `try_lock` keeps the constructor synchronous.
            match cache.state.try_lock() {
                Ok(mut state) => state.cached = Some(cached),
                Err(_) => tracing::warn!(
                    "the persisted programme guide was read but could not be adopted"
                ),
            }
        }
        cache
    }

    /// Adopt a guide written by a previous process, or decide not to. Garbage,
    /// a version this build does not know, and a copy already past the stale
    /// window are all ignored; the file is left where it is, because the next
    /// successful refresh overwrites it anyway.
    fn load_persisted(path: &Path) -> Option<CachedGuide> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "reading the persisted programme guide"
                );
                return None;
            }
        };
        let mut persisted: PersistedGuide = match serde_json::from_slice(&bytes) {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "the persisted programme guide is unreadable; starting cold"
                );
                return None;
            }
        };
        if persisted.version != PERSISTED_GUIDE_VERSION {
            tracing::warn!(
                path = %path.display(),
                version = persisted.version,
                "ignoring a persisted programme guide written by another version"
            );
            return None;
        }
        if guide::normalise_guide_programmes(&mut persisted.guide) {
            tracing::warn!(
                path = %path.display(),
                "normalised unsorted programmes in the persisted guide cache"
            );
        }
        // Clamped: a clock step at boot must not discard a good copy, and must
        // not make one look like it came from the future.
        let age = Duration::from_secs(
            unix_seconds()
                .saturating_sub(persisted.fetched_at)
                .max(0)
                .unsigned_abs(),
        );
        if age > guide::GUIDE_STALE_TTL {
            return None;
        }
        let now = tokio::time::Instant::now();
        let (observed, age_offset) = match now.checked_sub(age) {
            Some(observed) => (observed, Duration::ZERO),
            // A process whose clock has been running for less time than the
            // guide's age has no instant that far back. Keep the age in the
            // offset instead of losing it.
            None => (now, age),
        };
        Some(CachedGuide {
            generation: persisted.generation,
            observed,
            age_offset,
            fetched_at: persisted.fetched_at,
            guide: Arc::new(persisted.guide),
        })
    }

    /// Write the copy out for the next process. Never an error to the refresh
    /// that produced it: a guide that could not be saved is still a guide.
    async fn persist_guide(&self, cached: &CachedGuide) {
        let Some(path) = self.persist.clone() else {
            return;
        };
        let epoch = self.discard_epoch.load(Ordering::Acquire);
        let persisted = PersistedGuide {
            version: PERSISTED_GUIDE_VERSION,
            generation: cached.generation,
            fetched_at: cached.fetched_at,
            guide: (*cached.guide).clone(),
        };
        let gate = Arc::clone(&self.persist_gate);
        let discard_epoch = Arc::clone(&self.discard_epoch);
        let written = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            // One writer at a time, and the rename checks that nobody
            // discarded the cache while the bytes were being encoded.
            let _held = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec(&persisted)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, &bytes)?;
            if discard_epoch.load(Ordering::Acquire) != epoch {
                let _ = std::fs::remove_file(&tmp);
                return Ok(());
            }
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })
        .await;
        match written {
            Ok(Ok(())) => self
                .persisted_generation
                .store(cached.generation, Ordering::Release),
            Ok(Err(error)) => {
                tracing::warn!(%error, "persisting the programme guide")
            }
            Err(error) => tracing::warn!(%error, "the programme-guide writer did not finish"),
        }
    }

    async fn discard_persisted(&self) {
        self.discard_epoch.fetch_add(1, Ordering::AcqRel);
        let Some(path) = self.persist.clone() else {
            return;
        };
        let gate = Arc::clone(&self.persist_gate);
        let removed = tokio::task::spawn_blocking(move || {
            let _held = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        })
        .await;
        self.persisted_generation.store(i64::MIN, Ordering::Release);
        if let Ok(Err(error)) = removed {
            tracing::warn!(%error, "removing the persisted programme guide");
        }
    }

    /// A permit, not a broadcast: `notify_one` stores a wake that arrives while
    /// the loop is mid-refresh, so a lineup that fills during a fetch is not
    /// lost.
    fn wake(&self) {
        self.wake.notify_one();
    }

    /// A settings save is the one skip reason that does not resolve itself, so
    /// it wakes the loop. The first observation is not a change.
    fn observe_generation(&self, generation: i64) {
        let previous = self.observed_generation.swap(generation, Ordering::AcqRel);
        if previous != i64::MIN && previous != generation {
            self.wake();
        }
    }

    async fn note_next_refresh(&self, at: i64) {
        self.state.lock().await.next_refresh_at = Some(at);
    }

    fn begin_refresh(&self, config: &LiveTvConfig) -> CancellationToken {
        let token = CancellationToken::new();
        let mut active = self
            .refresh_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, previous)) = active.replace((config.clone(), token.clone())) {
            previous.cancel();
        }
        token
    }

    fn cancel_if_config_changed(&self, config: &LiveTvConfig) {
        let active = self
            .refresh_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((started, token)) = active.as_ref() {
            if started != config {
                token.cancel();
            }
        }
    }

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
            .filter(|cached| cached.age_at(now) <= guide::GUIDE_STALE_TTL)
        else {
            let mut guide = LiveTvGuide::unavailable(
                config.guide_source,
                window,
                state.error_for(config.generation),
            );
            // An unavailable answer still says when to ask again — that is the
            // difference between a client polling on the owner's clock and one
            // guessing a cadence of its own.
            guide.next_refresh_at = state.next_refresh_at;
            return guide;
        };
        let age = cached.age_at(now);
        let cached_guide = Arc::clone(&cached.guide);
        let fetched_at = cached.fetched_at;
        let refresh_error = state.error_for(config.generation);
        let next_refresh_at = state.next_refresh_at;
        drop(state);
        let mut guide = cached_guide.clipped(&window);
        guide.age_seconds = age.as_secs();
        guide.fetched_at = Some(fetched_at);
        guide.freshness = if age <= guide::GUIDE_REFRESH_INTERVAL {
            GuideFreshness::Fresh
        } else {
            GuideFreshness::Stale
        };
        guide.refresh_error = refresh_error;
        guide.next_refresh_at = next_refresh_at;
        guide
    }

    /// The owner's full cached guide for this generation, never response-
    /// clipped and shared by reference. A refresh swaps the `Arc` rather than
    /// mutating a snapshot already held by a scheduler tick.
    async fn view(&self, generation: i64) -> Option<GuideView> {
        let state = self.state.lock().await;
        let now = tokio::time::Instant::now();
        state
            .cached
            .as_ref()
            .filter(|cached| cached.generation == generation)
            .filter(|cached| cached.age_at(now) <= guide::GUIDE_STALE_TTL)
            .map(|cached| {
                let age = cached.age_at(now);
                GuideView {
                    guide: Arc::clone(&cached.guide),
                    fetched_at: cached.fetched_at,
                    age,
                    freshness: if age <= guide::GUIDE_REFRESH_INTERVAL {
                        GuideFreshness::Fresh
                    } else {
                        GuideFreshness::Stale
                    },
                }
            })
    }

    /// The owner's own copy, unclipped and without the serving dressing — what
    /// the DVR scheduler matches rules against, and what an incremental
    /// refresh extends rather than refetching. `None` when nothing usable is
    /// cached for this generation.
    async fn owner_copy(&self, generation: i64) -> Option<LiveTvGuide> {
        self.view(generation)
            .await
            .map(|view| (*view.guide).clone())
    }

    /// A completed refresh. A failure keeps the previous cache and records the
    /// error; only a success replaces the content.
    async fn store(
        &self,
        generation: i64,
        sequence: u64,
        result: &Result<LiveTvGuide, LiveTvError>,
    ) -> bool {
        let stored = {
            let mut state = self.state.lock().await;
            if sequence < state.published_sequence {
                return false;
            }
            state.published_sequence = sequence;
            match result {
                Ok(guide) => {
                    let mut guide = guide.clone();
                    if guide::normalise_guide_programmes(&mut guide) {
                        tracing::warn!(
                            generation,
                            "normalised unsorted programmes before caching the guide"
                        );
                    }
                    let cached = CachedGuide {
                        generation,
                        observed: tokio::time::Instant::now(),
                        age_offset: Duration::ZERO,
                        fetched_at: unix_seconds(),
                        guide: Arc::new(guide),
                    };
                    state.cached = Some(cached.clone());
                    state.last_error = None;
                    Some(cached)
                }
                Err(error) => {
                    state.last_error = Some((generation, guide::sanitize_refresh_error(error)));
                    None
                }
            }
        };
        // Outside the state lock: the write is a blocking file operation and
        // every guide read would otherwise wait behind it.
        if let Some(cached) = stored {
            self.persist_guide(&cached).await;
        }
        true
    }

    /// A settings change discards the cache: the lineup it was matched
    /// against, and possibly the source itself, just changed.
    async fn invalidate(&self) {
        {
            let mut state = self.state.lock().await;
            state.cached = None;
            state.last_error = None;
        }
        self.discard_persisted().await;
    }

    /// What the Developer panel reports about the copy on disk: where it is,
    /// how big it is, and which settings generation it was matched under.
    /// Advisory only.
    fn persisted_status(&self) -> Option<(PathBuf, u64, Option<i64>)> {
        let path = self.persist.clone()?;
        // Metadata only. Parsing the document here would have meant a
        // multi-megabyte blocking read on a runtime worker every time an
        // administrator opened the Developer panel; the generation is the one
        // thing worth reporting and it is already known in memory.
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).ok()?;
        let generation = match self.persisted_generation.load(Ordering::Acquire) {
            i64::MIN => None,
            generation => Some(generation),
        };
        Some((path, bytes, generation))
    }

    /// How old the copy matched to this generation is, if there is one.
    async fn age_for(&self, generation: i64) -> Option<Duration> {
        let state = self.state.lock().await;
        let now = tokio::time::Instant::now();
        state
            .cached
            .as_ref()
            .filter(|cached| cached.generation == generation)
            .map(|cached| cached.age_at(now))
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

/// Fold the previously cached rows into a freshly fetched bulk answer.
///
/// Rows that ended before the new window's start are dropped — a guide is not
/// an archive — and everything else is merged per channel, where
/// `normalise_programmes`' de-duplication decides which copy of a repeated
/// airing survives. A channel present only in the cache is kept: the bulk call
/// answers a few hours, and dropping the tail every tick is exactly the
/// behaviour that made a long horizon impossible.
fn merge_carried_guide(
    channels: &mut Vec<LiveTvGuideChannel>,
    carried: LiveTvGuide,
    keep_from: i64,
) {
    let mut by_number = channels
        .iter()
        .enumerate()
        .map(|(index, channel)| (channel.guide_number.clone(), index))
        .collect::<BTreeMap<_, _>>();
    for mut previous in carried.channels {
        previous.programmes.retain(|row| row.end > keep_from);
        if previous.programmes.is_empty() {
            continue;
        }
        match by_number.get(&previous.guide_number) {
            Some(&index) => guide::merge_channel(&mut channels[index], previous),
            None => {
                by_number.insert(previous.guide_number.clone(), channels.len());
                channels.push(previous);
            }
        }
    }
}

/// Whether plurxd will POST a DVR event to this URL.
///
/// This is a **weaker** outbound boundary than any other the server has, and
/// deliberately so. `approved_guide_url` pins scheme, port and a two-host
/// allowlist because it carries a credential to a vendor's API. This carries
/// no credential and no secret — a reminder and a recording's start and finish
/// — and its whole point is a box on the operator's own network, which no
/// allowlist could name in advance.
///
/// So the rule is about *reach* rather than identity: https to anywhere, or
/// plain http only to a private or link-local **literal address**. Userinfo is
/// refused outright, because a URL carrying a credential is a credential this
/// process would then hold and log.
///
/// A *name* over plain http is refused even when it currently resolves
/// privately, so `http://ha.lan:8123` has to be written
/// `http://10.42.4.7:8123`. That is a real cost and it is the deliberate
/// choice: this check runs when a setting is saved, a name that resolves
/// privately today can resolve publicly tomorrow, and the setting would not be
/// re-examined. The Developer row says exactly this when it refuses one.
///
/// Redirects are refused by the client that uses this (as the artwork client
/// does), so the host checked here is the host contacted.
pub(crate) fn approved_webhook_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| format!("`{raw}` is not a URL this server can parse."))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "A webhook URL must not carry a username or password — this server would then hold              and log a credential it has no use for."
                .to_owned(),
        );
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" => {
            let host = url
                .host_str()
                .ok_or_else(|| "That URL names no host.".to_owned())?;
            if private_webhook_host(host) {
                Ok(url)
            } else {
                Err(format!(
                    "`{host}` is not a private address, so plain http would put this event on                      the public internet in the clear. Use https, or a host on your own network."
                ))
            }
        }
        other => Err(format!(
            "`{other}` is not a scheme this server will call; use https, or http to a private              address."
        )),
    }
}

/// RFC 1918, loopback, link-local and unique-local, by literal address.
///
/// A *name* is deliberately not resolved here: a name that resolves privately
/// now can resolve publicly later, and this check runs when an operator saves
/// a setting rather than when the request goes out. A hostname is therefore
/// only accepted over https, where the transport protects the body regardless
/// of where the name points.
fn private_webhook_host(host: &str) -> bool {
    // `Url::host_str` keeps the brackets on an IPv6 literal, and `IpAddr`
    // does not parse them — so without this a LAN address like
    // `http://[fd00::1]:8123/` would be read as a hostname and refused.
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                // fc00::/7 unique-local and fe80::/10 link-local, neither of
                // which `std` exposes as a stable predicate.
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// Free bytes on the filesystem holding `path`, or `None` when the path is
/// empty or this node cannot see it.
///
/// A non-owner node answering `None` is the common case and not a fault: the
/// DVR root is a mount the owner writes to, and a node that does not have it
/// has nothing honest to say about its free space.
#[cfg(unix)]
pub(crate) fn free_space_bytes(path: &str) -> Option<u64> {
    if path.trim().is_empty() {
        return None;
    }
    let raw = std::ffi::CString::new(path).ok()?;
    // SAFETY: `statvfs` reads through the NUL-terminated path and writes only
    // into the zeroed struct below; both live for the duration of the call.
    let stats = unsafe {
        let mut stats = std::mem::zeroed::<libc::statvfs>();
        if libc::statvfs(raw.as_ptr(), &mut stats) != 0 {
            return None;
        }
        stats
    };
    Some((stats.f_bsize as u64).saturating_mul(stats.f_bavail as u64))
}

#[cfg(windows)]
pub(crate) fn free_space_bytes(path: &str) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt as _;

    let mut path = std::ffi::OsStr::new(path).encode_wide().collect::<Vec<_>>();
    if path.is_empty() || path.contains(&0) {
        return None;
    }
    path.push(0);
    let mut available = 0u64;
    // SAFETY: the path is NUL-terminated and `available` is writable for the
    // duration of the read-only filesystem query.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

fn remaining_guide_budget(deadline: tokio::time::Instant) -> Result<Duration, LiveTvError> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        Err(LiveTvError::DeviceUnavailable(
            "programme guide refresh exceeded its total deadline".to_owned(),
        ))
    } else {
        Ok(remaining)
    }
}

fn guide_parser_stopped(error: tokio::task::JoinError) -> LiveTvError {
    LiveTvError::DeviceUnavailable(format!("programme guide parser stopped: {error}"))
}

fn guide_refresh_cancelled() -> LiveTvError {
    LiveTvError::DeviceUnavailable(
        "programme guide settings changed while the refresh was running".to_owned(),
    )
}

async fn cancellable_guide_step<T, F>(
    cancel: &CancellationToken,
    future: F,
) -> Result<T, LiveTvError>
where
    F: std::future::Future<Output = Result<T, LiveTvError>>,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(guide_refresh_cancelled()),
        result = future => result,
    }
}

async fn await_guide_parser<T: Send + 'static>(
    mut parser: tokio::task::JoinHandle<Result<T, LiveTvError>>,
    cancel: &CancellationToken,
) -> Result<T, LiveTvError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // Dropping a blocking JoinHandle detaches rather than aborts it.
            // Return promptly; the permit clone inside the closure prevents a
            // replacement parser from starting until this stale one exits.
            drop(parser);
            Err(guide_refresh_cancelled())
        }
        result = &mut parser => result.map_err(guide_parser_stopped)?,
    }
}

#[derive(Clone)]
struct CachedGraphProbe {
    height: u16,
    encoder: Encoder,
    observed: tokio::time::Instant,
    result: (bool, String),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SourceFormatKey {
    generation: i64,
    device_id: String,
    channel_id: String,
}

#[derive(Clone)]
struct CachedSourceFormat {
    expires_at: tokio::time::Instant,
    format: LiveTvSourceFormat,
}

/// The complete probe result a channel's last tune produced — everything the
/// delivery planner reads — under the same key and the same expiry as its
/// `CachedSourceFormat` (20 minutes, clipped to the current programme's end,
/// where a mux most often changes format). A warm opener plans from it
/// without waiting for a probe, and publishes only once this tune's own probe
/// has re-proven the plan (plan L-02 §3.3).
#[derive(Clone)]
struct CachedSourceFacts {
    expires_at: tokio::time::Instant,
    facts: LiveSourceFacts,
}

/// How a viewer's session came by the facts it plans from, and so which
/// `plurx_live_tv_start_plan_total` series it counts under.
enum LiveStartKind {
    /// It opened the tuner and waited for the opening probe.
    Cold,
    /// It opened the tuner planning from cached facts; the opening probe
    /// verifies them before anything is published.
    Warm(Box<LiveSourceFacts>),
    /// It joined a transport already open and planned from that transport's
    /// facts.
    Joined,
}

/// `plurx_live_tv_start_plan_total{source}`, indexed as the label list.
#[derive(Clone, Copy)]
enum StartPlanSource {
    Cold = 0,
    Joined = 1,
    WarmAgreed = 2,
    WarmDisagreed = 3,
}

impl StartPlanSource {
    const LABELS: [&'static str; 4] = ["cold", "joined", "warm_agreed", "warm_disagreed"];
}

/// One node's latest validated reading of the replicated Live TV settings.
///
/// The sessions used to take this reading each, four times a second between
/// them per session; they now share this one. What is shared is the *reading*,
/// not the decision: every session still runs `validate_start_config` against
/// it on every fence tick, so owner, generation, enabled and drain admission
/// are checked exactly as often as before.
struct FenceObservation {
    config: LiveTvConfig,
    /// When the consistent read that produced this completed.
    observed_at: tokio::time::Instant,
}

/// Which Live TV path took a consistent settings read. Only the label of
/// `plurx_live_tv_settings_reads_total`; every site takes the same read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsReadSite {
    /// The node's `settings_observer_loop`, which feeds the session fence:
    /// 1 Hz while a tuner transport is held, parked otherwise.
    Observer = 0,
    /// The direct start and pre-publication fences (`ensure_session_fence`).
    Fence = 1,
    /// Every other caller of `LiveTvManager::config`: API handlers and the
    /// guide refresh paths.
    Request = 2,
}

impl SettingsReadSite {
    const ALL: [Self; 3] = [Self::Observer, Self::Fence, Self::Request];

    fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Fence => "fence",
            Self::Request => "request",
        }
    }
}

/// How a session's fence tick found the observation it validated against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FenceFreshness {
    /// Read within one interval plus one store timeout.
    Fresh,
    /// Older than that but inside `FENCE_GRACE_MAX_AGE`: the read is failing
    /// and the session is running on the last thing this node proved.
    Grace,
    /// Past the grace bound, or never read at all. The session ends.
    Expired,
}

struct FenceObserver {
    latest: tokio::sync::watch::Sender<Option<Arc<FenceObservation>>>,
    /// Fence ticks counted by state, so an operator can see what sessions
    /// actually validated against rather than only how many reads succeeded.
    metrics: Arc<LiveTvMetrics>,
}

impl FenceObserver {
    fn new(metrics: Arc<LiveTvMetrics>) -> Self {
        Self {
            latest: tokio::sync::watch::Sender::new(None),
            metrics,
        }
    }

    /// Publish a completed consistent read. Callers pass the config they just
    /// read; this records when they read it.
    fn publish(&self, config: LiveTvConfig) {
        self.latest.send_replace(Some(Arc::new(FenceObservation {
            config,
            observed_at: tokio::time::Instant::now(),
        })));
    }

    /// Publish a direct fence read only when nothing fresh is published: the
    /// observer is parked because nothing was live, or its reads are failing.
    /// A running observer stays the one publisher, so two reads that finish
    /// out of order cannot put an older reading over a newer one.
    fn publish_unless_fresh(&self, config: &LiveTvConfig) {
        let fresh = self.observed().is_some_and(|observation| {
            tokio::time::Instant::now().saturating_duration_since(observation.observed_at)
                <= FENCE_OBSERVATION_FRESH_MAX_AGE
        });
        if !fresh {
            self.publish(config.clone());
        }
    }

    fn observed(&self) -> Option<Arc<FenceObservation>> {
        self.latest.borrow().clone()
    }

    fn count(&self, state: FenceFreshness) {
        let index = match state {
            FenceFreshness::Fresh => 0,
            FenceFreshness::Grace => 1,
            FenceFreshness::Expired => 2,
        };
        self.metrics.fence_observations[index].fetch_add(1, Ordering::Relaxed);
    }

    /// The observation a session may validate against, or the reason it may
    /// not. Counts what it found either way.
    fn validated(&self) -> Result<Arc<FenceObservation>, LiveTvError> {
        let Some(observation) = self.observed() else {
            self.count(FenceFreshness::Expired);
            return Err(LiveTvError::OwnerUnavailable(
                "the replicated settings have not been read on this node; live TV stopped so a \
                 disabled or moved tuner cannot keep streaming"
                    .to_owned(),
            ));
        };
        let age = tokio::time::Instant::now().saturating_duration_since(observation.observed_at);
        if age <= FENCE_OBSERVATION_FRESH_MAX_AGE {
            self.count(FenceFreshness::Fresh);
            Ok(observation)
        } else if age <= FENCE_GRACE_MAX_AGE {
            self.count(FenceFreshness::Grace);
            Ok(observation)
        } else {
            self.count(FenceFreshness::Expired);
            Err(LiveTvError::OwnerUnavailable(format!(
                "the replicated settings could not be read for {}s; live TV stopped so a \
                 disabled or moved tuner cannot keep streaming",
                age.as_secs()
            )))
        }
    }
}

/// Live TV's view of serving authority.
///
/// Admissions and commits use the fence exactly (an admission takes its
/// generation from [`Self::admit`]; the ingress commit checks the fence
/// itself): a start, an ingress commit or a recording admitted under a
/// generation that has since moved on refuses itself, as before.
///
/// Work that is already running — a viewer's session, a shared tuner
/// transport, the recording sinks on it — is not an admission. It asks
/// [`Self::running`], which stays true through a loss of serving authority
/// that recovers within [`crate::serving_fence::SERVING_FENCE_SESSION_GRACE`]
/// (summed over one outage, the policy the rolling registry and the
/// progressive remux owner use), and it hands a client nothing unless
/// [`Self::is_ready`]: every playlist and segment request answers a retryable
/// `serving_fenced` while authority is lost. Before this, every check compared
/// the admission generation with the current one, and generations only rise,
/// so the second or two of lost quorum a leader restart costs every voter
/// ended every Live TV session, transport and recording on the owner.
///
/// Keeping running work through the grace leaves single ownership where it
/// already lived, in the replicated resource ledger: a replacement owner can
/// claim a capture or an ingest only after this owner's 30 s lease
/// (`live_tv_resource::LEASE_MS`, renewed every five seconds) has lapsed, so
/// at least twenty seconds of lease remain when the grace runs out.
///
/// One loop decides ([`Self::serving_fence_loop`]): when a loss outlasts the
/// grace it raises `running_floor` to the loss generation, and everything
/// admitted before that loss stops at its next check (25 ms for a session,
/// one tuner chunk for a transport). The loop is spawned once in `main.rs`;
/// the owner ledger pins that spawn, because without the loop running work
/// would outlive a sustained loss.
#[derive(Clone)]
pub(crate) struct LiveTvAuthority {
    serving: crate::serving_fence::ServingAuthority,
    /// The lowest admission generation whose running work may continue.
    running_floor: Arc<AtomicU64>,
}

impl LiveTvAuthority {
    pub(crate) fn new(serving: crate::serving_fence::ServingAuthority) -> Self {
        Self {
            serving,
            running_floor: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    pub(crate) fn always_ready() -> Self {
        Self::new(crate::serving_fence::ServingAuthority::always_ready())
    }

    /// The generation a new admission is made under, while authority is held.
    pub(crate) fn admit(&self) -> Option<u64> {
        self.serving.admit()
    }

    /// Serving authority is held right now.
    pub(crate) fn is_ready(&self) -> bool {
        self.serving.state().ready
    }

    /// Running work admitted under `admitted_generation` may continue. It may
    /// publish to a client only while [`Self::is_ready`] as well.
    pub(crate) fn running(&self, admitted_generation: u64) -> bool {
        admitted_generation >= self.running_floor.load(Ordering::Acquire)
    }

    /// Running and authoritative now: what a first publication requires.
    pub(crate) fn serving(&self, admitted_generation: u64) -> bool {
        self.running(admitted_generation) && self.is_ready()
    }

    /// End all running work admitted before `generation`. Monotonic.
    pub(crate) fn end_running_before(&self, generation: u64) -> bool {
        self.running_floor.fetch_max(generation, Ordering::AcqRel) < generation
    }

    /// Resolve every loss of serving authority through the shared session
    /// grace, and end running Live TV work only when a loss outlasts it.
    pub(crate) async fn serving_fence_loop(
        self,
        mut serving: tokio::sync::watch::Receiver<crate::serving_fence::ServingState>,
    ) {
        let mut grace = crate::serving_fence::SessionGrace::default();
        let mut resolved = serving.borrow_and_update().loss_generation;
        loop {
            let state = *serving.borrow_and_update();
            if state.authority_lost_since(resolved) {
                match grace.resolve_loss(&mut serving).await {
                    crate::serving_fence::LossOutcome::Recovered {
                        outage,
                        budget_spent,
                    } => {
                        resolved = serving.borrow_and_update().loss_generation;
                        tracing::info!(
                            outage_ms = u64::try_from(outage.as_millis()).unwrap_or(u64::MAX),
                            outage_budget_spent_ms =
                                u64::try_from(budget_spent.as_millis()).unwrap_or(u64::MAX),
                            loss_generation = resolved,
                            "serving authority returned within the session grace; running Live TV kept"
                        );
                    }
                    crate::serving_fence::LossOutcome::Expired => {
                        let generation = serving.borrow_and_update().loss_generation;
                        if self.end_running_before(generation) {
                            tracing::warn!(
                                loss_generation = generation,
                                grace_ms = u64::try_from(
                                    crate::serving_fence::SERVING_FENCE_SESSION_GRACE.as_millis()
                                )
                                .unwrap_or(u64::MAX),
                                "serving authority not regained within the session grace; running Live TV ended"
                            );
                        }
                        // `resolved` stays where it was. Until authority is
                        // seen back, every observation goes through
                        // `resolve_loss` again with a fresh budget, so work
                        // admitted in a ready window this loop never saw (a
                        // recovery and a new loss coalesced into one
                        // notification) is still ended within one grace.
                    }
                    crate::serving_fence::LossOutcome::Closed => {
                        self.end_running_before(u64::MAX);
                        return;
                    }
                }
                continue;
            }
            if serving.changed().await.is_err() {
                self.end_running_before(u64::MAX);
                return;
            }
        }
    }
}

pub(crate) struct LiveTvManager {
    store: Arc<dyn Store>,
    client: Result<reqwest::Client, String>,
    system: Arc<SystemInfo>,
    transcode: Arc<crate::transcode::TranscodeManager>,
    serving: LiveTvAuthority,
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
    /// Which day of the cached horizon the next refresh re-reads. A plain
    /// counter: cycling is the point, and which day it starts on is not.
    guide_revalidation_cursor: AtomicU64,
    graph_cache: tokio::sync::Mutex<Option<CachedGraphProbe>>,
    caption_proofs: StdMutex<Vec<caption_probe::CaptionProof>>,
    source_formats: StdMutex<HashMap<SourceFormatKey, CachedSourceFormat>>,
    source_facts: StdMutex<HashMap<SourceFormatKey, CachedSourceFacts>>,
    registry: Arc<StdMutex<LiveTvRegistry>>,
    /// Owner-sampled during the DVR loop. Public overview reads this atomic;
    /// they never stat the recording filesystem themselves.
    dvr_storage_free_bytes: AtomicU64,
    scratch_claims: StdMutex<HashSet<PathBuf>>,
    scratch_sweep_gate: tokio::sync::Mutex<()>,
    /// The one settings observation every live session on this node validates
    /// against, refreshed by `settings_observer_loop`.
    fence: FenceObserver,
    /// Wakes a parked `settings_observer_loop` when a transport opens.
    observer_wake: tokio::sync::Notify,
    /// What that loop parks on; taken once when it starts.
    settings_changes: StdMutex<Option<SettingsChangeSource>>,
    /// Sessions that left the registry with a child or a directory still to
    /// confirm gone (plan L-02 §3.4). Held only for bookkeeping: no holder
    /// waits on the filesystem, because each orphan's directory removal runs
    /// on its own task (`OrphanSession::removal`). A tokio mutex so a drain
    /// can bound its wait for it by the drain deadline.
    orphans: tokio::sync::Mutex<Vec<OrphanSession>>,
    incarnation: OwnerIncarnation,
    #[cfg(test)]
    fail_settings_reads: AtomicBool,
    /// Test seam: the kernel has not confirmed an FFmpeg exit. Cleanup then
    /// fails exactly as it does when `child.wait()` outlasts its deadline, and
    /// the orphan owner's `try_wait` sees the child as still running.
    #[cfg(test)]
    withhold_reaps: AtomicBool,
    /// Test seam: every scratch inventory scan started from now on hangs, as
    /// a `stat` on a stalled mount does.
    #[cfg(test)]
    hang_scratch_scans: AtomicBool,
    /// Test seam: a warm start that disagreed lingers after reaping its
    /// FFmpeg, as a slow reap on a loaded host does, until the tuner's
    /// fan-out has seen that FFmpeg's feed closed and kept going.
    #[cfg(test)]
    linger_after_warm_reap: AtomicBool,
    #[cfg(test)]
    scratch_scans_started: AtomicU64,
    /// Test seam: while `true`, every session ledger read started waits for
    /// `false` before it reads, as a consistent read held across a slow
    /// leader change does.
    #[cfg(test)]
    hold_session_fence_reads: tokio::sync::watch::Sender<bool>,
    #[cfg(test)]
    session_fence_reads_started: AtomicU64,
    /// Test seam: every orphan directory removal started from now on hangs,
    /// as `remove_dir_all` on a stalled mount does.
    #[cfg(test)]
    hang_orphan_removals: AtomicBool,
    metrics: Arc<LiveTvMetrics>,
}

impl LiveTvManager {
    /// Live TV's serving authority, for the loop that resolves its losses.
    pub(crate) fn authority(&self) -> LiveTvAuthority {
        self.serving.clone()
    }

    pub(crate) fn new(
        store: Arc<dyn Store>,
        system: Arc<SystemInfo>,
        transcode: Arc<crate::transcode::TranscodeManager>,
        serving: crate::serving_fence::ServingAuthority,
        node_id: String,
        scratch_root: PathBuf,
        guide_store: PathBuf,
    ) -> Arc<Self> {
        let metrics = Arc::new(LiveTvMetrics::default());
        let incarnation = OwnerIncarnation::adopt(Some(guide_store.join(INCARNATION_FILE)));
        let guide_cache = GuideCache::with_store(guide_store);
        let lineup_wake = Arc::clone(&guide_cache.wake);
        let manager = Arc::new(Self {
            store,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(|error| error.to_string()),
            system,
            transcode,
            serving: LiveTvAuthority::new(serving),
            node_id,
            scratch_root,
            cache: SnapshotCache::with_wake(lineup_wake),
            guide_cache,
            guide_titles: StdMutex::new(Arc::new(BTreeMap::new())),
            relayed_guide: tokio::sync::Mutex::new(None),
            guide_revalidation_cursor: AtomicU64::new(0),
            graph_cache: tokio::sync::Mutex::new(None),
            caption_proofs: StdMutex::new(Vec::new()),
            source_formats: StdMutex::new(HashMap::new()),
            source_facts: StdMutex::new(HashMap::new()),
            registry: Arc::clone(&metrics.registry),
            dvr_storage_free_bytes: AtomicU64::new(u64::MAX),
            scratch_claims: StdMutex::new(HashSet::new()),
            scratch_sweep_gate: tokio::sync::Mutex::new(()),
            fence: FenceObserver::new(Arc::clone(&metrics)),
            observer_wake: tokio::sync::Notify::new(),
            settings_changes: StdMutex::new(Some(SettingsChangeSource::LocalOnly)),
            orphans: tokio::sync::Mutex::new(Vec::new()),
            incarnation,
            #[cfg(test)]
            fail_settings_reads: AtomicBool::new(false),
            #[cfg(test)]
            withhold_reaps: AtomicBool::new(false),
            #[cfg(test)]
            hang_scratch_scans: AtomicBool::new(false),
            #[cfg(test)]
            linger_after_warm_reap: AtomicBool::new(false),
            #[cfg(test)]
            scratch_scans_started: AtomicU64::new(0),
            #[cfg(test)]
            hold_session_fence_reads: tokio::sync::watch::channel(false).0,
            #[cfg(test)]
            session_fence_reads_started: AtomicU64::new(0),
            #[cfg(test)]
            hang_orphan_removals: AtomicBool::new(false),
            metrics,
        });
        manager.adopt_persisted_guide();
        manager
    }

    /// Probe this node's exact FFmpeg build in the background. Until a graph
    /// has completed its end-to-end proof, sessions publish no caption group.
    pub(crate) fn start_caption_probe(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let system = Arc::clone(&manager.system);
            match tokio::spawn(caption_probe::probe_available_graphs(system)).await {
                Ok(proofs) => {
                    tracing::info!(graphs = proofs.len(), "caption graph probe complete");
                    *manager
                        .caption_proofs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = proofs;
                }
                Err(error) => tracing::warn!(%error, "caption graph probe unavailable"),
            }
        });
    }

    /// Make a guide loaded from disk visible to everything that reads the
    /// in-memory one: the age gauge, and the activity path's title index. The
    /// cache itself already holds it — this only publishes the projections.
    ///
    /// The generation is deliberately not checked here: settings are not
    /// readable synchronously at construction, and `read()` filters on it
    /// anyway, so a guide matched under an older generation is never *served*.
    /// The bounded consequence is that the Activity page can label programmes
    /// from it until the refresh loop's first tick invalidates, which is
    /// seconds away — against the certainty of an empty grid if adoption
    /// waited for that tick.
    fn adopt_persisted_guide(&self) {
        let Ok(state) = self.guide_cache.state.try_lock() else {
            return;
        };
        let Some(cached) = state.cached.as_ref() else {
            return;
        };
        self.metrics.observe_guide_at(
            cached.observed,
            cached.age_offset,
            cached.guide.total_programmes(),
        );
        self.publish_guide_titles(&cached.guide);
    }

    /// Where the owner's guide copy lives, how big it is, and the generation it
    /// was matched under — for the Developer panel, which never gates on it.
    pub(crate) fn guide_store_status(&self) -> Option<(PathBuf, u64, Option<i64>)> {
        self.guide_cache.persisted_status()
    }

    pub(crate) async fn config(&self) -> Result<LiveTvConfig, LiveTvError> {
        self.config_read_at(SettingsReadSite::Request).await
    }

    /// One consistent read of the replicated settings, counted under `site`.
    async fn config_read_at(&self, site: SettingsReadSite) -> Result<LiveTvConfig, LiveTvError> {
        let read = self.settings_snapshot().await;
        self.metrics.observe_settings_read(site, read.is_ok());
        let snapshot = read.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        let config = LiveTvConfig::from_snapshot(&snapshot, &self.node_id);
        self.observe_config(&config);
        Ok(config)
    }

    async fn settings_snapshot(
        &self,
    ) -> Result<BTreeMap<String, String>, plurx_core::error::StoreError> {
        #[cfg(test)]
        if self.fail_settings_reads.load(Ordering::SeqCst) {
            return Err(plurx_core::error::StoreError::Database(
                "injected replicated-settings read failure".to_owned(),
            ));
        }
        self.store.settings_snapshot().await
    }

    /// Make every settings read on this manager fail until cleared, the way a
    /// leader failover does, so the fence's failure arm is driven by a real
    /// failed read rather than by the absence of one.
    #[cfg(test)]
    fn test_fail_settings_reads(&self, fail: bool) {
        self.fail_settings_reads.store(fail, Ordering::SeqCst);
    }

    /// Take one consistent observation of the replicated settings and publish
    /// it for every live session on this node.
    ///
    /// A failure keeps the previous observation: sessions run on it for
    /// `FENCE_GRACE_MAX_AGE` and then end with a message naming the settings
    /// store. Nothing here decides that — `FenceObserver::validated` does, on
    /// the age of what this last managed to publish.
    pub(crate) async fn observe_fence(&self) -> bool {
        match self.config_read_at(SettingsReadSite::Observer).await {
            Ok(config) => {
                self.fence.publish(config);
                true
            }
            Err(error) => {
                // Nothing is published: the previous observation stays, and
                // sessions keep validating against it until its age passes
                // `FENCE_GRACE_MAX_AGE`. It is that age, not a count of
                // failures, that ends them — a read that fails once and
                // succeeds on the next tick has cost nothing.
                tracing::debug!(
                    kind = %error.code(),
                    "Live TV settings observation failed; sessions continue on the last one"
                );
                false
            }
        }
    }

    pub(crate) fn observe_config(&self, config: &LiveTvConfig) {
        self.guide_cache.cancel_if_config_changed(config);
        self.guide_cache.observe_generation(config.generation);
        self.source_formats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, _| key.generation == config.generation);
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
        let snapshot = self.with_source_formats(snapshot, config.generation);
        Ok(self
            .with_graph_probe(snapshot, config, force, probe_graph)
            .await)
    }

    fn with_source_formats(&self, mut snapshot: LiveTvSnapshot, generation: i64) -> LiveTvSnapshot {
        let now = tokio::time::Instant::now();
        let device_id = snapshot.device.device_id.clone();
        let channel_ids = snapshot
            .channels
            .iter()
            .map(|channel| channel.id.as_str())
            .collect::<HashSet<_>>();
        let mut cache = self
            .source_formats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|key, value| {
            key.generation == generation
                && key.device_id == device_id
                && channel_ids.contains(key.channel_id.as_str())
                && value.expires_at > now
        });
        self.source_facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, value| {
                key.generation == generation
                    && key.device_id == device_id
                    && channel_ids.contains(key.channel_id.as_str())
                    && value.expires_at > now
            });
        for channel in &mut snapshot.channels {
            let key = SourceFormatKey {
                generation,
                device_id: device_id.clone(),
                channel_id: channel.id.clone(),
            };
            if let Some(cached) = cache.get(&key) {
                channel.source_format = Some(cached.format.clone());
            }
        }
        snapshot
    }

    fn record_source_format(
        &self,
        key: SourceFormatKey,
        guide_number: &str,
        format: LiveTvSourceFormat,
    ) -> i64 {
        let expires_at = self.source_format_expiry(guide_number, format.observed_at);
        let now_wall = unix_seconds();
        let ttl = Duration::from_secs(expires_at.saturating_sub(now_wall) as u64);
        self.source_formats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                CachedSourceFormat {
                    expires_at: tokio::time::Instant::now() + ttl,
                    format,
                },
            );
        expires_at
    }

    /// Remember the facts a session planned from, for the next opener of this
    /// channel. Same expiry rule as the UI's source format.
    fn record_source_facts(
        &self,
        key: SourceFormatKey,
        guide_number: &str,
        facts: LiveSourceFacts,
    ) {
        let now_wall = unix_seconds();
        let expires_at = self.source_format_expiry(guide_number, now_wall);
        let ttl = Duration::from_secs(expires_at.saturating_sub(now_wall).max(0) as u64);
        self.source_facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                CachedSourceFacts {
                    expires_at: tokio::time::Instant::now() + ttl,
                    facts,
                },
            );
    }

    /// Unexpired cached facts for exactly this generation, device and channel.
    fn warm_source_facts(&self, key: &SourceFormatKey) -> Option<LiveSourceFacts> {
        let now = tokio::time::Instant::now();
        self.source_facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .filter(|cached| cached.expires_at > now)
            .map(|cached| cached.facts.clone())
    }

    /// The cached facts no longer describe the mux: a warm start disagreed,
    /// or a running session saw the format change.
    fn forget_source_facts(&self, key: &SourceFormatKey) {
        self.source_facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }

    fn source_format_expiry(&self, guide_number: &str, observed_at: i64) -> i64 {
        let absolute_ceiling = observed_at.saturating_add(SOURCE_FORMAT_TTL.as_secs() as i64);
        let now_wall = unix_seconds();
        self.guide_titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(guide_number)
            .and_then(|rows| {
                rows.iter()
                    .find(|(start, end, _)| *start <= now_wall && *end > now_wall)
                    .map(|(_, end, _)| *end)
            })
            .map_or(absolute_ceiling, |programme_end| {
                absolute_ceiling.min(programme_end)
            })
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
                "Run the Live TV readiness check to test the live-TV FFmpeg graph".to_owned(),
            // 3 = client-supplied request ids and the /live-tv/starts/*
            // recovery routes. An ingress intersects this with its own list.
            start_protocols: vec![1, 2, 3, 5],
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
                "FFmpeg is not configured on the session server".to_owned(),
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
        let result = run_graph_probe(encoder, &self.system, height, &probe_dir).await;
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
        let result = self.start_local_inner(request.clone()).await;
        if let Ok(response) = &result {
            self.resource_advance(
                &request,
                plurx_core::live_tv_resource::StartPhase::Active,
                serde_json::to_string(response).ok(),
            )
            .await?;
        }
        // A refused replay is not authority to fail the original operation.
        // Spawned workers close their own claim; reservations that never
        // spawned expire through the ledger's bounded pending-start window.
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
                "the start request names a different session server".to_owned(),
            ));
        }
        let serving_generation = self.serving.admit().ok_or_else(|| {
            LiveTvError::OwnerUnavailable(crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned())
        })?;
        let config = self.config().await?;
        validate_start_config(&config, &request, &self.node_id)?;

        let key = LiveTvRequestKey::from(&request);
        if let Some(session) = self.session_for_request(&key, &request)? {
            self.resource_session_fence(&session).await?;
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
        let device_id = snapshot.device.device_id.clone();
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
        // The transport builds the same URL; refusing an unusable one here
        // keeps that refusal ahead of any tuner reservation.
        let path = format!("/auto/v{}", channel.guide_number);
        pinned_url(address, 5004, &path)?;
        self.resource_start(
            &request,
            &device_id,
            config.max_sessions.min(snapshot.device.tuner_count),
        )
        .await?;

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
        let source_format = Arc::new(StdMutex::new(channel.source_format.clone()));
        let source_format_expires_at = Arc::new(AtomicI64::new(
            channel.source_format.as_ref().map_or(0, |format| {
                self.source_format_expiry(&channel.guide_number, format.observed_at)
            }),
        ));
        let session = Arc::new(LiveTvSession {
            capability: capability.clone(),
            activation_token,
            request: request.clone(),
            channel,
            device_id,
            output: StdMutex::new(output),
            delivery: StdMutex::new(None),
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
            source_format_changed: Arc::new(AtomicBool::new(false)),
            encoder_diagnostic: Arc::new(StdMutex::new(None)),
            source_format,
            source_format_expires_at,
            tuner_bytes: Arc::new(AtomicU64::new(0)),
            codec_qualification_counted: AtomicBool::new(false),
            resource_admission: Arc::new(tokio::sync::Semaphore::new(LOCAL_RESOURCE_CONCURRENCY)),
            signal_cache: tokio::sync::Mutex::new(None),
            state: StdMutex::new(LiveTvSessionState {
                phase: LiveTvSessionPhase::Starting,
                startup: None,
                activated: false,
                provisional_at: None,
                last_touch: now,
                last_progress: now,
                newest_listed: None,
                media_sequence: 0,
                publication: None,
                encoder: "pending".to_owned(),
                error: None,
                terminal_error: None,
                cleanup: None,
            }),
            transport: StdMutex::new(None),
            seat_reserved: AtomicBool::new(false),
            consumer: StdMutex::new(None),
        });

        let client = self
            .client
            .as_ref()
            .map_err(|_| {
                LiveTvError::DeviceUnavailable("the HDHomeRun HTTP client is unavailable".into())
            })?
            .clone();
        let device_id = session.device_id.clone();
        let seat = TransportSeat {
            channel_id: &request.channel_id,
            device_id: &device_id,
            address,
            running_floor: self.serving.running_floor.load(Ordering::Acquire),
        };
        // Plan L-02 §3.3: facts this owner probed on this channel, under this
        // configuration and device, still inside their expiry, from which
        // this request's plan resolves. Used only if this start opens the
        // tuner; a joiner plans from its transport's facts.
        let warm_facts = self
            .warm_source_facts(&SourceFormatKey {
                generation: config.generation,
                device_id: device_id.clone(),
                channel_id: request.channel_id.clone(),
            })
            .filter(|facts| {
                crate::live_tv_delivery::resolve_live_delivery(
                    facts,
                    request.playback.as_ref(),
                    &live_quality_policy(&config, request.playback.is_some()),
                    &live_execution_support(&self.system),
                )
                .is_ok()
            });
        // One decision per registry lock hold, and at most one retry after
        // waiting for a transport to close — never a wait while holding the
        // lock, and never a second transport for one channel.
        let mut waited = false;
        let (raced_session, opened) = loop {
            let decision = {
                let mut registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if registry.closing {
                    return Err(LiveTvError::Conflict(
                        "the live-TV start was refused: this node is shutting down".into(),
                    ));
                }
                // A concurrent identical request may have won while the lineup
                // refresh was in flight. Recover it without consuming capacity.
                if let Some(existing) = registry.request_session(&request)? {
                    break (Some(existing), None);
                }
                if !registry.recovery_history_admits() {
                    return Err(LiveTvError::Capacity(
                        "live-TV request recovery history is full; retry after one minute".into(),
                    ));
                }
                // Slots are transports: recordings hold tuners too, so a
                // viewer refused because a capture has the last tuner is owed
                // the real reason. The reservation happens in this same lock
                // hold (plan L-03 §2.4 D2).
                match registry.viewer_admission(&seat, request.user_id, now, config.max_sessions) {
                    ViewerAdmission::Join(transport) => {
                        if transport.reserve_seat() {
                            session.hold_seat_on(transport);
                            registry.requests.insert(key.clone(), capability.clone());
                            registry
                                .sessions
                                .insert(capability.clone(), Arc::clone(&session));
                            break (None, None);
                        }
                        // Retired between the check and the reservation.
                        ViewerAdmission::WaitFor(transport)
                    }
                    ViewerAdmission::Open => {
                        let transport = dvr::DvrTransport::new(dvr::TransportInit {
                            channel: session.channel.clone(),
                            generation: request.config_generation,
                            owner_serving_generation: serving_generation,
                            device_id: device_id.clone(),
                            address,
                            origin: dvr::TransportOrigin::Viewer,
                            scratch: self.transport_scratch(),
                            metrics: Arc::clone(&self.metrics),
                            seats: 1,
                        });
                        if warm_facts.is_some() {
                            transport.open_warm(&capability);
                        }
                        registry
                            .transports
                            .insert(request.channel_id.clone(), Arc::clone(&transport));
                        self.wake_settings_observer();
                        session.hold_seat_on(Arc::clone(&transport));
                        registry.requests.insert(key.clone(), capability.clone());
                        registry
                            .sessions
                            .insert(capability.clone(), Arc::clone(&session));
                        break (None, Some(transport));
                    }
                    other => other,
                }
            };
            let leaving = match decision {
                ViewerAdmission::Refuse(error) => return Err(error),
                ViewerAdmission::WaitFor(transport) => transport,
                ViewerAdmission::EvictStray(stray, transport) => {
                    stray.cancel.cancel();
                    self.metrics.observe_stray_eviction();
                    transport
                }
                ViewerAdmission::Join(_) | ViewerAdmission::Open => {
                    unreachable!("admissions that reserve break out of the loop above")
                }
            };
            if waited {
                return Err(LiveTvError::Capacity(
                    "a tuner connection on its way out did not close in time; try again".into(),
                ));
            }
            waited = true;
            // The close joins the worker that owns the tuner response, so
            // when this returns the device has the tuner back (§2.4 D4, D8).
            let budget =
                (tokio::time::Instant::now() + SESSION_DRAIN_TIMEOUT + Duration::from_secs(1))
                    .min(started + STARTUP_TIMEOUT);
            let _ = tokio::time::timeout_at(budget, leaving.closed()).await;
        };
        if let Some(existing) = raced_session {
            if !same_start(&existing.request, &request) {
                return Err(LiveTvError::Conflict(
                    "the live-TV request id was replayed with different fields".into(),
                ));
            }
            self.metrics
                .starts_recovered
                .fetch_add(1, Ordering::Relaxed);
            return wait_for_startup(existing, true).await;
        }
        let kind = match (opened.as_ref(), warm_facts) {
            (Some(_), Some(facts)) => LiveStartKind::Warm(Box::new(facts)),
            (Some(_), _) => LiveStartKind::Cold,
            (None, _) => LiveStartKind::Joined,
        };
        if let Some(transport) = opened {
            self.spawn_transport_worker(&transport, client);
        }

        let manager = Arc::downgrade(self);
        let worker_session = Arc::clone(&session);
        let worker = tokio::spawn(async move {
            run_live_session(manager, worker_session, config, kind).await;
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
        _source_node_id: &str,
    ) -> Result<LiveTvActivated, LiveTvError> {
        if request.expected_owner_node_id != self.node_id {
            return Err(LiveTvError::OwnerUnavailable(
                "the activation request names a different session server".into(),
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
        self.resource_session_fence(&session).await?;
        let config = self.config().await?;
        validate_start_config(&config, &session.request, &self.node_id)?;
        if !self.serving.running(session.owner_serving_generation) {
            session.cancel.cancel();
            return Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            ));
        }
        if !self.serving.is_ready() {
            // Inside the session grace: the session stays, the client retries.
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
            playlist_url: format!(
                "/api/v1/live-tv/sessions/{}/master.m3u8",
                session.capability
            ),
            channel: session.channel_with_source_format(),
            output: session.output(),
            delivery: session.delivery(),
            live: true,
        })
    }

    pub(crate) async fn resource_local(
        &self,
        request: LiveTvResourceRequest,
    ) -> Result<Response<Body>, LiveTvError> {
        validate_capability_owner(request.capability(), &self.node_id)?;
        let session = self.session(request.capability())?;
        // No Store read here (plan L-03): the observer's polled ledger verdict
        // ends a refused session, and the checks below see that end.
        self.resource_serving_fence(&session)?;
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
            LiveTvResourceRequest::Segment { sequence, .. }
            | LiveTvResourceRequest::Fragment { sequence, .. } => {
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
                let content_type = if session
                    .delivery()
                    .is_some_and(|delivery| delivery.packaging == LivePackaging::Fmp4)
                {
                    "video/mp4"
                } else {
                    "video/mp2t"
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, content_type)
                    .header(header::CACHE_CONTROL, "no-store")
                    .header(header::CONTENT_LENGTH, expected_len)
                    .body(body)
                    .map_err(|error| LiveTvError::StreamFailed(error.to_string()))
            }
            LiveTvResourceRequest::Init { .. } => {
                let permit = Arc::clone(&session.resource_admission)
                    .try_acquire_owned()
                    .map_err(|_| {
                        LiveTvError::StreamFailed(
                            "too many concurrent live-TV media responses; retry shortly".into(),
                        )
                    })?;
                let (name, expected_len) = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .publication
                    .as_ref()
                    .and_then(|publication| publication.init.clone())
                    .ok_or_else(|| {
                        LiveTvError::CapabilityExpired(
                            "the live-TV init segment is not published".into(),
                        )
                    })?;
                let file =
                    open_bounded_segment(&session.directory.join(&name), expected_len).await?;
                let body = bounded_segment_body(file, expected_len, session.cancel.clone(), permit);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "video/mp4")
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
        self.cancel_and_wait(vec![session], OrphanScope::Sessions)
            .await
            .map(|_| ())
    }

    pub(crate) async fn shutdown(&self) -> Result<usize, LiveTvError> {
        self.close_all_transports().await;
        let sessions = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.closing = true;
            registry.sessions.values().cloned().collect::<Vec<_>>()
        };
        let drained = self.cancel_and_wait(sessions, OrphanScope::All).await;
        // A clean stop is the last moment this process is seen. A capability
        // presented to the next process soon after is explained as a restart.
        self.incarnation.write();
        drained
    }

    async fn cancel_and_wait(
        &self,
        sessions: Vec<Arc<LiveTvSession>>,
        orphans: OrphanScope,
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
        let capabilities = sessions
            .iter()
            .map(|session| session.capability.clone())
            .collect::<HashSet<_>>();
        for session in sessions {
            loop {
                let notified = session.changed.notified();
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
        // A session whose cleanup failed has left the registry, but what it
        // could not release is the orphan owner's now. A drain acknowledges
        // disappearance, so it waits for that too, under the same deadline —
        // exactly as it used to wait for the un-retired session itself.
        self.drain_orphans(
            |orphan| {
                capabilities.contains(&orphan.capability)
                    || match orphans {
                        OrphanScope::Sessions => false,
                        OrphanScope::All => true,
                    }
            },
            deadline,
        )
        .await?;
        Ok(count)
    }

    #[cfg(test)]
    fn reap_withheld(&self) -> bool {
        self.withhold_reaps.load(Ordering::Acquire)
    }

    #[cfg(not(test))]
    fn reap_withheld(&self) -> bool {
        false
    }

    /// Take over what a session's failed cleanup left: its process, if the
    /// child's exit was not confirmed, and its directory. Claimed for the
    /// sweeper before the session leaves the registry.
    async fn adopt_orphan(&self, session: &LiveTvSession) {
        let process = session.process.lock().await.take();
        self.scratch_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.directory.clone());
        let mut orphans = self.orphans.lock().await;
        orphans.push(OrphanSession {
            capability: session.capability.clone(),
            directory: session.directory.clone(),
            process,
            removal: None,
            since: tokio::time::Instant::now(),
            attempts: 0,
        });
        self.bound_orphans(&mut orphans);
        self.metrics.set_orphans(&orphans);
    }

    /// One retry of one orphan. True when nothing is left to confirm.
    ///
    /// Never waits on the filesystem: the directory is removed on its own
    /// task, and a later retry reads the result. The caller holds the orphan
    /// list, and every drain, stop, shutdown and cleanup handoff needs that
    /// list, so one stalled removal must not hold it (PR #496 review 2).
    fn retry_orphan(&self, orphan: &mut OrphanSession) -> bool {
        orphan.attempts = orphan.attempts.saturating_add(1);
        if let Some(process) = orphan.process.as_mut() {
            if !self.reap_withheld() {
                let _ = process.child.start_kill();
                match process.child.try_wait() {
                    Ok(Some(_)) => {
                        if let Some(stderr) = process.stderr.take() {
                            stderr.abort();
                        }
                        if let Some(sink) = process.sink.take() {
                            orphan.removal =
                                Some(self.spawn_orphan_removal(&orphan.directory, Some(sink)));
                        }
                        // Drops the job and the encoder admission with it.
                        orphan.process = None;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(kind = %error.kind(), "checking an orphaned Live TV FFmpeg");
                    }
                }
            }
        }
        if orphan.process.is_some() {
            return false;
        }
        if let Some(removal) = orphan.removal.as_ref() {
            if !removal.is_finished() {
                return false;
            }
            let finished = orphan
                .removal
                .take()
                .and_then(futures_util::FutureExt::now_or_never);
            match finished {
                Some(Ok(Ok(()))) => return true,
                Some(Ok(Err(error))) if error.kind() == io::ErrorKind::NotFound => return true,
                Some(Ok(Err(error))) => {
                    if orphan.attempts.is_power_of_two() {
                        tracing::warn!(
                            kind = %error.kind(),
                            attempts = orphan.attempts,
                            age_s = orphan.since.elapsed().as_secs(),
                            "an orphaned Live TV scratch directory still cannot be removed"
                        );
                    }
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "an orphaned Live TV scratch removal task failed");
                }
                None => {}
            }
        }
        orphan.removal = Some(self.spawn_orphan_removal(&orphan.directory, None));
        false
    }

    /// Start one removal of an orphan's directory on its own task.
    fn spawn_orphan_removal(
        &self,
        directory: &Path,
        sink: Option<crate::scratch_put::PutSink>,
    ) -> OrphanRemoval {
        #[cfg(test)]
        if self.hang_orphan_removals.load(Ordering::Acquire) {
            return tokio::spawn(std::future::pending());
        }
        let directory = directory.to_owned();
        tokio::spawn(async move {
            if let Some(sink) = sink {
                sink.drain().await;
            }
            if crate::playback_cache::is_pinned(&directory) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "live-TV response still holds cache media",
                ));
            }
            tokio::fs::remove_dir_all(&directory).await
        })
    }

    /// Release the sweeper claim of every orphan that is finished with.
    fn release_orphan_claims<'a>(&self, done: impl IntoIterator<Item = &'a OrphanSession>) {
        let mut claims = self
            .scratch_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for orphan in done {
            claims.remove(&orphan.directory);
        }
    }

    /// Keep at most `ORPHAN_MAX`, dropping the oldest scratch-only orphans.
    fn bound_orphans(&self, orphans: &mut Vec<OrphanSession>) {
        while orphans.len() > ORPHAN_MAX {
            let Some(oldest) = orphans
                .iter()
                .enumerate()
                .filter(|(_, orphan)| orphan.process.is_none())
                .min_by_key(|(_, orphan)| orphan.since)
                .map(|(index, _)| index)
            else {
                break;
            };
            let dropped = orphans.remove(oldest);
            tracing::error!(
                attempts = dropped.attempts,
                "dropping the oldest orphaned Live TV scratch directory; the hourly sweep still sees it"
            );
            self.release_orphan_claims([&dropped]);
        }
    }

    /// The orphan owner's pass: retry every orphan once.
    async fn retry_orphans(&self) {
        let mut orphans = self.orphans.lock().await;
        if orphans.is_empty() {
            return;
        }
        let mut kept = Vec::with_capacity(orphans.len());
        for mut orphan in orphans.drain(..) {
            if self.retry_orphan(&mut orphan) {
                self.release_orphan_claims([&orphan]);
            } else {
                kept.push(orphan);
            }
        }
        *orphans = kept;
        self.bound_orphans(&mut orphans);
        self.metrics.set_orphans(&orphans);
    }

    /// Retry the orphans `wanted` selects until none is left or `deadline`
    /// passes. Other orphans are left to the retry loop. Bounded by
    /// `deadline` throughout, including the wait for the orphan list: a
    /// removal stalled on a hung mount keeps its orphan here until the
    /// deadline, and holds up nothing else.
    async fn drain_orphans(
        &self,
        wanted: impl Fn(&OrphanSession) -> bool,
        deadline: tokio::time::Instant,
    ) -> Result<(), LiveTvError> {
        let overdue = || {
            LiveTvError::StreamFailed(
                "live-TV cleanup was not confirmed before the drain deadline".into(),
            )
        };
        loop {
            {
                let mut orphans = tokio::time::timeout_at(deadline, self.orphans.lock())
                    .await
                    .map_err(|_| overdue())?;
                let mut kept = Vec::with_capacity(orphans.len());
                let mut remaining = 0usize;
                for mut orphan in orphans.drain(..) {
                    if !wanted(&orphan) {
                        kept.push(orphan);
                    } else if self.retry_orphan(&mut orphan) {
                        self.release_orphan_claims([&orphan]);
                    } else {
                        remaining += 1;
                        kept.push(orphan);
                    }
                }
                *orphans = kept;
                self.metrics.set_orphans(&orphans);
                if remaining == 0 {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(overdue());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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
        let terminal = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal_error
            .clone();
        // A session with no terminal error was released, not failed — a stop,
        // a drain, a shutdown, an idle reap. Counting that under
        // `capability_expired` would bury every real failure in it.
        let reason = terminal.as_ref().map_or("released", LiveTvError::code);
        let error = terminal.unwrap_or_else(|| {
            LiveTvError::CapabilityExpired(
                "the live-TV session ended; start a new Watch request".into(),
            )
        });
        // Every session end is on the box. `ps auxwww` is not an acceptable
        // answer for work that held a tuner: what it was, why it stopped, how
        // long it ran, and how much the tuner actually delivered. No
        // capability, no tuner URL, no DeviceAuth — the same rule the guide
        // fetch follows.
        tracing::info!(
            channel = %session.channel.guide_number,
            reason,
            cause = %sanitize_error(&error.to_string()),
            duration_s = session.started.elapsed().as_secs(),
            tuner_bytes = session.tuner_bytes.load(Ordering::Acquire),
            user = session.request.user_id,
            "Live TV session ended"
        );
        self.metrics.observe_session_end(reason);
        registry.terminals.insert(
            session.capability.clone(),
            LiveTvTerminalTombstone {
                error,
                request: session.request.clone(),
                expires_at: tokio::time::Instant::now() + TERMINAL_TOMBSTONE_TTL,
            },
        );
        session.changed.notify_waiters();
    }

    /// Stop whatever this viewer's request produced, and make sure nothing can
    /// be produced from it later. The fence goes in **before** anything is
    /// looked at, so a start racing this call finds `Conflict` or a session
    /// that is about to be cancelled — never a fresh admission.
    pub(crate) async fn retire_local(&self, user_id: i64, request_id: &str) -> LiveTvRetireOutcome {
        let (live, ended) = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retire(user_id, request_id.to_owned(), tokio::time::Instant::now());
            let live = registry.sessions_for_public_request(user_id, request_id);
            let ended = registry
                .tombstone_for_public_request(user_id, request_id)
                .is_some();
            (live, ended)
        };
        if !live.is_empty() {
            let _ = self.cancel_and_wait(live, OrphanScope::Sessions).await;
            return LiveTvRetireOutcome::Stopped;
        }
        if ended {
            return LiveTvRetireOutcome::Ended;
        }
        LiveTvRetireOutcome::Retired
    }

    /// The same document the original activation answered, for a session that
    /// is still live and still this viewer's. One public identity, one stream:
    /// every other match is cancelled.
    pub(crate) async fn resume_local(&self, user_id: i64, request_id: &str) -> LiveTvResumeAnswer {
        let (resumed, pending, ended, strays) = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.prune_terminals();
            if registry.is_retired(user_id, request_id) {
                return LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Retired);
            }
            let matches = registry.sessions_for_public_request(user_id, request_id);
            let resumable = |session: &Arc<LiveTvSession>| {
                if session.cancel.is_cancelled() {
                    return None;
                }
                let state = session
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (state.activated && state.phase == LiveTvSessionPhase::Active)
                    .then_some(state.last_touch)
            };
            let resumed = matches
                .iter()
                .filter_map(|session| resumable(session).map(|touch| (touch, Arc::clone(session))))
                .max_by_key(|(touch, _)| *touch)
                .map(|(_, session)| session);
            let pending = resumed.is_none()
                && matches.iter().any(|session| {
                    !session.cancel.is_cancelled()
                        && matches!(
                            session
                                .state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .phase,
                            LiveTvSessionPhase::Starting | LiveTvSessionPhase::Provisional
                        )
                });
            let strays = match resumed.as_ref() {
                Some(kept) => matches
                    .iter()
                    .filter(|session| !Arc::ptr_eq(session, kept))
                    .cloned()
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            let ended = registry
                .tombstone_for_public_request(user_id, request_id)
                .is_some();
            (resumed, pending, ended, strays)
        };
        if !strays.is_empty() {
            let _ = self.cancel_and_wait(strays, OrphanScope::Sessions).await;
        }
        if let Some(session) = resumed {
            if self.resource_session_fence(&session).await.is_err() {
                return LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Pending);
            }
            session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last_touch = tokio::time::Instant::now();
            return LiveTvResumeAnswer {
                outcome: LiveTvResumeOutcome::Live,
                session: Some(LiveTvActivated {
                    session_id: session.capability.clone(),
                    playlist_url: format!(
                        "/api/v1/live-tv/sessions/{}/master.m3u8",
                        session.capability
                    ),
                    channel: session.channel_with_source_format(),
                    output: session.output(),
                    delivery: session.delivery(),
                    live: true,
                }),
            };
        }
        if pending {
            // An activation is in flight from an ingress. The client keeps its
            // hint, shows the list and touches nothing: that ingress, or the
            // provisional timeout, settles it.
            return LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Pending);
        }
        if ended {
            return LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Ended);
        }
        if self.resource_retire(user_id, request_id).await.is_err() {
            return LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Pending);
        }
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retire(user_id, request_id.to_owned(), tokio::time::Instant::now());
        LiveTvResumeAnswer::outcome(LiveTvResumeOutcome::Retired)
    }

    /// What a client's poll of its own start says, without ever handing back a
    /// capability. `unknown` is the honest answer for an id this owner has
    /// never seen — which, after a restart, is every id.
    pub(crate) fn start_state_local(&self, user_id: i64, request_id: &str) -> LiveTvStartState {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for session in registry.sessions_for_public_request(user_id, request_id) {
            if session.cancel.is_cancelled() {
                continue;
            }
            let phase = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .phase;
            return match phase {
                LiveTvSessionPhase::Active => LiveTvStartState {
                    state: "active".to_owned(),
                    code: None,
                },
                LiveTvSessionPhase::Starting | LiveTvSessionPhase::Provisional => {
                    LiveTvStartState {
                        state: "starting".to_owned(),
                        code: None,
                    }
                }
                LiveTvSessionPhase::Failed => LiveTvStartState {
                    state: "ended".to_owned(),
                    code: Some("stream_failed".to_owned()),
                },
            };
        }
        if let Some(tombstone) = registry.tombstone_for_public_request(user_id, request_id) {
            return LiveTvStartState {
                state: "ended".to_owned(),
                code: Some(tombstone.error.code().to_owned()),
            };
        }
        if registry.is_retired(user_id, request_id) {
            return LiveTvStartState {
                state: "retired".to_owned(),
                code: None,
            };
        }
        LiveTvStartState {
            state: "unknown".to_owned(),
            code: None,
        }
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
                    output_height: session.output().height,
                    state: session_phase_name(state.phase).to_owned(),
                    delivery: session.delivery(),
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
        let mut owned = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .sessions
                .values()
                .map(|session| session.directory.clone())
                .chain(
                    registry
                        .transports
                        .values()
                        .map(|transport| transport.scratch.clone()),
                )
                .collect::<HashSet<_>>()
        };
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

    /// The hourly scratch sweep, the orphan owner's `ORPHAN_RETRY` pass, and
    /// the incarnation touch share one loop.
    pub(crate) async fn scratch_sweep_loop(self: Arc<Self>, shutdown: CancellationToken) {
        let mut next_sweep = tokio::time::Instant::now();
        loop {
            if tokio::time::Instant::now() >= next_sweep {
                let removed = self.sweep_orphan_scratch().await;
                if removed > 0 {
                    tracing::info!(removed, "swept orphaned Live TV scratch directories");
                }
                next_sweep = tokio::time::Instant::now() + SCRATCH_SWEEP_INTERVAL;
            }
            self.retry_orphans().await;
            self.touch_incarnation().await;
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(ORPHAN_RETRY) => {}
            }
        }
    }

    /// Refresh `last_seen_at` every `INCARNATION_TOUCH` while anything is
    /// live, so a crash leaves a recent enough value to explain the
    /// capabilities it stranded.
    async fn touch_incarnation(self: &Arc<Self>) {
        let held = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .held();
        if !self.incarnation.touch_due(held) {
            return;
        }
        let manager = Arc::clone(self);
        let _ = tokio::task::spawn_blocking(move || manager.incarnation.write()).await;
    }

    pub(crate) fn metrics_handle(&self) -> Arc<LiveTvMetrics> {
        Arc::clone(&self.metrics)
    }

    pub(crate) fn set_settings_changes(&self, changes: SettingsChangeSource) {
        *self
            .settings_changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(changes);
    }

    fn transports_held(&self) -> usize {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .held()
    }

    /// A transport opened: a parked settings observer starts reading.
    fn wake_settings_observer(&self) {
        self.observer_wake.notify_one();
    }

    /// This node's Live TV settings observation (plan L-02 §3.1).
    ///
    /// While a transport is held it takes one consistent read a second and
    /// publishes it as the session fence's observation, which is why sessions
    /// do not read the store themselves. While nothing is held nothing
    /// validates against the observation, so it parks: one read at boot (read
    /// again each interval until one succeeds), then none until a transport
    /// opens (whose start fence read is the first
    /// observation, `ensure_session_fence`) or the settings may have changed
    /// on another node. A change elsewhere moves this node's applied Raft
    /// index; the loop then reads `live_tv.config_generation` from its own
    /// replica, at most once an interval, and takes a consistent read only
    /// when that generation differs from the one it holds. Every Live TV
    /// settings save bumps the generation (`http/system.rs`), and a save on
    /// this node reaches `observe_config` itself. The deinterlace switch is
    /// saved without a bump, but nothing this observation feeds reads it:
    /// a start reads the settings directly.
    ///
    /// The `plurx_live_tv_enabled` and `plurx_live_tv_device_ready` gauges
    /// and the guide cache's generation ride on what this loop observes; see
    /// `LiveTvMetrics::settings_watched` for how they stay current while it
    /// is parked.
    pub(crate) async fn settings_observer_loop(self: Arc<Self>, shutdown: CancellationToken) {
        let mut changes = self
            .settings_changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or(SettingsChangeSource::LocalOnly);
        // Nothing is proved until a read succeeds, so the first pass reads (the
        // boot read) and the loop keeps reading at the running cadence until
        // one succeeds: parking on a failed read would hold the gauges unproved
        // until some unrelated write moved the applied index.
        let mut proved = false;
        let mut applied = None;
        let mut watch_open = true;
        loop {
            if self.transports_held() > 0
                || !proved
                || matches!(changes, SettingsChangeSource::Polled)
            {
                self.metrics
                    .settings_watched
                    .store(false, Ordering::Release);
                // A consistent read covers every entry applied before it.
                applied = changes.applied_index();
                proved = tokio::select! {
                    _ = shutdown.cancelled() => return,
                    proved = self.observe_fence() => proved,
                };
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(FENCE_OBSERVATION_INTERVAL) => {}
                }
                continue;
            }
            self.metrics
                .settings_watched
                .store(proved && watch_open && changes.live(), Ordering::Release);
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                () = self.observer_wake.notified() => {
                    // The start that opened the transport has just published
                    // its own read; the running reads begin one interval on.
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(FENCE_OBSERVATION_INTERVAL) => {}
                    }
                }
                open = changes.changed(), if watch_open => {
                    if !open {
                        // No later change can be seen, so the parked reading
                        // is no longer proved; the gauges age out.
                        watch_open = false;
                        continue;
                    }
                    if changes.applied_index() == applied {
                        continue;
                    }
                    // One replica read per interval however many entries apply.
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(FENCE_OBSERVATION_INTERVAL) => {}
                    }
                    applied = changes.applied_index();
                    proved = match plurx_core::store::SettingsStore::local_setting_hint(
                        &*self.store,
                        keys::LIVE_TV_CONFIG_GENERATION,
                    )
                    .await
                    {
                        Ok(value) => {
                            let generation = value
                                .and_then(|value| value.parse::<i64>().ok())
                                .unwrap_or(0);
                            if self
                                .fence
                                .observed()
                                .is_some_and(|held| held.config.generation == generation)
                            {
                                true
                            } else {
                                tokio::select! {
                                    _ = shutdown.cancelled() => return,
                                    proved = self.observe_fence() => proved,
                                }
                            }
                        }
                        Err(error) => {
                            tracing::debug!(
                                %error,
                                "Live TV settings generation hint could not be read"
                            );
                            false
                        }
                    };
                }
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
        if config.guide_source == GuideSource::Off {
            return LiveTvGuide::unavailable(config.guide_source, window, None);
        }
        self.guide_cache.read(config, window).await
    }

    /// The owner's immutable full guide for internal scheduling. This never
    /// fetches and never applies an HTTP response byte cap.
    pub(crate) async fn local_guide_view(&self, config: &LiveTvConfig) -> Option<GuideView> {
        if config.guide_source == GuideSource::Off {
            return None;
        }
        let view = self.guide_cache.view(config.generation).await;
        if let Some(view) = view.as_ref() {
            tracing::trace!(
                fetched_at = view.fetched_at,
                age_seconds = view.age.as_secs(),
                freshness = ?view.freshness,
                "using the full owner-side guide view"
            );
        }
        view
    }

    /// Run one refresh now. Manual and background work share one admission
    /// slot, so a click cannot overlap the loop and publish an older result.
    pub(crate) async fn refresh_guide(
        &self,
        config: &LiveTvConfig,
        force: bool,
    ) -> Result<LiveTvGuide, LiveTvError> {
        self.refresh_guide_with_cancel(config, force, CancellationToken::new())
            .await
    }

    async fn refresh_guide_with_cancel(
        &self,
        config: &LiveTvConfig,
        _force: bool,
        external_cancel: CancellationToken,
    ) -> Result<LiveTvGuide, LiveTvError> {
        if !config.guide_fetches() {
            return Err(LiveTvError::InvalidConfig(
                "no programme guide source is configured".to_owned(),
            ));
        }
        let refresh_permit = Arc::new(
            Arc::clone(&self.guide_cache.refresh_admission)
                .try_acquire_owned()
                .map_err(|_| {
                    LiveTvError::DeviceUnavailable(
                        "a guide refresh is already running; retry shortly".to_owned(),
                    )
                })?,
        );
        let refresh_cancel = self.guide_cache.begin_refresh(config);
        let sequence = self
            .guide_cache
            .next_sequence
            .fetch_add(1, Ordering::Relaxed);
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
        let clock = resource::now_ms();
        let mut guide_lease = match self
            .store
            .acquire_lease(
                &format!("live-tv-guide:{}", config.generation),
                &self.node_id,
                clock,
                clock.saturating_add(300_000),
            )
            .await
            .map_err(|e| LiveTvError::DeviceUnavailable(e.to_string()))?
        {
            plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) => lease,
            plurx_core::cluster::coordination::LeaseClaim::Held { owner_node_id, .. } => {
                return Err(LiveTvError::DeviceUnavailable(format!("guide source refresh is assigned to {owner_node_id}; cached guides remain available")));
            }
        };
        let result = async {
            let deadline = tokio::time::Instant::now() + guide::GUIDE_REFRESH_TIMEOUT;
            let fetch = self.fetch_guide(
                config,
                &channels,
                deadline,
                &refresh_cancel,
                &refresh_permit,
            );
            tokio::pin!(fetch);
            let watch_config = async {
                loop {
                    tokio::select! {
                        _ = external_cancel.cancelled() => {
                            refresh_cancel.cancel();
                            return;
                        }
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    }
                    match self.config().await {
                        Ok(current) if current == *config => {}
                        _ => {
                            refresh_cancel.cancel();
                            return;
                        }
                    }
                }
            };
            tokio::pin!(watch_config);
            let coordinated = async {
                tokio::select! {
                    result = &mut fetch => result,
                    _ = &mut watch_config => fetch.await,
                }
            };
            let (result, timed_out) = match tokio::time::timeout_at(deadline, coordinated).await {
                Ok(result) => (result, false),
                Err(_) => {
                    refresh_cancel.cancel();
                    (
                        Err(LiveTvError::DeviceUnavailable(
                            "programme guide refresh exceeded its total deadline".to_owned(),
                        )),
                        true,
                    )
                }
            };
            if external_cancel.is_cancelled() {
                return Err(LiveTvError::DeviceUnavailable(
                    "programme guide refresh stopped during shutdown".to_owned(),
                ));
            }
            let current = self.config().await?;
            if current != *config || (refresh_cancel.is_cancelled() && !timed_out) {
                return Err(LiveTvError::DeviceUnavailable(
                    "programme guide settings changed while the refresh was running".to_owned(),
                ));
            }
            let clock = resource::now_ms();
            guide_lease = match tokio::time::timeout(
                Duration::from_secs(2),
                self.store
                    .renew_lease(&guide_lease, clock, clock.saturating_add(300_000)),
            )
            .await
            {
                Ok(Ok(Some(lease))) => lease,
                _ => {
                    return Err(LiveTvError::DeviceUnavailable(
                        "guide refresh authority expired before publication".into(),
                    ))
                }
            };
            let outcome = if result.is_ok() { "ok" } else { "error" };
            self.metrics
                .observe_guide_refresh(config.guide_source, outcome);
            let published = self
                .guide_cache
                .store(config.generation, sequence, &result)
                .await;
            // A failed refresh keeps the previous titles for as long as the cache
            // behind them is still served.
            if published {
                if let Ok(guide) = &result {
                    self.metrics.observe_guide(guide.total_programmes());
                    self.publish_guide_titles(guide);
                }
            }
            result
        }
        .await;
        // Release success and ordinary failure alike. Cancellation of this
        // future still falls back to the bounded lease expiry.
        crate::store_result::observe_timeout(
            crate::store_result::Operation::ReleaseLiveTvGuideLease,
            crate::store_result::Discard::BestEffort,
            tokio::time::timeout(
                Duration::from_secs(2),
                self.store.release_lease(&guide_lease, resource::now_ms()),
            )
            .await,
        );
        result
    }

    /// The lineup the guide is matched against, read from the snapshot cache
    /// without ever refreshing it. An empty answer is a refresh that has to
    /// wait, not a guide with no channels.
    /// The display name of one lineup channel, if this node has a lineup for
    /// the current generation. `None` is a real answer — a node that has never
    /// read channels does not know, and guessing would put an invented name on
    /// a recording that keeps it for ever.
    pub(crate) async fn channel_name(
        &self,
        config: &LiveTvConfig,
        channel_id: &str,
    ) -> Option<String> {
        self.cached_lineup(config)
            .await
            .into_iter()
            .find(|channel| channel.id == channel_id)
            .map(|channel| channel.guide_name)
    }

    pub(crate) async fn channel_guide_number(
        &self,
        config: &LiveTvConfig,
        channel_id: &str,
    ) -> Option<String> {
        self.cached_lineup(config)
            .await
            .into_iter()
            .find(|channel| channel.id == channel_id)
            .map(|channel| channel.guide_number)
    }

    /// A probe folder of its own for each transport: two transports on one
    /// channel can overlap by a close, and neither may delete the other's.
    fn transport_scratch(&self) -> PathBuf {
        self.scratch_root
            .join(format!("transport-{}", uuid::Uuid::new_v4().simple()))
    }

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
        deadline: tokio::time::Instant,
        cancel: &CancellationToken,
        refresh_permit: &Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<LiveTvGuide, LiveTvError> {
        let window = guide::refresh_window(config.guide_hours);
        let channels = match config.guide_source {
            GuideSource::Off => Vec::new(),
            GuideSource::HdHomeRun => {
                let address = config.device_ipv4.ok_or_else(|| {
                    LiveTvError::InvalidConfig(
                        "an HDHomeRun address is required for the HDHomeRun guide".to_owned(),
                    )
                })?;
                // Only the HDHomeRun source accumulates: XMLTV answers the
                // whole horizon in one document, so carrying rows forward
                // there would preserve a programme the grabber deliberately
                // removed.
                let carried = self.guide_cache.owner_copy(config.generation).await;
                self.fetch_hdhomerun_guide(
                    address,
                    lineup,
                    &window,
                    deadline,
                    cancel,
                    refresh_permit,
                    carried,
                )
                .await?
            }
            GuideSource::Xmltv => {
                let client = self.guide_client()?;
                let url = validate_xmltv_url(&config.xmltv_url)?;
                let pinned = cancellable_guide_step(cancel, resolve_permitted_addrs(&url)).await?;
                let client = if pinned.is_empty() {
                    client.clone()
                } else {
                    self.pinned_guide_client(&url, &pinned)?
                };
                let body = cancellable_guide_step(
                    cancel,
                    fetch_bounded(
                        &client,
                        url,
                        guide::GUIDE_MAX_DOCUMENT_BYTES,
                        remaining_guide_budget(deadline)?.min(guide::GUIDE_FETCH_TIMEOUT),
                    ),
                )
                .await?;
                let lineup = lineup.to_vec();
                let permit = Arc::clone(refresh_permit);
                let parser = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let body = guide::decompress_if_gzip(body)?;
                    guide::parse_xmltv(&body, &lineup)
                });
                await_guide_parser(parser, cancel).await?
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
            next_refresh_at: None,
            matched_channels: matched,
            lineup_channels: lineup.len(),
            channels,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_hdhomerun_guide(
        &self,
        address: Ipv4Addr,
        lineup: &[LiveTvChannel],
        window: &GuideWindow,
        deadline: tokio::time::Instant,
        cancel: &CancellationToken,
        refresh_permit: &Arc<tokio::sync::OwnedSemaphorePermit>,
        carried: Option<LiveTvGuide>,
    ) -> Result<Vec<LiveTvGuideChannel>, LiveTvError> {
        let client = self.guide_client()?;
        // Read the credential, use it, drop it. It is a local binding inside
        // this function and appears in nothing this function returns.
        let device_auth =
            cancellable_guide_step(cancel, guide::read_device_auth(&client, address)).await?;
        let bulk = cancellable_guide_step(
            cancel,
            fetch_bounded(
                &client,
                guide::guide_request_url(&device_auth, None, None)?,
                guide::GUIDE_MAX_DOCUMENT_BYTES,
                remaining_guide_budget(deadline)?.min(guide::GUIDE_FETCH_TIMEOUT),
            ),
        )
        .await?;
        let parse_lineup = lineup.to_vec();
        let permit = Arc::clone(refresh_permit);
        let parser = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let by_number = parse_lineup
                .iter()
                .map(|channel| (channel.guide_number.clone(), channel))
                .collect::<BTreeMap<_, _>>();
            guide::parse_hdhomerun_guide(&bulk, &by_number)
        });
        let mut channels = await_guide_parser(parser, cancel).await?;

        // Carry the previous tick's rows forward before extending. Filling a
        // fortnight is not one refresh: the bulk call answers a few hours, and
        // an extension page costs a request each. Accumulating across ticks is
        // what lets a 14-day horizon exist at all, and it is only safe because
        // the cache is durable — a restart no longer starts from nothing.
        if let Some(carried) = carried {
            merge_carried_guide(&mut channels, carried, window.start);
        }

        // The free tier answers a few hours to the bulk call. Extend the
        // channels that fall short until the request budget is spent — an
        // empty or repeated answer means that channel has no more data and
        // stops it, so a lineup of stubborn channels cannot spin here.
        //
        // The budget is shared with revalidation and the tail goes first: a
        // grid that ends early is more visible than one whose far end is a few
        // hours out of date.
        let mut budget = guide::GUIDE_MAX_EXTENSION_REQUESTS;
        let mut index = 0;
        while index < channels.len() && budget > 0 {
            if cancel.is_cancelled() {
                return Err(guide_refresh_cancelled());
            }
            let Ok(remaining) = remaining_guide_budget(deadline) else {
                break;
            };
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
            let page = match cancellable_guide_step(
                cancel,
                fetch_bounded(
                    &client,
                    url,
                    guide::GUIDE_MAX_DOCUMENT_BYTES,
                    remaining.min(guide::GUIDE_FETCH_TIMEOUT),
                ),
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
            let parse_lineup = lineup.to_vec();
            let permit = Arc::clone(refresh_permit);
            let parser = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let by_number = parse_lineup
                    .iter()
                    .map(|channel| (channel.guide_number.clone(), channel))
                    .collect::<BTreeMap<_, _>>();
                guide::parse_hdhomerun_guide(&page, &by_number)
            });
            let extra = await_guide_parser(parser, cancel).await?;
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

        // Whatever request budget survived the tail goes on re-reading one day
        // of the cached horizon, cycling through it a day at a time. Without
        // this a filled fortnight is never revisited and a provider's schedule
        // change would never reach the scheduler.
        self.revalidate_guide_day(
            &client,
            &device_auth,
            lineup,
            &mut channels,
            window,
            deadline,
            cancel,
            refresh_permit,
            budget,
        )
        .await?;
        Ok(channels)
    }

    /// Re-fetch one day of the horizon, chosen by cycling a counter, and merge
    /// it over what is cached. Failures are skipped rather than propagated:
    /// the grid is already useful and a revalidation that could fail a refresh
    /// would make the guide *less* reliable than not revalidating at all.
    #[allow(clippy::too_many_arguments)]
    async fn revalidate_guide_day(
        &self,
        client: &reqwest::Client,
        device_auth: &str,
        lineup: &[LiveTvChannel],
        channels: &mut [LiveTvGuideChannel],
        window: &GuideWindow,
        deadline: tokio::time::Instant,
        cancel: &CancellationToken,
        refresh_permit: &Arc<tokio::sync::OwnedSemaphorePermit>,
        mut budget: usize,
    ) -> Result<(), LiveTvError> {
        if budget == 0 || channels.is_empty() {
            return Ok(());
        }
        let span = (window.end - window.start).max(1);
        let stride = GUIDE_REVALIDATION_HOURS * 3600;
        let days = ((span + stride - 1) / stride).max(1) as u64;
        let cursor = self
            .guide_revalidation_cursor
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let day = (cursor % days) as i64;
        let from = window.start + day * stride;
        if from >= window.end {
            return Ok(());
        }
        for channel in channels.iter_mut() {
            if budget == 0 || cancel.is_cancelled() {
                break;
            }
            let Ok(remaining) = remaining_guide_budget(deadline) else {
                break;
            };
            budget -= 1;
            let number = channel.guide_number.clone();
            let url = guide::guide_request_url(device_auth, Some(&number), Some(from))?;
            let Ok(page) = cancellable_guide_step(
                cancel,
                fetch_bounded(
                    client,
                    url,
                    guide::GUIDE_MAX_DOCUMENT_BYTES,
                    remaining.min(guide::GUIDE_FETCH_TIMEOUT),
                ),
            )
            .await
            else {
                continue;
            };
            let parse_lineup = lineup.to_vec();
            let permit = Arc::clone(refresh_permit);
            let parser = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let by_number = parse_lineup
                    .iter()
                    .map(|channel| (channel.guide_number.clone(), channel))
                    .collect::<BTreeMap<_, _>>();
                guide::parse_hdhomerun_guide(&page, &by_number)
            });
            let extra = await_guide_parser(parser, cancel).await?;
            if let Some(extra) = extra
                .into_iter()
                .find(|candidate| candidate.guide_number == number)
                .filter(|candidate| !candidate.programmes.is_empty())
            {
                guide::merge_channel(channel, extra);
            }
        }
        Ok(())
    }

    /// An ingress's memory of the owner's answer, if it is still current.
    pub(crate) async fn relayed_guide(&self, generation: i64) -> Option<LiveTvGuide> {
        let cached = self.relayed_guide.lock().await;
        let (cached_generation, observed, guide) = cached.as_ref()?;
        (*cached_generation == generation
            && tokio::time::Instant::now().duration_since(*observed)
                <= relay_guide_memory(guide, unix_seconds()))
        .then(|| guide.clone())
    }

    pub(crate) async fn remember_relayed_guide(&self, generation: i64, guide: LiveTvGuide) {
        if guide.freshness != GuideFreshness::Unavailable {
            let cached = CachedGuide {
                generation,
                observed: tokio::time::Instant::now(),
                age_offset: Duration::from_secs(guide.age_seconds),
                fetched_at: guide.fetched_at.unwrap_or(unix_seconds()),
                guide: Arc::new(guide.clone()),
            };
            let mut state = self.guide_cache.state.lock().await;
            let newer = state.cached.as_ref().is_none_or(|old| {
                old.generation != generation || old.fetched_at < cached.fetched_at
            });
            if newer {
                state.cached = Some(cached.clone());
            }
            drop(state);
            if newer {
                self.publish_guide_titles(&guide);
                self.guide_cache.persist_guide(&cached).await;
            }
        }
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
    pub(crate) async fn guide_refresh_loop(
        self: Arc<Self>,
        mut serving: tokio::sync::watch::Receiver<crate::serving_fence::ServingState>,
        shutdown: CancellationToken,
    ) {
        let mut delay = guide::GUIDE_REFRESH_INTERVAL;
        let mut serving_open = true;
        loop {
            if let Ok(config) = self.config().await {
                let stale_generation = self
                    .guide_cache
                    .cached_generation()
                    .await
                    .is_some_and(|generation| generation != config.generation);
                if stale_generation || !config.guide_fetches() {
                    self.guide_cache.invalidate().await;
                    self.clear_guide_titles();
                }
                // A wake can arrive for a guide that was fetched moments ago —
                // a rolling deploy produces one fence transition per node, and
                // each one wakes this loop. Refetching from a third-party
                // grabber four times in a minute is rude and buys nothing, so
                // a copy younger than the cold-retry window stands.
                let just_refreshed = self
                    .guide_cache
                    .age_for(config.generation)
                    .await
                    .is_some_and(|age| age < guide::GUIDE_COLD_LINEUP_RETRY);
                if config.guide_fetches() && !just_refreshed && self.serving.admit().is_some() {
                    // One bounded lineup read when nothing has been read yet:
                    // the guide cannot be matched against an empty lineup, and
                    // on a cold owner nothing else has asked for one. A warm
                    // snapshot is never refreshed from here — `local_snapshot`
                    // serves it, and its own connect timeout bounds a tuner
                    // that is switched off.
                    if self.cached_lineup(&config).await.is_empty() {
                        let _ = self.local_snapshot(&config, false, false).await;
                    }
                    let refreshed = self
                        .refresh_guide_with_cancel(&config, false, shutdown.clone())
                        .await;
                    if shutdown.is_cancelled() {
                        return;
                    }
                    match refreshed {
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
                                "programme guide refresh failed on the session server"
                            );
                        }
                    }
                } else if just_refreshed {
                    // Not a skip in the sense the metric means: there is a
                    // guide, it is current, and the loop simply has nothing to
                    // do until the interval comes round.
                    delay = guide::GUIDE_REFRESH_INTERVAL;
                } else {
                    delay = guide_skip_delay(config.guide_fetches());
                    self.metrics
                        .observe_guide_refresh(config.guide_source, "skipped");
                }
            }
            self.guide_cache
                .note_next_refresh(unix_seconds().saturating_add(delay.as_secs() as i64))
                .await;
            let wake = Arc::clone(&self.guide_cache.wake);
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
                _ = wake.notified() => {}
                // A fence gained *or* lost fires this. On a loss the next tick
                // sees `admit()` is `None`, records a skip and sleeps a minute;
                // that is cheap, and it is what makes admission arrive within
                // seconds rather than at the next interval.
                changed = async {
                    if serving_open {
                        serving.changed().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if changed.is_err() {
                        serving_open = false;
                    }
                }
            }
        }
    }
}

/// How long to wait after a tick that did no work. The owner with a source
/// that is merely not admitted yet is seconds from being admitted, so it comes
/// back in a minute; every other skip — not the owner, no source configured —
/// changes only with a settings save, and a save wakes the loop itself.
/// How long an ingress may repeat the owner's answer without asking again.
/// Bounded by the owner's own clock, so an ingress never holds a guide past
/// the moment a newer one exists.
fn relay_guide_memory(guide: &LiveTvGuide, now: i64) -> Duration {
    if guide.freshness == GuideFreshness::Unavailable {
        return RELAY_UNAVAILABLE_GUIDE_MEMORY;
    }
    match guide.next_refresh_at {
        Some(next) => RELAY_GUIDE_MEMORY.min(Duration::from_secs(
            next.saturating_sub(now).max(0).unsigned_abs(),
        )),
        None => RELAY_GUIDE_MEMORY,
    }
}

/// How long to wait after a tick that did no work. A node with a source that
/// is merely not admitted yet is seconds from being admitted, so it comes back
/// in a minute; a skip with no source configured changes only with a settings
/// save, and a save wakes the loop itself.
fn guide_skip_delay(source_configured: bool) -> Duration {
    if source_configured {
        guide::GUIDE_COLD_LINEUP_RETRY
    } else {
        guide::GUIDE_REFRESH_INTERVAL
    }
}

impl LiveTvMetrics {
    pub(crate) fn prometheus(&self) -> String {
        let projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = tokio::time::Instant::now();
        let watched = self.settings_watched.load(Ordering::Acquire);
        let config = projection.config.filter(|(_, _, observed)| {
            watched || now.duration_since(*observed) <= Duration::from_secs(3)
        });
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
        let transports = Self::transports_prometheus(&registry, &self.consumer_evictions);
        drop(registry);
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
             # HELP plurx_live_tv_stray_evictions_total Sessions cancelled to admit their own viewer's new start.\n\
             # TYPE plurx_live_tv_stray_evictions_total counter\n\
             plurx_live_tv_stray_evictions_total {}\n\
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
            self.stray_evictions.load(Ordering::Acquire),
            self.relay_bytes.load(Ordering::Acquire),
        ) + &self.session_ends_prometheus()
            + &self.guide_prometheus()
            + &self.dvr_sink_failures_prometheus()
            + &self.settings_reads_prometheus()
            + &self.fence_observations_prometheus()
            + &self.orphans_prometheus()
            + &self.start_plans_prometheus()
            + &transports
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
            self.incarnation
                .unknown_capability_message(unix_seconds())
                .into(),
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
    /// How many segments the playlist lists, and how much playable media that
    /// list contains. `segments` includes every final file on disk, including
    /// deletion lag and rename-before-rewrite transients, and must never be
    /// used as the publication gate.
    listed: usize,
    listed_seconds: f64,
    target_duration: u64,
    init: Option<(String, u64)>,
    segments: HashMap<u64, (String, u64)>,
}

impl ScratchInventory {
    /// The newest sequence number the playlist lists. Unlike
    /// `media_sequence`, this advances before the 24-entry window is full and
    /// therefore measures producer progress from the first segment onward.
    fn newest_listed(&self) -> Option<u64> {
        self.listed
            .checked_sub(1)
            .map(|offset| self.media_sequence + offset as u64)
    }
}

fn record_producer_inventory(
    state: &mut LiveTvSessionState,
    inventory: ScratchInventory,
    now: tokio::time::Instant,
) -> bool {
    let publishable = startup_publishable(&inventory);
    let newest = inventory.newest_listed();
    if newest > state.newest_listed || state.publication.is_none() {
        state.last_progress = now;
    }
    state.newest_listed = newest;
    state.media_sequence = inventory.media_sequence;
    // Manifest bytes and their complete current/deletion-lag inventory cross
    // the lock together. A client can never receive a playlist from one
    // sample and authorization from another.
    state.publication = Some(inventory);
    publishable
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
        let now = tokio::time::Instant::now();
        let verdict = {
            let state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            startup_waiter_verdict(
                &state,
                session.started,
                now + Duration::from_secs(2),
                session.tuner_bytes.load(Ordering::Acquire),
            )
        };
        if let Some(result) = verdict {
            return result.map(|mut provisional| {
                if recovered {
                    provisional.outcome = LiveTvStartOutcome::Recovered;
                }
                provisional
            });
        }
        // Waiting in slices rather than to the deadline: the budget can grow
        // while this request is parked, and a wait pinned to the budget in
        // force when it began would refuse a channel that started feeding a
        // moment later.
        let _ = tokio::time::timeout(STARTUP_BUDGET_REVIEW, notified).await;
    }
}

/// Snapshot the producer's answer and publication under one lock. Once the
/// gate is satisfied, the producer may be awaiting its serving fence before it
/// installs the provisional; that interval is not a startup timeout, and can
/// never produce the impossible "2 segments, but not 2" verdict.
fn startup_waiter_verdict(
    state: &LiveTvSessionState,
    started: tokio::time::Instant,
    review_at: tokio::time::Instant,
    tuner_bytes: u64,
) -> Option<Result<LiveTvProvisional, LiveTvError>> {
    if let Some(result) = &state.startup {
        return Some(result.clone());
    }
    if state.publication.as_ref().is_some_and(startup_publishable) {
        return None;
    }
    startup_overdue(
        started,
        review_at,
        tuner_bytes,
        state
            .publication
            .as_ref()
            .map_or(0, |publication| publication.listed),
    )
    .map(|reason| Err(LiveTvError::StartupTimeout(reason)))
}

/// A live-TV request id is 128 bits written as 32 lower-case hex characters,
/// and it is opaque to everything that handles it: the owner keys a registry
/// slot by it, a client persists it as its hint, and neither reads a meaning
/// out of its bytes.
///
/// This is the ONLY definition. The owner used to demand a version-4 UUID
/// here, which was true only for as long as the server minted the id itself.
/// Once a client supplied it — three hex digits of a v4 UUID are not random,
/// and `crypto.getRandomValues` does not know that — the ingress accepted ids
/// the owner then refused as `invalid_request`, which is to say every start
/// whose thirteenth hex digit was not a `4`. The public surface and the owner
/// share one function so the two can never drift apart again.
pub(crate) fn valid_request_id(value: &str) -> bool {
    plurx_core::live_tv_resource::valid_request_id(value)
        && value.bytes().all(|b| !b.is_ascii_uppercase())
}

fn validate_start_request(request: &LiveTvStartRequest) -> Result<(), LiveTvError> {
    if !valid_request_id(&request.request_id) {
        return Err(LiveTvError::InvalidResponse(
            "live-TV request id is invalid".into(),
        ));
    }
    if request.source_node_id.trim().is_empty()
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
    if let Some(playback) = &request.playback {
        playback.validate().map_err(LiveTvError::InvalidResponse)?;
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
            "Live TV is disabled in Settings → Live TV".into(),
        ));
    }
    // The configuration is this node's own read (`from_snapshot` names the
    // local node), so only the request's addressing can disagree with it.
    if request.expected_owner_node_id != local_node_id {
        return Err(LiveTvError::OwnerUnavailable(
            "this node is not the current HDHomeRun worker".into(),
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
    kind: LiveStartKind,
) {
    let result = run_live_session_inner(&manager, &session, &config, kind).await;
    session.cancel.cancel();
    // Detach before anything slower: the transport's fan-out sees this
    // viewer gone on its next chunk, and if it was the last consumer the
    // tuner is released without waiting for this session's own cleanup.
    session.detach_from_transport();
    // Join the child and its bounded stderr reader before classifying startup
    // failure; otherwise EOF can beat the decoder diagnostic to the waiter.
    let withhold_reap = manager
        .upgrade()
        .is_some_and(|manager| manager.reap_withheld());
    let cleanup = cleanup_session(&session, withhold_reap).await;
    let diagnostic = *session
        .encoder_diagnostic
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A fast graph reconfiguration failure can exit before the next observer
    // tick. Cleanup has joined the existing bounded stderr reader, so its
    // confirmed source-change fact now takes part in terminal classification.
    let source_changed = session.source_format_changed.load(Ordering::Acquire);
    if source_changed {
        if let Some(transport) = session.transport() {
            transport.mark_source_stale();
        }
        if let Some(owner) = manager.upgrade() {
            owner.forget_source_facts(&SourceFormatKey {
                generation: config.generation,
                device_id: session.device_id.clone(),
                channel_id: session.channel.id.clone(),
            });
        }
    }
    let result = classify_live_terminal_error(
        result,
        session.decoder_unavailable.load(Ordering::Acquire),
        diagnostic,
        source_changed,
    );
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
    let manager = manager.upgrade();
    if let (Some(manager), Err(error)) = (manager.as_ref(), cleanup.as_ref()) {
        // Plan L-02 §3.4: the session leaves the registry either way. What it
        // could not release — an unconfirmed child with its job and encoder
        // admission, or a directory — moves to the orphan owner first, so
        // there is no instant at which neither holds it.
        tracing::error!(
            kind = %error,
            "Live TV session cleanup did not finish before its deadline; the owner keeps retrying it"
        );
        manager.adopt_orphan(&session).await;
    }
    session
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cleanup = Some(cleanup);
    session.changed.notify_waiters();
    if let Some(manager) = manager {
        let _ = manager
            .resource_advance(
                &session.request,
                plurx_core::live_tv_resource::StartPhase::Closed,
                None,
            )
            .await;
        manager.retire_session(&session);
    }
}

fn classify_live_source_error(
    result: Result<(), LiveTvError>,
    decoder_unavailable: bool,
    encoder_diagnostic: Option<&'static str>,
) -> Result<(), LiveTvError> {
    if decoder_unavailable
        && matches!(
            result,
            Err(LiveTvError::StreamFailed(_) | LiveTvError::StartupTimeout(_))
        )
    {
        Err(LiveTvError::CodecUnsupported("The session server's FFmpeg could not detect or decode the required video and audio. ATSC 3.0 may require HEVC and AC-4 support; try an ATSC 1.0 channel or a decoder-capable FFmpeg build.".into()))
    } else {
        match (result, encoder_diagnostic) {
            (Err(LiveTvError::StreamFailed(message)), Some(cause)) => {
                Err(LiveTvError::StreamFailed(format!("{message}; {cause}")))
            }
            (result, _) => result,
        }
    }
}

fn classify_live_terminal_error(
    result: Result<(), LiveTvError>,
    decoder_unavailable: bool,
    encoder_diagnostic: Option<&'static str>,
    source_changed: bool,
) -> Result<(), LiveTvError> {
    let result = classify_live_source_error(result, decoder_unavailable, encoder_diagnostic);
    if source_changed
        && !decoder_unavailable
        && encoder_diagnostic.is_none()
        && matches!(result, Err(LiveTvError::StreamFailed(_)))
    {
        Err(LiveTvError::SourceFormatChanged(
            "the broadcast changed format; start the channel again so a fresh delivery route can be selected".into(),
        ))
    } else {
        result
    }
}

async fn remove_session_directory(path: &Path) -> Result<(), LiveTvError> {
    let deadline = tokio::time::Instant::now() + SESSION_DRAIN_TIMEOUT;
    loop {
        let removal = if crate::playback_cache::is_pinned(path) {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "live-TV response still holds cache media",
            ))
        } else {
            tokio::fs::remove_dir_all(path).await
        };
        match removal {
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

async fn cleanup_session(session: &LiveTvSession, withhold_reap: bool) -> Result<(), LiveTvError> {
    // The mutex also serializes retries after a failed physical cleanup.
    let mut process = session.process.lock().await;
    if withhold_reap && process.is_some() {
        return Err(LiveTvError::StreamFailed(
            "FFmpeg exit was not confirmed before the cleanup deadline".into(),
        ));
    }
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
        if let Some(sink) = process.sink.take() {
            sink.drain().await;
        }
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
    kind: LiveStartKind,
) -> Result<(), LiveTvError> {
    let owner = manager
        .upgrade()
        .ok_or_else(|| LiveTvError::StreamFailed("live-TV manager stopped".into()))?;
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

    // The tuner GET, its prefix and its probe belong to the shared transport
    // this session was admitted to (plan L-03 §3.1): an opener waits for its
    // own transport's probe of the prefix it will be fed. A joiner gets the
    // running transport's facts only while they are fresh; failed, stale or
    // old facts are re-probed from the live edge first
    // (`DvrTransport::source_for_viewer`). Either way the plan is checked
    // against the input this viewer's FFmpeg actually opens, below. A device
    // refusal (`tuner_unavailable`) reaches the viewers waiting here as the
    // transport's error.
    let transport = session.transport().ok_or_else(|| {
        LiveTvError::StreamFailed("the live-TV session has no tuner connection".into())
    })?;
    let startup_deadline = session.started + STARTUP_TIMEOUT;
    let facts_key = SourceFormatKey {
        generation: config.generation,
        device_id: session.device_id.clone(),
        channel_id: session.channel.id.clone(),
    };
    // Plan L-02 §3.3. A warm opener plans from the facts this channel's last
    // tune produced and starts its FFmpeg now, fed from the tuner's first
    // byte; `verifying` then holds publication until the transport's probe of
    // those same first bytes has re-proven the plan. Everyone else plans
    // from the facts of this tune, as before, and leaves them for the next
    // opener.
    let (mut source, mut verifying) = match kind {
        LiveStartKind::Warm(facts) => (*facts, true),
        other => {
            let source = transport
                .source_for_viewer(&session.cancel, startup_deadline)
                .await?;
            owner.record_source_facts(
                facts_key.clone(),
                &session.channel.guide_number,
                source.clone(),
            );
            owner.metrics.observe_start_plan(match other {
                LiveStartKind::Joined => StartPlanSource::Joined,
                _ => StartPlanSource::Cold,
            });
            (source, false)
        }
    };
    let policy = live_quality_policy(config, session.request.playback.is_some());
    let support = live_execution_support(&owner.system);
    let system = Arc::clone(&owner.system);
    drop(owner);
    loop {
        let owner = manager
            .upgrade()
            .ok_or_else(|| LiveTvError::StreamFailed("live-TV manager stopped".into()))?;
        let observed = LiveTvSourceFormat {
            video_width: source.width,
            video_height: source.height,
            scan: source.field_order.as_deref().map(|order| {
                if matches!(order, "progressive" | "unknown") {
                    "progressive".to_owned()
                } else {
                    "interlaced".to_owned()
                }
            }),
            audio_channels: source.audio_channels,
            audio_layout: source.audio_layout.clone(),
            observed_at: unix_seconds(),
        };
        session.source_format_expires_at.store(
            observed
                .observed_at
                .saturating_add(SOURCE_FORMAT_TTL.as_secs() as i64),
            Ordering::Release,
        );
        *session
            .source_format
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observed.clone());
        let delivery = crate::live_tv_delivery::resolve_live_delivery(
            &source,
            session.request.playback.as_ref(),
            &policy,
            &support,
        )
        .map_err(LiveTvError::CodecUnsupported)?;
        if delivery.audio_track != 0 {
            let selected = LiveTvSourceFormat {
                audio_channels: delivery.source.audio_channels,
                audio_layout: delivery.source.audio_layout.clone(),
                ..observed
            };
            *session
                .source_format
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(selected);
        }
        let admission = if delivery.video_action == LiveTrackAction::Encode {
            let source_height = source.height.unwrap_or(delivery.output.height);
            let source_codec = source.video_codec.as_deref().unwrap_or("unknown");
            Some(tokio::select! {
                biased;
                _ = session.cancel.cancelled() => {
                    return Err(LiveTvError::CapabilityExpired(
                        "the live-TV start was cancelled before admission".into(),
                    ));
                }
                admission = owner.transcode.admit_live_tv(
                    source_height,
                    source_codec,
                    source.hdr.as_deref(),
                    delivery.output.height,
                    ADMISSION_WAIT,
                ) => admission.map_err(|why| {
                    LiveTvError::EncoderCapacity(format!(
                        "the tuner owner's video encoder could not take this channel: {why}"
                    ))
                })?,
            })
        } else {
            None
        };
        {
            let mut state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.encoder = admission.as_ref().map_or_else(
                || "copy/remux".to_owned(),
                |admission| admission.encoder.label().to_owned(),
            );
        }
        // The caption advertisement is published with the delivery but is
        // not part of the plan FFmpeg is built from, so a warm start's
        // agreement check compares plans, not the proof list's state.
        session.set_delivery(advertise_proven_captions(
            &owner
                .caption_proofs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &source,
            delivery.clone(),
            admission.as_ref().map(|value| value.encoder.label()),
            session.caption_decoders().as_deref(),
        ));
        let mut transcode_plan = LiveTvTranscodePlan::new(
            &owner.system,
            delivery,
            admission.as_ref().map(|value| value.encoder),
            admission
                .as_ref()
                .and_then(crate::transcode::LiveAdmission::software_threads),
        )?;
        let macos_context = owner
            .transcode
            .macos_video_report()
            .context(owner.transcode.macos_video_processing_enabled());
        tokio::select! {
            _ = session.cancel.cancelled() => return Err(LiveTvError::StreamFailed("live-TV start cancelled".into())),
            () = transcode_plan.bind_macos_upload(&owner.system, macos_context.as_ref()) => {}
        }
        tokio::select! {
            _ = session.cancel.cancelled() => return Err(LiveTvError::StreamFailed("live-TV start cancelled".into())),
            result = transcode_plan.verify_macos_upload() => result?
        }
        let sink = crate::scratch_put::PutSink::bind_live_tv(session.directory.clone()).map_err(
            |error| LiveTvError::StreamFailed(format!("binding live-TV cache writer: {error}")),
        )?;
        let mut command = live_ffmpeg_command_for_output(
            &owner.system,
            &transcode_plan,
            &session.directory,
            LiveTvFfmpegInput::Tuner,
            Some(&sink.base_url(0)),
        )?;
        let (mut child, child_job) = crate::process_control::spawn_job_owned(
            &mut command,
            crate::process_control::ChildWork::realtime("Live TV stream"),
        )
        .map_err(|error| {
            LiveTvError::CodecUnsupported(format!("starting live-TV FFmpeg: {error}"))
        })?;
        let stdin = child.stdin.take();
        session
            .source_format_changed
            .store(false, Ordering::Release);
        let source_format_changed = Arc::clone(&session.source_format_changed);
        let input_format = Arc::new(StdMutex::new(None));
        let stderr = child.stderr.take().map(|stderr| {
            tokio::spawn(capture_live_stderr(
                stderr,
                Arc::clone(&session.decoder_unavailable),
                Arc::clone(&session.encoder_diagnostic),
                Arc::clone(&session.source_format),
                Arc::clone(&session.source_format_expires_at),
                Arc::clone(&source_format_changed),
                Arc::clone(&input_format),
            ))
        });
        let Some(stdin) = stdin else {
            *session.process.lock().await = Some(LiveTvProcess {
                child,
                sink: Some(sink),
                _job: child_job,
                _admission: admission,
                stderr,
            });
            return Err(LiveTvError::StreamFailed(
                "live-TV FFmpeg did not expose stdin".into(),
            ));
        };
        // Attach only now that this viewer's own resources exist (its scratch
        // and its FFmpeg's stdin), so the fan-out never feeds a queue nothing
        // drains (plan §3.3).
        let (consumer, feed) =
            dvr::ViewerConsumer::new(session.capability.clone(), session.cancel.clone());
        if let Err(error) = session.attach_consumer(Arc::clone(&consumer)) {
            *session.process.lock().await = Some(LiveTvProcess {
                child,
                sink: Some(sink),
                _job: child_job,
                _admission: admission,
                stderr,
            });
            return Err(error);
        }
        let pump_failure = Arc::new(StdMutex::new(None));
        let pump = tokio::spawn(pump_viewer_feed(
            feed,
            stdin,
            session.cancel.clone(),
            Arc::clone(&session.tuner_bytes),
            Arc::clone(&pump_failure),
        ));
        let serving = owner.serving.clone();
        drop(owner);

        let mut tick = tokio::time::interval(SESSION_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut ticks = 0u64;
        let mut published = false;
        let mut recorded_source_observation = session
            .channel
            .source_format
            .as_ref()
            .map_or(0, |format| format.observed_at);
        let mut input_checked = false;
        // At most one inventory scan in flight (plan L-02 §3.4). A scan that has
        // not returned within a tick — a `stat` on a stalled mount — is kept and
        // polled again on the next tick rather than replaced, so a hung
        // filesystem cannot pile up blocking-pool threads, and the tick carries
        // on to the startup and producer-progress timers, which end the session.
        let mut inventory_scan: Option<ScratchScanTask> = None;
        // The running fence takes the same shape (plan L-03): its ledger read is
        // a leader-consistent Store read with no deadline of its own, so it runs
        // on its own task and the tick polls it. Awaited inline, a slow read
        // stopped the tick, and with it the scratch scans that advance the
        // publication: the playlist froze while FFmpeg kept producing, and a
        // player that sees an unchanged playlist for 3.5 target durations gives
        // up. At most one check is in flight; its verdict ends the session
        // exactly as the awaited one did.
        let mut fence_check: Option<SessionFenceTask> = None;
        let observe = async {
            loop {
                tick.tick().await;
                if session.cancel.is_cancelled() {
                    break Ok(None);
                }
                if let Some(error) = sink.failure() {
                    break Err(LiveTvError::StreamFailed(format!(
                        "live-TV cache writer refused output: {error}"
                    )));
                }
                if let Some(status) = child.try_wait().map_err(|error| {
                    LiveTvError::StreamFailed(format!("waiting for FFmpeg: {error}"))
                })? {
                    break Err(LiveTvError::StreamFailed(format!(
                        "live-TV FFmpeg exited with {status}"
                    )));
                }
                // An eviction ends this session on this tick, under its own
                // reason. The pump may still be blocked writing to an FFmpeg that
                // stopped reading, for up to `TUNER_READ_TIMEOUT`; waiting for it
                // would keep that FFmpeg and its encode permit, and report the
                // stall instead of the eviction (plan L-03 §3.2).
                if pump.is_finished() || consumer.evicted().is_some() {
                    break Err(viewer_stream_error(&pump_failure, &consumer, &transport));
                }
                // Plan L-02 §3.3: a warm start publishes nothing until the probe
                // of this tune's first bytes re-proves its plan. Agreement means
                // the FFmpeg already running is exactly the one those facts would
                // have started; anything else restarts the start cold.
                if verifying {
                    match transport.opening_facts() {
                        None => {}
                        Some(Err(error)) => break Err(error),
                        Some(Ok(probed)) => {
                            if let Err(error) = transcode_plan.verify_macos_upload().await {
                                break Err(error);
                            }
                            match warm_plan_agrees(
                                &system,
                                &transcode_plan,
                                &source,
                                &probed,
                                session.request.playback.as_ref(),
                                &policy,
                                &support,
                                &session.directory,
                            ) {
                                Some(delivery) => {
                                    verifying = false;
                                    *session
                                        .source_format
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(session_source_format(&probed, &delivery));
                                    let owner = manager.upgrade();
                                    session.set_delivery(match &owner {
                                        Some(owner) => advertise_proven_captions(
                                            &owner
                                                .caption_proofs
                                                .lock()
                                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                                            &probed,
                                            delivery,
                                            transcode_plan
                                                .encoder
                                                .as_ref()
                                                .map(|encoder| encoder.label()),
                                            session.caption_decoders().as_deref(),
                                        ),
                                        None => delivery,
                                    });
                                    if let Some(owner) = owner {
                                        owner.record_source_facts(
                                            facts_key.clone(),
                                            &session.channel.guide_number,
                                            probed.clone(),
                                        );
                                        owner
                                            .metrics
                                            .observe_start_plan(StartPlanSource::WarmAgreed);
                                    }
                                    source = probed;
                                }
                                None => break Ok(Some(probed)),
                            }
                        }
                    }
                }
                if !input_checked && !verifying {
                    let input = input_format
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    if let Some(input) = input {
                        input_checked = true;
                        if input_contradicts_plan(&source, &input) {
                            // The facts this viewer planned from do not describe
                            // the bytes it was fed: the transport's facts are
                            // stale, and the next viewer re-probes (§2.4 D5).
                            transport.mark_source_stale();
                            if let Some(owner) = manager.upgrade() {
                                owner.forget_source_facts(&facts_key);
                            }
                            break Err(LiveTvError::SourceFormatChanged(
                            "the broadcast changed format since this channel's tuner was last observed; start the channel again so a fresh delivery route can be selected".into(),
                        ));
                        }
                    }
                }
                if source_format_changed.load(Ordering::Acquire) {
                    // The terminal owner invalidates the source facts after
                    // settling stderr, including when child exit wins this tick.
                    break Err(LiveTvError::SourceFormatChanged(
                    "the broadcast changed format; start the channel again so a fresh delivery route can be selected".into(),
                ));
                }

                let observed = session
                    .source_format
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(format) =
                    observed.filter(|format| format.observed_at > recorded_source_observation)
                {
                    if let Some(owner) = manager.upgrade() {
                        let expires_at = owner.record_source_format(
                            SourceFormatKey {
                                generation: config.generation,
                                device_id: session.device_id.clone(),
                                channel_id: session.channel.id.clone(),
                            },
                            &session.channel.guide_number,
                            format.clone(),
                        );
                        session
                            .source_format_expires_at
                            .store(expires_at, Ordering::Release);
                        recorded_source_observation = format.observed_at;
                    }
                }

                let scan = inventory_scan
                    .get_or_insert_with(|| spawn_scratch_scan(manager, &session.directory));
                let scanned = match tokio::time::timeout(SESSION_TICK, &mut *scan).await {
                    Ok(joined) => {
                        inventory_scan = None;
                        Some(joined.map_err(|error| {
                            LiveTvError::StreamFailed(format!("reading live-TV scratch: {error}"))
                        }))
                    }
                    Err(_) => None,
                };
                match scanned.map(|joined| joined.and_then(|result| result)) {
                    Some(Ok(Some(inventory))) => {
                        let now = tokio::time::Instant::now();
                        let publishable = {
                            let mut state = session
                                .state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            record_producer_inventory(&mut state, inventory, now)
                        };
                        if !published && publishable && !verifying {
                            let owner = manager.upgrade().ok_or_else(|| {
                                LiveTvError::StreamFailed("live-TV manager stopped".into())
                            })?;
                            ensure_session_fence(&owner, session).await?;
                            let provisional = LiveTvProvisional {
                                outcome: LiveTvStartOutcome::Created,
                                session_id: session.capability.clone(),
                                capability: session.capability.clone(),
                                activation_token: session.activation_token.clone(),
                                channel: session.channel_with_source_format(),
                                output: session.output(),
                                delivery: session.delivery(),
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
                            session.record_codec_qualification_publication(
                                &owner.transcode,
                                admission.as_ref(),
                            );
                            session.changed.notify_waiters();
                            published = true;
                        }
                    }
                    Some(Ok(None)) | None => {}
                    Some(Err(error)) => break Err(error),
                }

                let now = tokio::time::Instant::now();
                if !published {
                    if let Some(reason) = startup_overdue(
                        session.started,
                        now,
                        session.tuner_bytes.load(Ordering::Acquire),
                        session
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .publication
                            .as_ref()
                            .map_or(0, |publication| publication.listed),
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
                    if producer_progress_overdue(published, &state, now) {
                        break Err(LiveTvError::StreamFailed(
                            "the live-TV producer stopped advancing".into(),
                        ));
                    }
                }
                if fence_check
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    let verdict = fence_check.take().expect("a finished fence check");
                    verdict.await.map_err(|error| {
                        LiveTvError::StreamFailed(format!("checking the live-TV fence: {error}"))
                    })??;
                }
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(4) {
                    // Only the ledger read is spawned. The local half (serving
                    // authority, the settings observation's age, and that
                    // observation against this start) runs here on every fence
                    // tick, so a hung ledger read cannot also suspend the
                    // observation's grace or a disable seen by the observer.
                    let owner = manager.upgrade().ok_or_else(|| {
                        LiveTvError::StreamFailed("live-TV manager stopped".into())
                    })?;
                    if session_fence_local(&owner, session)? && fence_check.is_none() {
                        fence_check = Some(spawn_session_ledger_fence(manager, session));
                    }
                }
            }
        };
        // Running work: a loss that recovers inside the session grace keeps
        // the session; one that outlasts it raises the running floor.
        let authority_lost = async {
            loop {
                if !serving.running(session.owner_serving_generation) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        // No Store or filesystem wait may delay cancellation of the upstream and
        // child. Every observer exit, including `?`, flows through the join below.
        let terminal = tokio::select! {
            biased;
            _ = session.cancel.cancelled() => Ok(None),
            _ = authority_lost => Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.into(),
            )),
            result = observe => result,
        };

        if let Some(scan) = inventory_scan.take() {
            scan.abort();
        }
        if let Some(check) = fence_check.take() {
            check.abort();
        }
        let terminal = match terminal {
            Ok(Some(_)) if !transport.reserve_seat() => {
                Err(transport.terminal_error().unwrap_or_else(|| {
                    LiveTvError::StreamFailed(
                        "the shared tuner connection closed before the start could restart".into(),
                    )
                }))
            }
            Ok(Some(probed)) => {
                // The warm plan was wrong for this tune. Stop the FFmpeg it
                // started, keep this viewer's seat on the tuner, forget the
                // cached facts, and start again from the probed ones — the cold
                // path, inside the same start budget (`session.started` is
                // unchanged). Nothing was published from the first FFmpeg.
                //
                // The seat was reserved (above) before the pump goes. Aborting
                // the pump drops this viewer's feed, the fan-out closes it at
                // its next chunk, and a transport with no live consumer and no
                // seat retires there and then. Reserved only after the reap, as
                // it once was, any reap slower than one tuner chunk — a loaded
                // host — handed the restart a closed tuner connection.
                pump.abort();
                let _ = pump.await;
                let _ = child.start_kill();
                let reaped = tokio::time::timeout(SESSION_DRAIN_TIMEOUT, child.wait()).await;
                if !matches!(reaped, Ok(Ok(_))) {
                    transport.release_seat();
                    *session.process.lock().await = Some(LiveTvProcess {
                        child,
                        sink: Some(sink),
                        _job: child_job,
                        _admission: admission,
                        stderr,
                    });
                    return Err(LiveTvError::StreamFailed(
                        "the warm start's FFmpeg exit was not confirmed before it could restart"
                            .into(),
                    ));
                }
                sink.drain().await;
                drop(sink);
                #[cfg(test)]
                linger_after_warm_reap(manager, &transport, &session.capability).await;
                if let Some(stderr) = stderr {
                    stderr.abort();
                    let _ = stderr.await;
                }
                // The job goes with this iteration; the encoder admission is
                // given back now, before the restart asks for its own.
                drop(admission);
                session.hold_seat_on(Arc::clone(&transport));
                transport.detach_viewer(&session.capability);
                remove_session_directory(&session.directory).await?;
                tokio::fs::create_dir_all(&session.directory)
                    .await
                    .map_err(|error| {
                        LiveTvError::StreamFailed(format!("creating live-TV scratch: {error}"))
                    })?;
                {
                    let mut state = session
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.publication = None;
                    state.newest_listed = None;
                    state.media_sequence = 0;
                    state.last_progress = tokio::time::Instant::now();
                    state.encoder = "pending".to_owned();
                }
                if let Some(owner) = manager.upgrade() {
                    owner.forget_source_facts(&facts_key);
                    owner.record_source_facts(
                        facts_key.clone(),
                        &session.channel.guide_number,
                        probed.clone(),
                    );
                    owner
                        .metrics
                        .observe_start_plan(StartPlanSource::WarmDisagreed);
                }
                tracing::info!(
                    channel = %session.channel.guide_number,
                    "a warm Live TV start's cached source facts did not match this tune; restarting it from the probe"
                );
                source = probed;
                verifying = false;
                continue;
            }
            other => other.map(|_| ()),
        };
        session.cancel.cancel();
        pump.abort();
        let _ = pump.await;
        *session.process.lock().await = Some(LiveTvProcess {
            child,
            sink: Some(sink),
            _job: child_job,
            _admission: admission,
            stderr,
        });
        return terminal;
    }
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
    listed: usize,
) -> Option<String> {
    if tuner_bytes == 0 {
        return (now.duration_since(started) >= STARTUP_TIMEOUT).then(|| {
            "the tuner sent no data before the start budget ran out; the channel may have \
             no signal on this device"
                .to_owned()
        });
    }
    (now.duration_since(started) >= STARTUP_FEEDING_TIMEOUT).then(|| match listed {
        0 => format!("the tuner sent {tuner_bytes} bytes but no complete live segment in time"),
        count => format!(
            "the tuner sent {tuner_bytes} bytes and {count} live segment(s), but not the \
             {STARTUP_LISTED_SEGMENTS} a player needs to start without stalling"
        ),
    })
}

fn producer_progress_overdue(
    published: bool,
    state: &LiveTvSessionState,
    now: tokio::time::Instant,
) -> bool {
    published && now.duration_since(state.last_progress) >= PRODUCER_PROGRESS_TIMEOUT
}

fn provisional_expired(state: &LiveTvSessionState, now: tokio::time::Instant) -> bool {
    !state.activated
        && state
            .provisional_at
            .is_some_and(|published_at| now.duration_since(published_at) >= PROVISIONAL_TIMEOUT)
}

/// The fence a session takes at start and again immediately before it
/// publishes its first segment: the serving generation, then a **direct**
/// consistent read.
///
/// These two fail closed on a read error, as they always have. A start has
/// just read the settings for the public request anyway, and a
/// stale-but-in-grace observation must never admit a first segment — after
/// publication the client is watching and ending the session costs it the
/// stream; before publication it costs nothing but a retry.
async fn ensure_session_fence(
    manager: &LiveTvManager,
    session: &LiveTvSession,
) -> Result<(), LiveTvError> {
    if !manager.serving.serving(session.owner_serving_generation) {
        return Err(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
        ));
    }
    let config = manager.config_read_at(SettingsReadSite::Fence).await?;
    // While nothing is live the observer is parked, so this read is the
    // observation the session's first running fence validates against.
    manager.fence.publish_unless_fresh(&config);
    validate_start_config(&config, &session.request, &manager.node_id)
}

/// The per-second fence every live session takes while it runs.
///
/// Identical to `ensure_session_fence` except for where the settings come
/// from: the node's shared observation rather than a read of this session's
/// own. The three `validate_start_config` checks are unchanged and run just as
/// often, so a fresh observation that fails them ends the session with exactly
/// the error it ended with before. What changes is the cost — one consistent
/// read per node per second instead of one per session per second — and what
/// happens when that read fails: `FENCE_GRACE_MAX_AGE` on the last validated
/// observation, then an end that names the settings store rather than the
/// tuner.
///
/// The session tick runs the two halves itself (the local half inline, the
/// ledger half on its own task); this is the same fence in one call, which
/// is how the fence tests drive it.
#[cfg(test)]
async fn ensure_session_fence_from_observation(
    manager: &LiveTvManager,
    session: &LiveTvSession,
) -> Result<(), LiveTvError> {
    if session_fence_local(manager, session)? {
        manager.resource_session_fence(session).await
    } else {
        Ok(())
    }
}

/// The half of the running fence that reads nothing: serving authority, the
/// settings observation's age (`FENCE_GRACE_MAX_AGE`) and that observation
/// against this start. `Ok(true)` means the ledger half is due.
fn session_fence_local(
    manager: &LiveTvManager,
    session: &LiveTvSession,
) -> Result<bool, LiveTvError> {
    if !manager.serving.running(session.owner_serving_generation) {
        return Err(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
        ));
    }
    if !manager.serving.is_ready() {
        // Inside the session grace nothing is served, and the authority's
        // loop ends the session if the loss outlasts it; the ledger and
        // settings checks resume once authority is back.
        return Ok(false);
    }
    let observation = manager.fence.validated()?;
    validate_start_config(&observation.config, &session.request, &manager.node_id)?;
    Ok(true)
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
                "HDHomeRun stream request failed on the session server"
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

struct LiveTunerInput {
    prefix: bytes::Bytes,
    queued: Option<bytes::Bytes>,
    remainder: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
}

/// Retain one finite prefix while keeping ownership of the same response
/// stream. A chunk crossing the byte ceiling is split and the tail becomes
/// the first queued producer chunk, so every tuner byte is replayed once.
async fn collect_live_prefix(
    response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<LiveTunerInput, LiveTvError> {
    let mut remainder = response.bytes_stream().boxed();
    let first = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return Err(LiveTvError::CapabilityExpired(
                "the live-TV start was cancelled during source observation".into(),
            ));
        }
        first = tokio::time::timeout(TUNER_READ_TIMEOUT, remainder.next()) => first,
    }
    .map_err(|_| LiveTvError::StreamFailed("the HDHomeRun stream stopped producing bytes".into()))?
    .ok_or_else(|| LiveTvError::StreamFailed("the HDHomeRun stream ended".into()))?
    .map_err(|_| LiveTvError::StreamFailed("the HDHomeRun stream body failed".into()))?;

    let mut prefix = Vec::with_capacity(SOURCE_PREFIX_BYTES.min(first.len().saturating_mul(4)));
    let mut queued = None;
    if first.len() > SOURCE_PREFIX_BYTES {
        prefix.extend_from_slice(&first[..SOURCE_PREFIX_BYTES]);
        queued = Some(first.slice(SOURCE_PREFIX_BYTES..));
    } else {
        prefix.extend_from_slice(&first);
    }
    let deadline = tokio::time::Instant::now() + SOURCE_PREFIX_TIME;
    while prefix.len() < SOURCE_PREFIX_BYTES && queued.is_none() {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(LiveTvError::CapabilityExpired(
                    "the live-TV start was cancelled during source observation".into(),
                ));
            }
            next = tokio::time::timeout_at(deadline, remainder.next()) => next,
        };
        let Ok(next) = next else {
            break;
        };
        let Some(bytes) = next else {
            return Err(LiveTvError::StreamFailed(
                "the HDHomeRun stream ended during source observation".into(),
            ));
        };
        let bytes = bytes
            .map_err(|_| LiveTvError::StreamFailed("the HDHomeRun stream body failed".into()))?;
        let remaining = SOURCE_PREFIX_BYTES - prefix.len();
        if bytes.len() > remaining {
            prefix.extend_from_slice(&bytes[..remaining]);
            queued = Some(bytes.slice(remaining..));
        } else {
            prefix.extend_from_slice(&bytes);
        }
    }
    Ok(LiveTunerInput {
        prefix: bytes::Bytes::from(prefix),
        queued,
        remainder,
    })
}

fn parse_rational(value: Option<&str>) -> Option<crate::live_tv_delivery::LiveRational> {
    let (num, den) = value?.split_once('/')?;
    let num = num.parse::<u32>().ok()?;
    let den = den.parse::<u32>().ok()?;
    (num > 0 && den > 0).then_some(crate::live_tv_delivery::LiveRational { num, den })
}

fn json_string(object: &serde_json::Value, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_CHANNEL_FORMAT_BYTES)
        .map(str::to_ascii_lowercase)
}

fn parse_probe_facts(bytes: &[u8]) -> Result<LiveSourceFacts, LiveTvError> {
    let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
        LiveTvError::InvalidResponse(
            "source_probe_incomplete: FFprobe returned invalid JSON".into(),
        )
    })?;
    let streams = document
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            LiveTvError::InvalidResponse(
                "source_probe_incomplete: FFprobe returned no streams".into(),
            )
        })?;
    let video = streams
        .iter()
        .find(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })
        .ok_or_else(|| {
            LiveTvError::InvalidResponse(
                "source_probe_incomplete: no video stream was observed".into(),
            )
        })?;
    let audio_tracks: Vec<crate::live_tv_delivery::LiveSourceAudioTrack> = streams
        .iter()
        .filter(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
        })
        .take(usize::from(u8::MAX))
        .enumerate()
        .map(
            |(index, audio)| crate::live_tv_delivery::LiveSourceAudioTrack {
                index: u8::try_from(index).unwrap_or(u8::MAX),
                // FFprobe prints the MPEG-TS PID as `"0x33"`.
                id: audio
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| {
                        u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
                    })
                    .filter(|value| *value > 0),
                codec: json_string(audio, "codec_name"),
                sample_rate: audio
                    .get("sample_rate")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|value| *value > 0),
                channels: audio
                    .get("channels")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok())
                    .filter(|value| *value > 0),
                layout: json_string(audio, "channel_layout"),
                language: audio
                    .get("tags")
                    .and_then(|tags| json_string(tags, "language")),
                described: audio.get("disposition").is_some_and(|disposition| {
                    [
                        "visual_impaired",
                        "hearing_impaired",
                        "comment",
                        "descriptions",
                    ]
                    .into_iter()
                    .any(|flag| {
                        disposition
                            .get(flag)
                            .and_then(serde_json::Value::as_i64)
                            .is_some_and(|value| value != 0)
                    })
                }),
            },
        )
        .collect();
    let first_audio = audio_tracks.first().ok_or_else(|| {
        LiveTvError::InvalidResponse("source_probe_incomplete: no audio stream was observed".into())
    })?;
    let pixel_format = json_string(video, "pix_fmt");
    let bit_depth = video
        .get("bits_per_raw_sample")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|value| (8..=16).contains(value))
        .or_else(|| {
            pixel_format.as_deref().and_then(|format| {
                if matches!(format, "yuv420p" | "nv12") {
                    return Some(8);
                }
                [16u8, 14, 12, 10, 9, 8]
                    .into_iter()
                    .find(|depth| format.contains(&depth.to_string()))
            })
        });
    let color_transfer = json_string(video, "color_transfer");
    let hdr = match color_transfer.as_deref() {
        Some("smpte2084") => Some("pq".to_owned()),
        Some("arib-std-b67") => Some("hlg".to_owned()),
        Some("bt709") | Some("iec61966-2-1") => Some("sdr".to_owned()),
        _ => None,
    };
    Ok(LiveSourceFacts {
        video_codec: json_string(video, "codec_name"),
        video_profile: json_string(video, "profile").map(|value| value.replace(' ', "")),
        video_level: video
            .get("level")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        width: video
            .get("width")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        height: video
            .get("height")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        pixel_format,
        bit_depth,
        field_order: json_string(video, "field_order"),
        frame_rate: parse_rational(
            video
                .get("avg_frame_rate")
                .and_then(serde_json::Value::as_str),
        ),
        sample_aspect_ratio: json_string(video, "sample_aspect_ratio"),
        color_primaries: json_string(video, "color_primaries"),
        color_transfer,
        color_space: json_string(video, "color_space"),
        color_range: json_string(video, "color_range"),
        rotation: live_probe_rotation(video),
        hdr,
        audio_codec: first_audio.codec.clone(),
        audio_sample_rate: first_audio.sample_rate,
        audio_channels: first_audio.channels,
        audio_layout: first_audio.layout.clone(),
        audio_tracks,
    })
}

/// A complete stream probe can establish the absence of a rotation transform.
/// A present but unreadable transform stays unknown and cannot admit a native graph.
fn live_probe_rotation(video: &serde_json::Value) -> Option<i16> {
    let side_data = video
        .get("side_data_list")
        .and_then(serde_json::Value::as_array);
    if let Some(matrix) = side_data.and_then(|entries| {
        entries.iter().find(|entry| {
            entry
                .get("side_data_type")
                .and_then(serde_json::Value::as_str)
                == Some("Display Matrix")
        })
    }) {
        return matrix
            .get("rotation")
            .and_then(serde_json::Value::as_i64)
            .and_then(|rotation| i16::try_from(rotation).ok());
    }
    if let Some(rotation) = video.get("tags").and_then(|tags| tags.get("rotate")) {
        return rotation
            .as_str()
            .and_then(|value| value.parse::<i16>().ok());
    }
    Some(0)
}

/// Who waits on a source probe, as one value: the child's priority class and
/// the probe's budget travel together so a caller cannot pair a background
/// class with a viewer's two-second budget (review 76, P2-1), or the reverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceProbeWork {
    class: crate::process_control::ChildClass,
    budget: Duration,
}

impl SourceProbeWork {
    /// A viewer's start or a DVR transport is waiting on the probe.
    const VIEWER: Self = Self {
        class: crate::process_control::ChildClass::Realtime,
        budget: SOURCE_PROBE_TIME,
    };
    /// Nobody is waiting: the boot caption self-test. Background class, and
    /// the readiness probe's budget, because a loaded boot can hold a nice-15
    /// child well past a viewer's two seconds.
    const SELF_TEST: Self = Self {
        class: crate::process_control::ChildClass::Background,
        budget: READINESS_PROBE_TIME,
    };
}

/// Probe a retained tuner prefix with the configured ffprobe.
///
/// `work` is the caller's, as the other dual-use probe helpers take their
/// class from the caller (`ChildWork::new(class, ..)`): a viewer's start and a
/// DVR transport pass [`SourceProbeWork::VIEWER`]; the boot caption probe
/// reuses the same helper through the production graph path with nobody
/// waiting and passes [`SourceProbeWork::SELF_TEST`].
async fn probe_live_source(
    system: &SystemInfo,
    directory: &Path,
    prefix: &[u8],
    work: SourceProbeWork,
) -> Result<LiveSourceFacts, LiveTvError> {
    if system.ffprobe.trim().is_empty() {
        return Err(LiveTvError::CodecUnsupported(
            "source_probe_incomplete: FFprobe is not configured on the session server".into(),
        ));
    }
    let sample = directory.join("source-probe.ts");
    let _cache_pin = crate::playback_cache::pin(&sample).map_err(|error| {
        LiveTvError::StreamFailed(format!("pinning bounded source probe: {error}"))
    })?;
    let reservation = crate::playback_cache::reserve(&sample, prefix.len() as u64)
        .await
        .map_err(|error| {
            LiveTvError::StreamFailed(format!("reserving bounded source probe: {error}"))
        })?;
    tokio::fs::write(&sample, prefix).await.map_err(|error| {
        LiveTvError::StreamFailed(format!("writing bounded source probe: {error}"))
    })?;
    if let Some(reservation) = reservation {
        reservation.commit(prefix.len() as u64);
    }
    let mut command = tokio::process::Command::new(&system.ffprobe);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .args([
            "-v",
            "error",
            "-probesize",
            "8388608",
            "-analyzeduration",
            "2000000",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&sample)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::new(work.class, "Live TV source probe"),
    )
    .map_err(|error| {
        LiveTvError::CodecUnsupported(format!("starting bounded source probe: {error}"))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        LiveTvError::StreamFailed("bounded source probe did not expose stdout".into())
    })?;
    let probe = async {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_SOURCE_PROBE_JSON_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| LiveTvError::StreamFailed(format!("reading source probe: {error}")))?;
        let status = child.wait().await.map_err(|error| {
            LiveTvError::StreamFailed(format!("waiting for source probe: {error}"))
        })?;
        if !status.success() {
            return Err(LiveTvError::InvalidResponse(
                "source_probe_incomplete: FFprobe could not identify the retained prefix".into(),
            ));
        }
        if bytes.len() > MAX_SOURCE_PROBE_JSON_BYTES {
            return Err(LiveTvError::InvalidResponse(
                "source_probe_incomplete: FFprobe JSON exceeded 256 KiB".into(),
            ));
        }
        parse_probe_facts(&bytes)
    };
    let result = match tokio::time::timeout(work.budget, probe).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(LiveTvError::StartupTimeout(format!(
                "source_probe_incomplete: FFprobe exceeded its {} s budget",
                work.budget.as_secs()
            )))
        }
    };
    let remove = tokio::fs::remove_file(&sample).await;
    if let Err(error) = remove {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(LiveTvError::StreamFailed(format!(
                "removing bounded source probe: {error}"
            )));
        }
    }
    result
}

/// A tuner stream cannot expose codec/profile facts until after its HTTP body
/// is flowing. Freeze the complete route that is knowable before spawn and
/// state the remaining input contract explicitly: software auto-detection of
/// the first video stream. This keeps Live TV out of the movie-plan cache
/// namespace without leaving a command-construction bypass that re-infers the
/// encoder or decoder route.
#[derive(Debug, Clone)]
struct LiveTvTranscodePlan {
    delivery: LiveDeliveryPlan,
    encoder: Option<Encoder>,
    software_threads: Option<u32>,
    video_map: &'static str,
    force_idr: bool,
    macos_upload: Option<LiveMacosUpload>,
}

#[derive(Debug, Clone)]
struct LiveMacosUpload {
    graph: MacosProcessingGraph,
    identity: MacosProcessingIdentity,
    executable: crate::ffmpeg::EncodedExecutable,
}

impl LiveTvTranscodePlan {
    fn new(
        system: &SystemInfo,
        delivery: LiveDeliveryPlan,
        encoder: Option<Encoder>,
        software_threads: Option<u32>,
    ) -> Result<Self, LiveTvError> {
        if system.ffmpeg.trim().is_empty() {
            return Err(LiveTvError::CodecUnsupported(
                "FFmpeg is not configured on the session server".into(),
            ));
        }
        if delivery.output.height == 0
            || software_threads == Some(0)
            || (delivery.video_action == LiveTrackAction::Encode && encoder.is_none())
            || (delivery.video_action == LiveTrackAction::Copy && encoder.is_some())
        {
            return Err(LiveTvError::CodecUnsupported(
                "the live-TV transcode execution contract is invalid".into(),
            ));
        }
        Ok(Self {
            delivery,
            encoder,
            software_threads,
            video_map: "0:v:0",
            macos_upload: None,
            force_idr: encoder.is_some_and(|encoder| system.encoders.forced_idr.wanted_by(encoder)),
        })
    }
    async fn bind_macos_upload(
        &mut self,
        system: &SystemInfo,
        context: Option<&MacosProcessingContext>,
    ) {
        let Some(context) = context.filter(|context| context.enabled()) else {
            return;
        };
        let Some(graph) = live_macos_upload_graph(self) else {
            return;
        };
        if context.graph(graph) != MacosProcessingAvailability::Available {
            return;
        }
        let Ok(Ok(executable)) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            crate::ffmpeg::EncodedExecutable::capture_program(&system.ffmpeg),
        )
        .await
        else {
            return;
        };
        if executable.digest != context.identity().ffmpeg_sha256()
            || !tokio::time::timeout(CONNECT_TIMEOUT, executable.is_current())
                .await
                .unwrap_or(false)
        {
            return;
        }
        self.macos_upload = Some(LiveMacosUpload {
            graph,
            identity: context.identity().clone(),
            executable,
        });
    }

    async fn verify_macos_upload(&self) -> Result<(), LiveTvError> {
        if let Some(binding) = &self.macos_upload {
            if binding.executable.digest != binding.identity.ffmpeg_sha256()
                || !tokio::time::timeout(CONNECT_TIMEOUT, binding.executable.is_current())
                    .await
                    .unwrap_or(false)
            {
                return Err(LiveTvError::CodecUnsupported(
                    "the selected live-TV processing executable changed before launch".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Upload graphs describe software decode and native processing, not hardware decode.
/// Older or incomplete probes keep the incumbent route.
fn live_macos_upload_graph(plan: &LiveTvTranscodePlan) -> Option<MacosProcessingGraph> {
    let source = &plan.delivery.source;
    if plan.encoder != Some(Encoder::VideoToolbox)
        || plan.delivery.video_action != LiveTrackAction::Encode
        || source.rotation != Some(0)
        || source.sample_aspect_ratio.as_deref() != Some("1:1")
        || source.color_range.as_deref() != Some("tv")
        || source.color_primaries.as_deref() != Some("bt709")
        || source.color_transfer.as_deref() != Some("bt709")
        || source.color_space.as_deref() != Some("bt709")
        || source.hdr.as_deref() != Some("sdr")
    {
        return None;
    }
    let (width, height) = (source.width?, source.height?);
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    let rate = source.frame_rate?;
    if rate.num == 0 || rate.den == 0 {
        return None;
    }
    let codec = source.video_codec.as_deref()?;
    let depth = source.bit_depth?;
    let layout = source.pixel_format.as_deref()?;
    let eight_bit = depth == 8 && matches!(layout, "yuv420p" | "nv12");
    let ten_bit = depth == 10 && matches!(layout, "yuv420p10le" | "p010le");
    let graph = match (
        source.field_order.as_deref()?,
        plan.delivery.deinterlace_output,
    ) {
        ("progressive", None)
            if (eight_bit && matches!(codec, "h264" | "mpeg2video" | "hevc"))
                || (ten_bit && codec == "hevc") =>
        {
            MacosProcessingGraph::LiveSdrUploadScale
        }
        ("tt" | "bb", Some(LiveDeinterlaceOutput::Frame))
            if eight_bit && matches!(codec, "h264" | "mpeg2video") =>
        {
            MacosProcessingGraph::LiveSdrUploadBwdifFrame
        }
        ("tt" | "bb", Some(LiveDeinterlaceOutput::Field))
            if eight_bit && matches!(codec, "h264" | "mpeg2video") =>
        {
            MacosProcessingGraph::LiveSdrUploadBwdifField
        }
        _ => return None,
    };
    graph.live_upload_filter(
        u32::from(plan.delivery.output.width),
        u32::from(plan.delivery.output.height),
    )?;
    Some(graph)
}

#[cfg(test)]
fn spawn_live_ffmpeg(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
) -> Result<(tokio::process::Child, crate::process_control::ChildJob), LiveTvError> {
    let mut command = live_ffmpeg_command(system, plan, directory)?;
    crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("Live TV stream"),
    )
    .map_err(|error| LiveTvError::CodecUnsupported(format!("starting live-TV FFmpeg: {error}")))
}

fn live_ffmpeg_command(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
) -> Result<tokio::process::Command, LiveTvError> {
    live_ffmpeg_command_for_input(system, plan, directory, LiveTvFfmpegInput::Tuner)
}

#[derive(Clone, Copy)]
enum LiveTvFfmpegInput {
    Tuner,
    GraphProbe,
}

fn live_ffmpeg_command_for_input(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
    input: LiveTvFfmpegInput,
) -> Result<tokio::process::Command, LiveTvError> {
    live_ffmpeg_command_for_output(system, plan, directory, input, None)
}

fn live_ffmpeg_command_for_output(
    system: &SystemInfo,
    plan: &LiveTvTranscodePlan,
    directory: &Path,
    input: LiveTvFfmpegInput,
    upload: Option<&str>,
) -> Result<tokio::process::Command, LiveTvError> {
    let playlist = upload.map_or_else(
        || directory.join("index.m3u8").to_string_lossy().into_owned(),
        |base| format!("{base}/index.m3u8"),
    );
    let extension = match plan.delivery.packaging {
        LivePackaging::Mpegts => "ts",
        LivePackaging::Fmp4 => "m4s",
    };
    let segments = upload.map_or_else(
        || {
            directory
                .join(format!("segment-%06d.{extension}"))
                .to_string_lossy()
                .into_owned()
        },
        |base| format!("{base}/segment-%06d.{extension}"),
    );
    let program = plan.macos_upload.as_ref().map_or_else(
        || Path::new(&system.ffmpeg),
        |binding| binding.executable.path.as_path(),
    );
    let mut command = tokio::process::Command::new(program);
    command
        // Tuner bytes are untrusted media input. Do not hand their decoder the
        // daemon's database, API, object-store, or deployment credentials just
        // because those values happen to live in the parent environment.
        .env_clear()
        .env("LC_ALL", "C")
        // Driver caches write outside HLS's owned receiver. Disable those
        // optional writes so every persistent playback byte needs a grant.
        .env("CUDA_CACHE_DISABLE", "1")
        .env("MESA_SHADER_CACHE_DISABLE", "true")
        .args(["-hide_banner", "-loglevel", "info", "-nostdin", "-y"]);
    if let Some(binding) = &plan.macos_upload {
        command.args(
            binding
                .graph
                .live_upload_init_args()
                .expect("a frozen live upload graph"),
        );
    } else if let Some(encoder) = plan.encoder {
        command.args(encoder.init_args());
    }
    command
        .args(["-hwaccel", "none"])
        .args(["-fflags", "+genpts+discardcorrupt"])
        .args(if plan.delivery.audio_action == LiveTrackAction::Copy {
            // Copy needs complete codec parameters before the muxer header;
            // the retained source probe does not configure this new demuxer.
            ["-probesize", "2097152", "-analyzeduration", "2000000"]
        } else {
            live_probe_args()
        });
    let audio_map = match input {
        LiveTvFfmpegInput::Tuner => {
            command.args(["-i", "pipe:0"]);
            plan.delivery
                .source
                .audio_tracks
                .iter()
                .find(|track| track.index == plan.delivery.audio_track)
                .map_or_else(
                    || format!("0:a:{}", plan.delivery.audio_track),
                    crate::live_tv_delivery::LiveSourceAudioTrack::map_specifier,
                )
        }
        LiveTvFfmpegInput::GraphProbe => {
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
            "1:a:0".to_owned()
        }
    };
    command.args(["-map", plan.video_map, "-map", &audio_map, "-sn", "-dn"]);
    match plan.delivery.video_action {
        LiveTrackAction::Copy => {
            command.args(["-c:v", "copy"]);
            if plan.delivery.output.video_codec == "hevc" {
                command.args(["-tag:v", "hvc1"]);
            }
        }
        LiveTrackAction::Encode => {
            let encoder = plan.encoder.expect("an encoded route has an encoder");
            let filter = if let Some(binding) = &plan.macos_upload {
                binding
                    .graph
                    .live_upload_filter(
                        u32::from(plan.delivery.output.width),
                        u32::from(plan.delivery.output.height),
                    )
                    .expect("the frozen live graph has a valid raster")
            } else {
                live_video_filter(
                    encoder,
                    plan.delivery
                        .source
                        .height
                        .unwrap_or(plan.delivery.output.height),
                    plan.delivery.output.height,
                    plan.delivery.deinterlace_output,
                )
            };
            command.args(["-vf", &filter]);
            command.args([
                "-force_key_frames",
                "expr:gte(t,n_forced*1)",
                "-g",
                "120",
                "-keyint_min",
                "1",
            ]);
            command.args(encoder.encode_args(
                live_video_bitrate_kbps(&plan.delivery),
                EffectiveRateControl::Vbr,
                plan.force_idr,
                plan.software_threads,
            ));
            command.args(live_caption_args(encoder));
        }
    }
    match plan.delivery.audio_action {
        LiveTrackAction::Copy => {
            command.args(["-c:a", "copy"]);
        }
        LiveTrackAction::Encode => {
            // `-ac N` would upmix a stereo broadcast to a 5.1 ceiling and
            // could not be chosen at all for a track whose layout the probe
            // never learned. The layout list lets FFmpeg keep the decoded
            // layout when it fits the ceiling and fold only what exceeds it.
            let layouts = format!(
                "aformat=channel_layouts={}",
                crate::live_tv_delivery::encoded_channel_layouts(
                    plan.delivery.output.audio_channels
                )
            );
            let bitrate = if plan.delivery.output.audio_channels > 2 {
                "384k"
            } else {
                "192k"
            };
            command.args(["-c:a", "aac", "-b:a", bitrate, "-af", &layouts]);
        }
    }
    command.args(LIVE_HLS_OUTPUT_ARGS);
    let hls_flags = match (
        upload.is_some(),
        plan.delivery.video_action == LiveTrackAction::Encode,
    ) {
        (true, true) => "delete_segments+independent_segments+omit_endlist",
        (true, false) => "delete_segments+omit_endlist",
        (false, true) => "delete_segments+temp_file+independent_segments+omit_endlist",
        (false, false) => "delete_segments+temp_file+omit_endlist",
    };
    if upload.is_some() {
        command.args(["-method", "PUT", "-http_proxy", ""]);
    }
    command.args(["-hls_flags", hls_flags]);
    if plan.delivery.packaging == LivePackaging::Fmp4 {
        command.args([
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init.mp4",
        ]);
    }
    command
        .arg("-hls_segment_filename")
        .arg(segments)
        .arg(playlist)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Keep only runtime selectors a packaged GPU stack may genuinely need.
    // Loader paths are explicit exceptions; proxy, home and credential
    // variables stay absent.
    for name in [
        "PATH",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "LIBVA_DRIVER_NAME",
        "LIBVA_DRIVERS_PATH",
        "VDPAU_DRIVER",
        "CUDA_VISIBLE_DEVICES",
        "NVIDIA_VISIBLE_DEVICES",
        "NVIDIA_DRIVER_CAPABILITIES",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
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

fn live_video_bitrate_kbps(plan: &LiveDeliveryPlan) -> u32 {
    let base = match plan.output.height {
        0..=480 => 2_000u64,
        481..=720 => 4_000,
        721..=1080 => 8_000,
        _ => 20_000,
    };
    let frame_scaled = plan.output.frame_rate.map_or(base, |rate| {
        let numerator = u64::from(rate.num);
        let denominator = u64::from(rate.den).max(1);
        if numerator < denominator.saturating_mul(24) {
            base.saturating_mul(24) / 30
        } else if numerator > denominator.saturating_mul(60) {
            base.saturating_mul(60) / 30
        } else {
            base.saturating_mul(numerator) / denominator.saturating_mul(30).max(1)
        }
    });
    let bounded = plan
        .max_bitrate_bps
        .map_or(frame_scaled, |limit| frame_scaled.min(limit / 1_000));
    u32::try_from(bounded.max(500)).unwrap_or(u32::MAX)
}

fn live_video_filter(
    encoder: Encoder,
    source_height: u16,
    output_height: u16,
    deinterlace: Option<LiveDeinterlaceOutput>,
) -> String {
    let mut filters = Vec::new();
    if let Some(output) = deinterlace {
        filters.push(format!(
            "bwdif=mode=send_{}:parity=auto:deint=interlaced",
            output.as_str()
        ));
    }
    if output_height < source_height {
        filters.push(format!("scale=-2:{output_height}"));
    }
    match encoder {
        // ATSC 3.0 HEVC Main 10 reaches this graph as P010. Uploading it
        // unchanged creates a 10-bit QSV surface, which h264_qsv refuses.
        // Convert to the live H.264 contract's 8-bit format before upload.
        Encoder::Qsv => {
            filters.push("format=nv12".to_owned());
            filters.push(
                encoder
                    .filter_suffix()
                    .expect("QSV always has a hardware-upload suffix")
                    .to_owned(),
            );
        }
        _ => match encoder.filter_suffix() {
            // VAAPI already converts to nv12 before its hardware upload.
            Some(suffix) => {
                filters.push(suffix.to_owned());
            }
            // Nothing else pins one, and the source's own format survives the
            // chain. Live TV's output is SDR 8-bit H.264 by contract -- the
            // software encoder pins `-profile:v high` -- so a 10-bit broadcast
            // reaching libx264 as yuv420p10le makes x264 refuse the profile
            // outright. Converting here keeps one output contract for every
            // client.
            None => filters.push("format=yuv420p".to_owned()),
        },
    }
    filters.join(",")
}

fn live_caption_args(encoder: Encoder) -> &'static [&'static str] {
    match encoder {
        // MPEG-2 broadcasts carry A/53 captions as frame side data. FFmpeg's
        // VideoToolbox SEI insertion can reject them with AVERROR_INVALIDDATA
        // and kill otherwise valid live video. Live captions are unsupported;
        // disable this encoder-only forwarding, leaving copy routes intact.
        Encoder::VideoToolbox => &["-a53cc", "0"],
        _ => &[],
    }
}

fn live_encoder_diagnostic(bytes: &[u8]) -> Option<&'static str> {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.contains("unexpected end of sei nal unit parsing") {
        Some("VideoToolbox failed while inserting A/53 captions into H.264 SEI data")
    } else if text.contains("unsupported channel layout") {
        Some("FFmpeg rejected the requested audio channel layout")
    } else if text.contains("neither number of channels nor channel layout specified") {
        Some("FFmpeg could not initialize live audio because the input channel layout was unknown")
    } else if text.contains("sample rate not set") {
        Some("FFmpeg could not mux live audio because the input sample rate was unknown")
    } else {
        None
    }
}

fn live_source_parameters_changed(text: &str) -> bool {
    text.contains("new video stream")
        || text.contains("new audio stream")
        || text.contains("parameter change")
        || text.contains("reconfiguring filter graph because video parameters changed to ")
}

async fn capture_live_stderr(
    mut stderr: impl tokio::io::AsyncRead + Unpin,
    decoder_unavailable: Arc<AtomicBool>,
    encoder_diagnostic: Arc<StdMutex<Option<&'static str>>>,
    source_format: Arc<StdMutex<Option<LiveTvSourceFormat>>>,
    source_format_expires_at: Arc<AtomicI64>,
    source_format_changed: Arc<AtomicBool>,
    input_format: Arc<StdMutex<Option<LiveTvSourceFormat>>>,
) {
    // Drain forever without exposing device metadata. Retain only FFmpeg's
    // bounded initial input description and stop parsing at Stream mapping;
    // output facts must never be mistaken for source facts.
    let mut window = Vec::with_capacity(2048);
    let mut descriptor = Vec::with_capacity(MAX_SOURCE_DESCRIPTOR_BYTES);
    let mut descriptor_done = false;
    let mut diagnostic_window = Vec::with_capacity(2048);
    let mut diagnostic_found = false;
    let mut chunk = [0; 1024];
    while let Ok(read) = stderr.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        // A fast failure may share a read with the initial descriptor and
        // stream mapping. Recognize it before the format parser skips that
        // chunk, retaining only the first static cause, never raw metadata.
        if !diagnostic_found {
            diagnostic_window.extend_from_slice(&chunk[..read]);
            if let Some(cause) = live_encoder_diagnostic(&diagnostic_window) {
                encoder_diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert(cause);
                diagnostic_found = true;
                diagnostic_window.clear();
            } else if diagnostic_window.len() > 2048 {
                diagnostic_window.drain(..diagnostic_window.len() - 2048);
            }
        }
        let descriptor_was_done = descriptor_done;
        if !descriptor_done {
            let remaining = MAX_SOURCE_DESCRIPTOR_BYTES.saturating_sub(descriptor.len());
            descriptor.extend_from_slice(&chunk[..read.min(remaining)]);
            let boundary = String::from_utf8_lossy(&descriptor)
                .find("Stream mapping:")
                .or_else(|| String::from_utf8_lossy(&descriptor).find("Output #0"));
            if let Some(boundary) = boundary {
                descriptor.truncate(boundary);
                descriptor_done = true;
            } else if descriptor.len() == MAX_SOURCE_DESCRIPTOR_BYTES {
                descriptor_done = true;
            }
            if descriptor_done {
                if let Some(format) = parse_live_source_format(&descriptor, unix_seconds()) {
                    source_format_expires_at.store(
                        format
                            .observed_at
                            .saturating_add(SOURCE_FORMAT_TTL.as_secs() as i64),
                        Ordering::Release,
                    );
                    *input_format
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format.clone());
                    *source_format
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format);
                }
                descriptor.clear();
            }
        }
        if !descriptor_was_done && descriptor_done {
            // The boundary chunk can still contain initial MPEG-TS discovery
            // diagnostics. Establish an empty post-start baseline so only a
            // later stream announcement can invalidate the frozen plan.
            window.clear();
            continue;
        }
        window.extend_from_slice(&chunk[..read]);
        if window.len() > 2048 {
            window.drain(..window.len() - 2048);
        }
        let text = String::from_utf8_lossy(&window).to_ascii_lowercase();
        if descriptor_done && live_source_parameters_changed(&text) {
            source_format_changed.store(true, Ordering::Release);
        }
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
    if !descriptor_done {
        if let Some(format) = parse_live_source_format(&descriptor, unix_seconds()) {
            source_format_expires_at.store(
                format
                    .observed_at
                    .saturating_add(SOURCE_FORMAT_TTL.as_secs() as i64),
                Ordering::Release,
            );
            *input_format
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format.clone());
            *source_format
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format);
        }
    }
}

/// Drain this viewer's queue into its own FFmpeg. The only wait on a slow
/// FFmpeg is here, per viewer: the transport's reader never waits on it, and
/// if this falls `VIEWER_QUEUE_BYTES` behind the transport evicts this viewer
/// alone (plan L-03 §3.2). The write keeps `TUNER_READ_TIMEOUT`, so an FFmpeg
/// that stops reading mid-chunk still ends its session.
async fn pump_viewer_feed(
    mut feed: dvr::ViewerFeed,
    mut stdin: tokio::process::ChildStdin,
    cancel: CancellationToken,
    delivered: Arc<AtomicU64>,
    failure: Arc<StdMutex<Option<LiveTvError>>>,
) {
    let result = async {
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                next = feed.rx.recv() => next,
            };
            // A closed feed is the transport's decision (an eviction, a fence,
            // its own end); the session's observer reads why.
            let Some(bytes) = next else {
                return Ok(());
            };
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                result = tokio::time::timeout(TUNER_READ_TIMEOUT, stdin.write_all(&bytes)) => {
                    result.map_err(|_| LiveTvError::StreamFailed("FFmpeg input stalled".into()))?
                        .map_err(|error| LiveTvError::StreamFailed(format!("writing FFmpeg input: {error}")))?;
                }
            }
            feed.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            // Counted after the write, so it means "reached the graph" rather
            // than "arrived in this process".
            delivered.fetch_add(len, Ordering::Release);
        }
    }
    .await;
    if let Err(error) = result {
        *failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }
}

/// Why a viewer's feed ended, most specific first: an eviction the transport
/// chose, then this viewer's own write failure, then how the transport itself
/// ended. The eviction comes first because it is the cause: a pump blocked on
/// an FFmpeg that stopped reading fails "input stalled" only as a consequence
/// of the backlog the transport already evicted it for.
fn viewer_stream_error(
    pump_failure: &StdMutex<Option<LiveTvError>>,
    consumer: &dvr::ViewerConsumer,
    transport: &dvr::DvrTransport,
) -> LiveTvError {
    match consumer.evicted() {
        Some("backlog") => {
            return LiveTvError::StreamFailed(
                "the live-TV producer stopped consuming tuner bytes".into(),
            );
        }
        Some(_) => {
            return LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            );
        }
        None => {}
    }
    if let Some(error) = pump_failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return error;
    }
    transport
        .terminal_error()
        .unwrap_or_else(|| LiveTvError::StreamFailed("the tuner stream ended unexpectedly".into()))
}

/// The quality policy a start plans under: the configured ceiling for a
/// client that sent its capabilities, and the exact old H.264/AAC height
/// profile for a mixed-version start that did not.
fn live_quality_policy(config: &LiveTvConfig, capabilities_sent: bool) -> LiveQualityPolicy {
    if capabilities_sent {
        LiveQualityPolicy {
            max_height: (config.max_output_height > 0).then_some(config.max_output_height),
            max_bitrate_bps: None,
            deinterlace_output: config.deinterlace_output,
        }
    } else {
        LiveQualityPolicy {
            max_height: Some(config.output_height),
            max_bitrate_bps: None,
            deinterlace_output: config.deinterlace_output,
        }
    }
}

fn live_execution_support(system: &SystemInfo) -> LiveExecutionSupport {
    LiveExecutionSupport {
        video_encode: !system.ffmpeg.trim().is_empty(),
        audio_encode: !system.ffmpeg.trim().is_empty(),
        // The VOD probe does not prove this live command applies that
        // graph. Reject required HDR conversion honestly; compatible HDR
        // copy routes remain available.
        tone_map: false,
    }
}

/// The source format a session publishes for what it planned from: the
/// picture and scan of `source`, and the audio of the track `delivery`
/// selected.
fn session_source_format(
    source: &LiveSourceFacts,
    delivery: &LiveDeliveryPlan,
) -> LiveTvSourceFormat {
    LiveTvSourceFormat {
        video_width: source.width,
        video_height: source.height,
        scan: source.field_order.as_deref().map(|order| {
            if matches!(order, "progressive" | "unknown") {
                "progressive".to_owned()
            } else {
                "interlaced".to_owned()
            }
        }),
        audio_channels: delivery.source.audio_channels,
        audio_layout: delivery.source.audio_layout.clone(),
        observed_at: unix_seconds(),
    }
}

/// The delivery to publish for `delivery`: with a `captions_advertised`
/// reason when this node's caption probe proved the exact graph it runs.
/// The fixtures cover 1080i and 720p MPEG-2 with AC-3. Only a matching source
/// graph can use its proof; H.264 copy and other source families stay
/// unadvertised until their own production graph has been proven on this
/// FFmpeg build.
///
/// The reason lists only the proved services this viewer's player decodes
/// (`decoders`, `LivePlaybackRequest::caption_decoders`); `None` lists them
/// all, as before the claim existed. hls.js has no CEA-708 decoder and maps
/// `SERVICE1` onto the text track it made for `CC1`, so advertising both to it
/// produced one phantom track labelled "English 708".
fn advertise_proven_captions(
    proofs: &[caption_probe::CaptionProof],
    source: &LiveSourceFacts,
    mut delivery: LiveDeliveryPlan,
    encoder: Option<&str>,
    decoders: Option<&[String]>,
) -> LiveDeliveryPlan {
    if source.video_codec.as_deref() == Some("mpeg2video")
        && delivery.video_action == LiveTrackAction::Encode
    {
        if let Some(proof) = proofs.iter().find(|proof| {
            Some(proof.encoder.as_str()) == encoder
                && proof.packaging == delivery.packaging
                && proof.deinterlace == delivery.deinterlace_output
                && source.height == Some(proof.source_height)
                && proof.output_height == delivery.output.height
        }) {
            let services = proof
                .services
                .iter()
                .copied()
                .filter(|service| caption_service_decodable(service, decoders))
                .collect::<Vec<_>>();
            if !services.is_empty() {
                delivery
                    .reasons
                    .push(crate::live_tv_delivery::LiveDeliveryReason {
                        code: "captions_advertised".to_owned(),
                        explanation: services.join(","),
                    });
            }
        }
    }
    delivery
}

/// Whether a player that decodes `decoders` can show `service`: `CC1`–`CC4`
/// are CEA-608 channels, `SERVICE1`–`SERVICE63` CEA-708 services.
fn caption_service_decodable(service: &str, decoders: Option<&[String]>) -> bool {
    let Some(decoders) = decoders else {
        return true;
    };
    let format = if service.starts_with("CC") {
        "cea608"
    } else if service.starts_with("SERVICE") {
        "cea708"
    } else {
        return false;
    };
    decoders.iter().any(|decoder| decoder == format)
}

/// Plan L-02 §3.3: whether the FFmpeg a warm start is already running — the
/// one `running` describes, planned from `cached` facts — is exactly the one
/// this tune's `probed` facts would have started. Returns the delivery to
/// publish (the probed facts' own) on agreement, `None` on disagreement.
///
/// Agreement is decided on what the facts *produce*, not on a list of the
/// fields the planner reads: the resolved delivery must be identical apart
/// from the facts it carries, the FFmpeg argument vector built from it with
/// the same encoder admission must be identical byte for byte, and on an
/// encode route the admission's own inputs (source height, codec, HDR) must
/// be identical, since they chose that encoder. A field list would have to be
/// kept in step with the planner and the command builder by hand; this
/// cannot drift from them, because it runs them.
#[allow(clippy::too_many_arguments)] // the planner's inputs, as the start path holds them
fn warm_plan_agrees(
    system: &SystemInfo,
    running: &LiveTvTranscodePlan,
    cached: &LiveSourceFacts,
    probed: &LiveSourceFacts,
    playback: Option<&crate::live_tv_delivery::LivePlaybackRequest>,
    policy: &LiveQualityPolicy,
    support: &LiveExecutionSupport,
    directory: &Path,
) -> Option<LiveDeliveryPlan> {
    if running.delivery.video_action == LiveTrackAction::Encode
        && (cached.height, &cached.video_codec, &cached.hdr)
            != (probed.height, &probed.video_codec, &probed.hdr)
    {
        return None;
    }
    let delivery =
        crate::live_tv_delivery::resolve_live_delivery(probed, playback, policy, support).ok()?;
    let mut same_plan = delivery.clone();
    same_plan.source = running.delivery.source.clone();
    if same_plan != running.delivery {
        return None;
    }
    let mut probed_plan = LiveTvTranscodePlan::new(
        system,
        delivery.clone(),
        running.encoder,
        running.software_threads,
    )
    .ok()?;
    if let Some(binding) = &running.macos_upload {
        if live_macos_upload_graph(&probed_plan) != Some(binding.graph) || cached != probed {
            return None;
        }
        probed_plan.macos_upload = Some(binding.clone());
    }
    let argv = |plan: &LiveTvTranscodePlan| {
        live_ffmpeg_command(system, plan, directory).map(|command| {
            command
                .as_std()
                .get_args()
                .map(std::ffi::OsStr::to_os_string)
                .collect::<Vec<_>>()
        })
    };
    (argv(running).ok()? == argv(&probed_plan).ok()?).then_some(delivery)
}

/// Whether the input a viewer's FFmpeg opened contradicts the facts its plan
/// was made from (plan L-03 §2.4 D5). A joiner plans from its transport's
/// probe but is fed the live edge, so this is the check that the two agree.
///
/// Only facts the plan is sized from, and only where both sides state them:
/// frame size (output height, scaling and encode admission) and audio channel
/// count. Scan is left out: FFmpeg's banner omits field order where FFprobe
/// reports `unknown`, and the session maps `unknown` to progressive, so
/// comparing it would refuse starts whose plan is right.
fn input_contradicts_plan(planned: &LiveSourceFacts, input: &LiveTvSourceFormat) -> bool {
    fn differs<T: PartialEq>(planned: Option<T>, input: Option<T>) -> bool {
        matches!((planned, input), (Some(planned), Some(input)) if planned != input)
    }
    differs(planned.width, input.video_width)
        || differs(planned.height, input.video_height)
        || differs(planned.audio_channels, input.audio_channels)
}

type ScratchScanTask = tokio::task::JoinHandle<Result<Option<ScratchInventory>, LiveTvError>>;

/// The `linger_after_warm_reap` seam: hold a disagreeing warm start between
/// its FFmpeg's reap and its restart until the transport has closed this
/// viewer's feed, then for as long again as a slow reap would, so the fan-out
/// has run many chunks with that viewer gone. Bounded, so a fan-out that
/// never runs cannot hang the test here.
#[cfg(test)]
async fn linger_after_warm_reap(
    manager: &Weak<LiveTvManager>,
    transport: &dvr::DvrTransport,
    capability: &str,
) {
    if !manager
        .upgrade()
        .is_some_and(|manager| manager.linger_after_warm_reap.load(Ordering::Acquire))
    {
        return;
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < until
        && transport
            .live_viewers()
            .iter()
            .any(|viewer| viewer.capability == capability)
    {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
}

type SessionFenceTask = tokio::task::JoinHandle<Result<(), LiveTvError>>;

/// Start one running-session ledger read on its own task, so the session's
/// tick polls its verdict instead of waiting on the Store for it. The tick
/// keeps running the local half (`session_fence_local`) meanwhile.
fn spawn_session_ledger_fence(
    manager: &Weak<LiveTvManager>,
    session: &Arc<LiveTvSession>,
) -> SessionFenceTask {
    let manager = Weak::clone(manager);
    let session = Arc::clone(session);
    tokio::spawn(async move {
        let owner = manager
            .upgrade()
            .ok_or_else(|| LiveTvError::StreamFailed("live-TV manager stopped".into()))?;
        owner.resource_session_fence(&session).await
    })
}

/// Start one inventory scan of a session's scratch on its own task, so the
/// session's tick can stop waiting for it without abandoning it.
fn spawn_scratch_scan(manager: &Weak<LiveTvManager>, directory: &Path) -> ScratchScanTask {
    #[cfg(test)]
    if let Some(manager) = manager.upgrade() {
        manager.scratch_scans_started.fetch_add(1, Ordering::AcqRel);
        if manager.hang_scratch_scans.load(Ordering::Acquire) {
            return tokio::spawn(std::future::pending());
        }
    }
    #[cfg(not(test))]
    let _ = manager;
    let directory = directory.to_owned();
    tokio::spawn(async move { inspect_scratch(&directory).await })
}

/// What one walk of a session's scratch directory found: the final segments
/// by sequence and the init segment, each with its size.
struct ScratchScan {
    final_segments: HashMap<u64, (String, u64)>,
    init: Option<(String, u64)>,
}

/// The directory walk of `inspect_scratch`, synchronous, so that one
/// `spawn_blocking` runs all of it: one `read_dir` and one `symlink_metadata`
/// per entry on the thread that already holds the directory, rather than a
/// blocking-pool dispatch per entry (plan L-02 §3.4). Every check is the one
/// the asynchronous walk made, in the same order — the non-regular-entry
/// rule, the 128 MiB total (which a growing temporary segment reaches before
/// the playlist changes), the playlist's byte budget, the unexpected-name
/// rule and the at-most-one-temporary-file rule.
fn scan_scratch_directory(directory: &Path) -> Result<Option<ScratchScan>, LiveTvError> {
    let entries = match std::fs::read_dir(directory) {
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
    let mut init = None;
    let mut temporary_count = 0usize;
    let mut upload_temporary_count = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| {
            LiveTvError::StreamFailed(format!("reading live-TV scratch: {error}"))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = match std::fs::symlink_metadata(entry.path()) {
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
            "init.mp4" => init = Some((name, metadata.len())),
            "init.mp4.tmp" => temporary_count += 1,
            _ => {
                if let Some(sequence) = parse_segment_name(&name, false) {
                    final_segments.insert(sequence, (name, metadata.len()));
                } else if parse_segment_name(&name, true).is_some() {
                    temporary_count += 1;
                } else if is_upload_temporary(&name) {
                    upload_temporary_count += 1;
                } else {
                    return Err(LiveTvError::StreamFailed(
                        "live-TV scratch contains an unexpected filename".into(),
                    ));
                }
            }
        }
    }
    if temporary_count > 1 || upload_temporary_count > MAX_UPLOAD_TEMPORARIES {
        return Err(LiveTvError::StreamFailed(
            "live-TV scratch contains too many temporary files".into(),
        ));
    }
    Ok(Some(ScratchScan {
        final_segments,
        init,
    }))
}

async fn inspect_scratch(directory: &Path) -> Result<Option<ScratchInventory>, LiveTvError> {
    let walked = directory.to_owned();
    let scan = tokio::task::spawn_blocking(move || scan_scratch_directory(&walked))
        .await
        .map_err(|error| {
            LiveTvError::StreamFailed(format!("reading live-TV scratch: {error}"))
        })??;
    let Some(ScratchScan {
        final_segments,
        init,
    }) = scan
    else {
        return Ok(None);
    };
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
    if parsed.init.is_some() != init.is_some() {
        return Ok(None);
    }
    let unlisted = final_segments
        .keys()
        .filter(|sequence| !parsed.segments.contains(sequence))
        .filter(|sequence| {
            !final_segments
                .get(sequence)
                .is_some_and(|(name, _)| crate::playback_cache::is_pinned(&directory.join(name)))
        })
        .count();
    if parsed.segments.len() > MAX_LISTED_SEGMENTS || unlisted > MAX_DELETION_LAG_SEGMENTS {
        return Err(LiveTvError::StreamFailed(
            "live-TV scratch exceeded its segment inventory budget".into(),
        ));
    }
    Ok(Some(ScratchInventory {
        playlist,
        media_sequence: parsed.media_sequence,
        listed: parsed.segments.len(),
        listed_seconds: parsed.listed_seconds,
        target_duration: parsed.target_duration,
        init,
        // Include FFmpeg's one deletion-lag segment. A browser which received
        // the immediately previous manifest may still request it after the
        // current manifest is sampled; discarding it here turned a physically
        // present, valid segment into a false 410.
        segments: final_segments,
    }))
}

struct ParsedPlaylist {
    media_sequence: u64,
    target_duration: u64,
    listed_seconds: f64,
    init: Option<String>,
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
    let mut target_duration = None;
    let mut listed_seconds = 0.0;
    let mut init = None;
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
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if target_duration.is_some() {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist repeats its target duration".into(),
                ));
            }
            let parsed = value.trim().parse::<u64>().map_err(|_| {
                LiveTvError::StreamFailed("live-TV target duration is invalid".into())
            })?;
            if parsed == 0 {
                return Err(LiveTvError::StreamFailed(
                    "live-TV target duration is invalid".into(),
                ));
            }
            target_duration = Some(parsed);
        } else if line == "#EXT-X-MAP:URI=\"init.mp4\"" {
            if init.replace("init.mp4".to_owned()).is_some() {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist repeats its init segment".into(),
                ));
            }
        } else if line.starts_with("#EXT-X-MAP:")
            || (line.starts_with('#') && line.contains("URI="))
        {
            return Err(LiveTvError::StreamFailed(
                "live-TV playlist contains an unauthorized resource URI".into(),
            ));
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            if expect_segment {
                return Err(LiveTvError::StreamFailed(
                    "live-TV playlist has a segment without a resource".into(),
                ));
            }
            let duration = value
                .split_once(',')
                .map_or(value, |(duration, _)| duration)
                .trim()
                .parse::<f64>()
                .map_err(|_| {
                    LiveTvError::StreamFailed("live-TV segment duration is invalid".into())
                })?;
            if !duration.is_finite() || duration.is_sign_negative() || duration > 60.0 {
                return Err(LiveTvError::StreamFailed(
                    "live-TV segment duration is invalid".into(),
                ));
            }
            listed_seconds += duration;
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
        return Err(LiveTvError::StreamFailed(format!(
            "live-TV playlist exceeds {MAX_LISTED_SEGMENTS} segments"
        )));
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
    let target_duration = target_duration.ok_or_else(|| {
        LiveTvError::StreamFailed("live-TV playlist is missing its target duration".into())
    })?;
    Ok(ParsedPlaylist {
        media_sequence,
        target_duration,
        listed_seconds,
        init,
        segments,
    })
}

/// Publish only when the viewer receives one full segment after the one it
/// starts on, including a full target duration on a long-GOP copy route.
fn startup_publishable(inventory: &ScratchInventory) -> bool {
    inventory.listed >= STARTUP_LISTED_SEGMENTS
        && inventory.listed_seconds
            >= STARTUP_LISTED_TARGET_DURATIONS * inventory.target_duration as f64
}

/// Match only the HTTP receiver's `<published-name>.<request-u64>.tmp`
/// namespace. Uploads count toward the scratch byte budget, but never enter
/// the inventory of complete resources or the startup publication barrier.
fn is_upload_temporary(name: &str) -> bool {
    let Some((object, request)) = name.strip_suffix(".tmp").and_then(|s| s.rsplit_once('.')) else {
        return false;
    };
    !request.is_empty()
        && request.bytes().all(|byte| byte.is_ascii_digit())
        && request.parse::<u64>().is_ok()
        && (matches!(object, "index.m3u8" | "init.mp4")
            || parse_segment_name(object, false).is_some())
}

fn parse_segment_name(name: &str, temporary: bool) -> Option<u64> {
    let stem = if temporary {
        name.strip_suffix(".ts.tmp")
            .or_else(|| name.strip_suffix(".m4s.tmp"))?
    } else {
        name.strip_suffix(".ts")
            .or_else(|| name.strip_suffix(".m4s"))?
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

struct LiveSegmentFile {
    file: tokio::fs::File,
    cache_pin: Option<crate::playback_cache::CachePin>,
}

async fn open_bounded_segment(
    path: &Path,
    expected_len: u64,
) -> Result<LiveSegmentFile, LiveTvError> {
    if expected_len > MAX_SESSION_BYTES {
        return Err(LiveTvError::StreamFailed(
            "live-TV segment exceeds its byte budget".into(),
        ));
    }

    let cache_pin = crate::playback_cache::pin(path).map_err(|error| {
        LiveTvError::CapabilityExpired(format!("live segment is unavailable: {error}"))
    })?;
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
    Ok(LiveSegmentFile { file, cache_pin })
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
    opened: LiveSegmentFile,
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
        // Hold the disk pin as long as the file descriptor can retain blocks.
        let LiveSegmentFile {
            mut file,
            cache_pin: _cache_pin,
        } = opened;
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
            source_format: None,
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
                "HDHomeRun document request failed on the session server"
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
                    "HDHomeRun response body failed on the session server"
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
    encoder: Encoder,
    system: &SystemInfo,
    height: u16,
    directory: &Path,
) -> Result<(), String> {
    let playlist = directory.join("index.m3u8");
    let plan = LiveTvTranscodePlan::new(
        system,
        graph_probe_delivery(height),
        Some(encoder),
        (encoder == Encoder::Software).then_some(2),
    )
    .map_err(|error| format!("could not prepare live-TV graph probe: {error}"))?;
    let mut command =
        live_ffmpeg_command_for_input(system, &plan, directory, LiveTvFfmpegInput::GraphProbe)
            .map_err(|error| format!("could not build live-TV graph probe: {error}"))?;
    let output = tokio::time::timeout(
        READINESS_PROBE_TIME,
        crate::process_control::output_job_owned(
            &mut command,
            crate::process_control::ChildWork::background("Live TV readiness probe"),
        ),
    )
    .await
    .map_err(|_| "live-TV FFmpeg graph probe timed out".to_owned())?
    .map_err(|error| format!("could not start live-TV FFmpeg probe: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "live-TV FFmpeg graph failed: {}",
            graph_failure_summary(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    let manifest = tokio::fs::read_to_string(&playlist)
        .await
        .map_err(|error| format!("live-TV probe did not publish a playlist: {error}"))?;
    let one_second_segments = manifest
        .lines()
        .filter(|line| line.starts_with("#EXTINF:1."))
        .count();
    if manifest.contains("#EXT-X-ENDLIST")
        || !manifest.contains("#EXT-X-TARGETDURATION:1")
        || one_second_segments < 3
        || !manifest.contains(".ts")
    {
        return Err("live-TV probe published an invalid live playlist".to_owned());
    }
    Ok(())
}

/// The first three actionable stderr lines of a failed readiness graph. On
/// VA-API and QSV, libva's own `libva info:` banner comes first and was all the
/// error used to say (review 76, P3-1); it is shown only when nothing else was
/// printed.
fn graph_failure_summary(stderr: &str) -> String {
    let classified = crate::ffmpeg::classify_diagnostic(stderr);
    let lines = if classified.actionable.is_empty() {
        classified.informational
    } else {
        classified.actionable
    };
    lines.lines().take(3).collect::<Vec<_>>().join(" ")
}

fn graph_probe_delivery(height: u16) -> LiveDeliveryPlan {
    LiveDeliveryPlan {
        source: LiveSourceFacts {
            video_codec: Some("mpeg2video".into()),
            width: Some(1920),
            height: Some(1080),
            audio_codec: Some("ac3".into()),
            audio_channels: Some(2),
            ..LiveSourceFacts::default()
        },
        output: crate::live_tv_delivery::LiveDeliveryOutput {
            container: "mpegts".into(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            width: height.saturating_mul(16) / 9,
            height,
            bit_depth: Some(8),
            frame_rate: None,
            hdr: None,
            audio_channels: 2,
        },
        video_action: LiveTrackAction::Encode,
        audio_action: LiveTrackAction::Encode,
        audio_track: 0,
        packaging: LivePackaging::Mpegts,
        reasons: vec![crate::live_tv_delivery::LiveDeliveryReason {
            code: "graph_probe".into(),
            explanation: "exercise the encoded live path".into(),
        }],
        deinterlace: false,
        deinterlace_output: None,
        max_bitrate_bps: None,
    }
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
mod atsc_audio_tests;

mod caption_probe;

#[cfg(test)]
mod tests;
