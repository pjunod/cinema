//! Versioned playback-control messages and per-generation sequence fencing.
//!
//! M1 is deliberately behavior-neutral: accepted exchanges renew the legacy
//! delivery clock and return an observation with `action: none`.  Keeping the
//! wire contract and the mutation fence here prevents later policy work from
//! leaking into HTTP routing or reintroducing several independent recovery
//! owners.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::transcode::{HlsSessionInfo, SessionKind};

pub(crate) const PROTOCOL_V1: &str = "plurx-playback-control-v1";
pub(crate) const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RELAY_BYTES: usize = 20 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const EXCHANGE_DEADLINE: Duration = Duration::from_secs(4);
pub(crate) const NEXT_EXCHANGE_MS: u32 = 5_000;
pub(crate) const ROLLING_LEASE_TIMEOUT_MS: u32 = 60_000;
pub(crate) const VOD_LEASE_TIMEOUT_MS: u32 = 300_000;

const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const MAX_OBSERVED_DOWNLOAD_BPS: u64 = 10_000_000_000_000;
const MAX_ERROR_DETAIL_BYTES: usize = 512;
const MAX_CAPABILITY_VALUES: usize = 8;
const MIN_CONTROL_INTERVAL: Duration = Duration::from_millis(250);
const RELAY_BUCKETS_MS: [u64; 9] = [10, 25, 50, 100, 250, 500, 1_000, 2_500, 4_000];

/// Preserve the ingress's absolute exchange deadline across a cluster hop.
/// The cap also prevents a malformed trusted-peer envelope from extending the
/// public four-second contract.
pub(crate) fn inherited_exchange_budget(
    deadline_unix_ms: i64,
    now_unix_ms: i64,
) -> Option<Duration> {
    let remaining_ms = u64::try_from(deadline_unix_ms.saturating_sub(now_unix_ms))
        .ok()
        .filter(|remaining| *remaining > 0)?;
    Some(Duration::from_millis(remaining_ms).min(EXCHANGE_DEADLINE))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlBootstrap {
    pub protocol: String,
    pub url: String,
    pub generation: String,
    pub control_epoch: u64,
    pub next_exchange_ms: u32,
    pub lease_timeout_ms: u32,
}

impl ControlBootstrap {
    pub(crate) fn new(
        session_id: &str,
        generation: &str,
        owner_epoch: i64,
        lease_timeout_ms: u32,
    ) -> Option<Self> {
        let control_epoch = u64::try_from(owner_epoch).ok().filter(|epoch| *epoch > 0)?;
        if !matches!(
            lease_timeout_ms,
            ROLLING_LEASE_TIMEOUT_MS | VOD_LEASE_TIMEOUT_MS
        ) {
            return None;
        }
        Some(Self {
            protocol: PROTOCOL_V1.to_owned(),
            url: format!("/api/v1/hls/{session_id}/control"),
            generation: generation.to_owned(),
            control_epoch,
            next_exchange_ms: NEXT_EXCHANGE_MS,
            lease_timeout_ms,
        })
    }

    pub(crate) fn refreshed(
        &self,
        session_id: &str,
        generation: &str,
        owner_epoch: i64,
    ) -> Option<Self> {
        Self::new(session_id, generation, owner_epoch, self.lease_timeout_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlRequestV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub client_instance_id: String,
    pub sequence: u64,
    pub demand: PlaybackDemand,
    pub position_ms: i64,
    pub buffered_from_ms: Option<i64>,
    pub buffered_through_ms: i64,
    pub playback_rate: f64,
    pub render_state: RenderState,
    pub seek_target_ms: Option<i64>,
    pub observed_download_bps: Option<u64>,
    pub selection: ClientSelection,
    pub capabilities: Option<DynamicCapabilities>,
    pub observation: Option<ClientObservation>,
    pub acknowledgement: Option<ActionAcknowledgement>,
}

impl ControlRequestV1 {
    pub(crate) fn validate(
        &self,
        duration_ms: Option<i64>,
        target_duration_ms: i64,
    ) -> Result<(), &'static str> {
        if self.protocol != PROTOCOL_V1 {
            return Err("protocol");
        }
        if uuid::Uuid::parse_str(&self.generation).is_err() {
            return Err("generation");
        }
        if self.control_epoch == 0 {
            return Err("control_epoch");
        }
        if uuid::Uuid::parse_str(&self.client_instance_id).is_err() {
            return Err("client_instance_id");
        }
        if self.sequence == 0 {
            return Err("sequence");
        }
        let maximum_position = duration_ms
            .filter(|duration| (0..=MAX_MEDIA_MILLIS).contains(duration))
            .map_or(MAX_MEDIA_MILLIS, |duration| {
                duration.saturating_add(target_duration_ms.clamp(2_000, 30_000))
            });
        for (field, value) in [
            ("position_ms", self.position_ms),
            ("buffered_through_ms", self.buffered_through_ms),
        ] {
            if !(0..=maximum_position).contains(&value) {
                return Err(field);
            }
        }
        if let Some(buffered_from_ms) = self.buffered_from_ms {
            if !(0..=maximum_position).contains(&buffered_from_ms)
                || buffered_from_ms > self.buffered_through_ms
            {
                return Err("buffered_from_ms");
            }
        }
        let buffer_anchor = self.seek_target_ms.unwrap_or(self.position_ms);
        if self.buffered_through_ms < buffer_anchor {
            return Err("buffered_through_ms");
        }
        if self.buffered_from_ms.is_some_and(|start| {
            start > buffer_anchor.saturating_add(target_duration_ms.clamp(2_000, 30_000))
        }) {
            return Err("buffered_from_ms");
        }
        if !self.playback_rate.is_finite()
            || match self.demand {
                PlaybackDemand::Active => !(0.25..=4.0).contains(&self.playback_rate),
                PlaybackDemand::Hold | PlaybackDemand::End => {
                    !(0.0..=4.0).contains(&self.playback_rate)
                }
            }
        {
            return Err("playback_rate");
        }
        match (self.render_state, self.seek_target_ms) {
            (RenderState::Seeking, Some(target)) if (0..=maximum_position).contains(&target) => {}
            (RenderState::Seeking, _) => return Err("seek_target_ms"),
            (_, None) => {}
            (_, Some(_)) => return Err("seek_target_ms"),
        }
        if self
            .observed_download_bps
            .is_some_and(|value| value > MAX_OBSERVED_DOWNLOAD_BPS)
        {
            return Err("observed_download_bps");
        }
        self.selection.validate()?;
        if let Some(capabilities) = &self.capabilities {
            capabilities.validate()?;
        } else if self.sequence == 1 {
            return Err("capabilities");
        }
        if let Some(observation) = &self.observation {
            observation.validate()?;
        }
        if let Some(acknowledgement) = &self.acknowledgement {
            acknowledgement.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlaybackDemand {
    Active,
    Hold,
    End,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RenderState {
    Starting,
    Rendering,
    Waiting,
    Stalled,
    Seeking,
    Ended,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientSelection {
    pub quality: QualitySelection,
    pub audio_track: Option<i64>,
    pub subtitle: SubtitleSelection,
    pub audio_offset_ms: i64,
    pub codec: CodecPolicy,
    pub dynamic_range: DynamicRangePolicy,
}

impl ClientSelection {
    fn validate(&self) -> Result<(), &'static str> {
        if let QualitySelection::Manual { height } = self.quality {
            if !(crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT).contains(&height) {
                return Err("selection.quality.height");
            }
        }
        if self
            .audio_track
            .is_some_and(|index| !(0..=1_024).contains(&index))
        {
            return Err("selection.audio_track");
        }
        self.subtitle.validate()?;
        if !(-15_000..=15_000).contains(&self.audio_offset_ms) {
            return Err("selection.audio_offset_ms");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum QualitySelection {
    Auto,
    Manual { height: i64 },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubtitleSelection {
    pub mode: SubtitleMode,
    pub track: Option<i64>,
}

impl SubtitleSelection {
    fn validate(&self) -> Result<(), &'static str> {
        if self
            .track
            .is_some_and(|index| !(0..=1_024).contains(&index))
        {
            return Err("selection.subtitle.track");
        }
        if matches!(self.mode, SubtitleMode::Off) && self.track.is_some() {
            return Err("selection.subtitle.track");
        }
        if !matches!(self.mode, SubtitleMode::Off) && self.track.is_none() {
            return Err("selection.subtitle.track");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubtitleMode {
    Off,
    Native,
    Overlay,
    Burn,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodecPolicy {
    Auto,
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DynamicRangePolicy {
    Auto,
    DolbyVision,
    Hdr10,
    Hlg,
    Sdr,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DynamicCapabilities {
    pub platform: ClientPlatform,
    pub max_height: i64,
    pub codecs: Vec<CodecPolicy>,
    pub dynamic_ranges: Vec<DynamicRangePolicy>,
    pub dual_player_preparation: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientPlatform {
    Web,
    Apple,
    Android,
}

impl DynamicCapabilities {
    fn validate(&self) -> Result<(), &'static str> {
        if !(crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT).contains(&self.max_height)
        {
            return Err("capabilities.max_height");
        }
        if self.codecs.is_empty()
            || self.codecs.len() > MAX_CAPABILITY_VALUES
            || self.dynamic_ranges.is_empty()
            || self.dynamic_ranges.len() > MAX_CAPABILITY_VALUES
        {
            return Err("capabilities");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientObservation {
    pub dropped_frames: Option<u64>,
    pub decoder_state: Option<DecoderState>,
    pub error_code: Option<ClientErrorCode>,
    pub error_detail: Option<String>,
}

impl ClientObservation {
    fn validate(&self) -> Result<(), &'static str> {
        if self.error_detail.is_some() && self.error_code.is_none() {
            return Err("observation.error_detail");
        }
        if self.error_detail.as_ref().is_some_and(|detail| {
            detail.len() > MAX_ERROR_DETAIL_BYTES
                || detail
                    .bytes()
                    .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
        }) {
            return Err("observation.error_detail");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecoderState {
    Unknown,
    Ready,
    Starved,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientErrorCode {
    Network,
    Manifest,
    Media,
    Decoder,
    Drm,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionAcknowledgement {
    pub action_id: String,
    pub state: AcknowledgementState,
    pub buffered_through_ms: Option<i64>,
    pub first_frame_unix_ms: Option<i64>,
}

impl ActionAcknowledgement {
    fn validate(&self) -> Result<(), &'static str> {
        if uuid::Uuid::parse_str(&self.action_id).is_err() {
            return Err("acknowledgement.action_id");
        }
        if self
            .buffered_through_ms
            .is_some_and(|value| !(0..=MAX_MEDIA_MILLIS).contains(&value))
        {
            return Err("acknowledgement.buffered_through_ms");
        }
        if self.first_frame_unix_ms.is_some_and(|value| value <= 0) {
            return Err("acknowledgement.first_frame_unix_ms");
        }
        match self.state {
            AcknowledgementState::BufferReady if self.buffered_through_ms.is_none() => {
                Err("acknowledgement.buffered_through_ms")
            }
            AcknowledgementState::Committed if self.first_frame_unix_ms.is_none() => {
                Err("acknowledgement.first_frame_unix_ms")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcknowledgementState {
    MetadataReady,
    BufferReady,
    Committed,
    Failed,
    Aborted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlResponseV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub accepted_sequence: u64,
    pub server_time_unix_ms: i64,
    pub lease: PlaybackLeaseView,
    pub delivery: DeliveryView,
    pub effective_selection: EffectiveSelection,
    pub action: ControlAction,
}

impl ControlResponseV1 {
    pub(crate) fn is_valid_for(&self, request: &ControlRelayRequest) -> bool {
        self.protocol == PROTOCOL_V1
            && self.generation == request.generation
            && u64::try_from(request.expected_owner_epoch).ok() == Some(self.control_epoch)
            && self.accepted_sequence > 0
            && self.accepted_sequence <= request.control.sequence
            && self.server_time_unix_ms > 0
            && self.lease.state == "active"
            && self.lease.renew_after_ms == NEXT_EXCHANGE_MS
            && self.lease.expires_at_unix_ms >= self.server_time_unix_ms
            && self.lease.expires_at_unix_ms
                <= self
                    .server_time_unix_ms
                    .saturating_add(i64::from(VOD_LEASE_TIMEOUT_MS))
            && matches!(self.delivery.presentation.as_str(), "live-recovery" | "vod")
            && matches!(
                self.delivery.producer_state.as_str(),
                "running" | "held" | "complete" | "failed" | "waiting" | "vod"
            )
            && self
                .delivery
                .produced_through_ms
                .is_none_or(|value| value >= 0)
            && self.delivery.fetched_through_ms >= 0
            && self.delivery.delivered_bps.is_none_or(|value| value >= 0)
            && self
                .delivery
                .delivered_idle_ms
                .is_none_or(|value| value >= 0)
            && self
                .delivery
                .recent_producer_speed
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && self.delivery.client_runway_ms >= 0
            && self.delivery.hold_reason.as_deref().is_none_or(|value| {
                matches!(
                    value,
                    "time" | "bytes" | "global" | "ahead" | "working_set" | "no_room"
                )
            })
            && self.delivery.owner_epoch == self.control_epoch
            && self.delivery.owner_node_hash.starts_with("n-")
            && self.delivery.owner_node_hash.len() == 18
            && self.delivery.owner_node_hash[2..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            && (0..=crate::transcode::MAX_HEIGHT).contains(&self.effective_selection.height)
            && matches!(
                self.effective_selection.codec.as_str(),
                "source" | "server_selected"
            )
            && self
                .effective_selection
                .dynamic_range
                .as_deref()
                .is_none_or(|value| matches!(value, "dolby_vision" | "hdr10" | "hlg" | "sdr"))
            && self.action == ControlAction::None
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackLeaseView {
    pub state: String,
    pub renew_after_ms: u32,
    pub expires_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeliveryView {
    pub presentation: String,
    pub producer_state: String,
    pub produced_through_ms: Option<i64>,
    pub fetched_through_ms: i64,
    pub delivered_bps: Option<i64>,
    pub delivered_idle_ms: Option<i64>,
    pub recent_producer_speed: Option<f64>,
    pub client_runway_ms: i64,
    pub admitted: Option<bool>,
    pub hold_reason: Option<String>,
    pub owner_node_hash: String,
    pub owner_epoch: u64,
}

impl DeliveryView {
    pub(crate) fn from_status(
        status: &HlsSessionInfo,
        request: &ControlRequestV1,
        owner_node_id: &str,
        owner_epoch: u64,
        media_origin_ms: i64,
    ) -> Self {
        let client_runway_ms = request
            .buffered_through_ms
            .saturating_sub(request.position_ms);
        match status {
            HlsSessionInfo::Live(info) => Self {
                presentation: info.presentation.to_owned(),
                producer_state: info.producer_state.to_owned(),
                produced_through_ms: info
                    .published_end_ms
                    .map(|value| value.saturating_add(media_origin_ms)),
                fetched_through_ms: info.fetched_end_ms.saturating_add(media_origin_ms),
                delivered_bps: info.delivered_bps,
                delivered_idle_ms: Some(info.delivered_idle_ms),
                recent_producer_speed: info.recent_speed,
                client_runway_ms,
                admitted: None,
                hold_reason: info.hold_reason.map(|reason| {
                    match reason {
                        crate::transcode::AheadHoldReason::Time => "time",
                        crate::transcode::AheadHoldReason::Bytes => "bytes",
                        crate::transcode::AheadHoldReason::Global => "global",
                    }
                    .to_owned()
                }),
                owner_node_hash: node_hash(owner_node_id),
                owner_epoch,
            },
            HlsSessionInfo::Vod(info) => Self {
                presentation: "vod".to_owned(),
                producer_state: info.producer_state.to_owned(),
                produced_through_ms: info.published_end_ms,
                fetched_through_ms: info.fetched_end_ms,
                delivered_bps: None,
                delivered_idle_ms: None,
                recent_producer_speed: None,
                client_runway_ms,
                admitted: Some(info.admitted),
                hold_reason: info.producer_hold.map(str::to_owned),
                owner_node_hash: node_hash(owner_node_id),
                owner_epoch,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectiveSelection {
    pub quality_auto: bool,
    pub height: i64,
    pub audio_track: Option<i64>,
    pub subtitle_burn: Option<i64>,
    pub audio_offset_ms: i64,
    pub codec: String,
    pub dynamic_range: Option<String>,
}

impl EffectiveSelection {
    pub(crate) fn from_recipe(
        recipe: &crate::media_sessions::RemoteStartRequest,
        delivered_height: i64,
        dynamic_range: Option<String>,
    ) -> Self {
        let codec = match &recipe.request.kind {
            SessionKind::Copy { .. } => "source",
            SessionKind::Transcode { .. } => "server_selected",
        };
        Self {
            quality_auto: recipe.request.automatic,
            height: delivered_height,
            audio_track: recipe.request.audio_index,
            subtitle_burn: recipe.request.subtitle_burn,
            audio_offset_ms: recipe.request.audio_offset_ms,
            codec: codec.to_owned(),
            dynamic_range,
        }
    }
}

pub(crate) fn target_duration_ms(recipe: &crate::media_sessions::RemoteStartRequest) -> i64 {
    let seconds = match &recipe.request.kind {
        SessionKind::Copy { .. } => plurx_core::transcode::COPY_SEGMENT_MAX_SECS,
        SessionKind::Transcode { .. } => plurx_core::transcode::SEGMENT_SECONDS,
    };
    i64::from(seconds) * 1_000
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ControlAction {
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_field: Option<String>,
}

impl ControlErrorBody {
    pub(crate) fn is_valid_for_status(&self, status: u16) -> bool {
        matches!(
            (status, self.code.as_str()),
            (400, "invalid_control")
                | (404, "session_gone")
                | (409, "owner_changed" | "stale_control")
                | (410, "session_ended")
                | (425, "owner_transition")
                | (429, "control_rate_limited")
                | (503, "control_unavailable")
        ) && !self.message.is_empty()
            && self.message.len() <= 512
            && self
                .generation
                .as_deref()
                .is_none_or(|value| uuid::Uuid::parse_str(value).is_ok())
            && self.control_epoch.is_none_or(|epoch| epoch > 0)
            && self.retry_after_ms.is_none_or(|delay| delay <= 60_000)
            && self
                .invalid_field
                .as_deref()
                .is_none_or(|field| field.len() <= 128)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlRelayRequest {
    pub session_id: String,
    pub generation: String,
    pub expected_owner_node_id: String,
    pub expected_owner_epoch: i64,
    /// Absolute budget inherited from the public ingress exchange. The owner
    /// must never restart the four-second budget after routing or Store I/O.
    pub deadline_unix_ms: i64,
    pub control: ControlRequestV1,
}

impl ControlRelayRequest {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && uuid::Uuid::parse_str(&self.generation).is_ok()
            && !self.expected_owner_node_id.is_empty()
            && self.expected_owner_node_id.len() <= 256
            && !self
                .expected_owner_node_id
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
            && self.expected_owner_epoch > 0
            && self.deadline_unix_ms > 0
            && self.control.generation == self.generation
            && u64::try_from(self.expected_owner_epoch).ok() == Some(self.control.control_epoch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlDisposition {
    Accepted,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlStateError {
    StaleGeneration,
    OwnerChanged,
    StaleClient,
    StaleSequence,
    RateLimited(u32),
    SessionEnded,
    OwnerTransition,
    Unavailable,
}

/// Re-read the committed owner tuple immediately at the local mutation gate.
/// That read is the control operation's authority linearization point: a
/// terminal transition committed before it is observed; one committed after
/// it is ordered after this exchange and the local retirement gate prevents
/// the two process-local mutations from crossing.
pub(crate) async fn verify_authority(
    store: &dyn plurx_core::store::Store,
    session_id: &str,
    generation: &str,
    owner_node_id: &str,
    owner_epoch: u64,
) -> Result<(), ControlStateError> {
    let route = tokio::time::timeout(
        Duration::from_secs(2),
        store.media_session_route(session_id),
    )
    .await
    .map_err(|_| ControlStateError::Unavailable)?
    .map_err(|_| ControlStateError::Unavailable)?
    .ok_or(ControlStateError::SessionEnded)?;
    if route.incarnation_id != generation {
        return Err(ControlStateError::StaleGeneration);
    }
    if route.owner_node_id != owner_node_id
        || u64::try_from(route.owner_epoch).ok() != Some(owner_epoch)
    {
        return Err(ControlStateError::OwnerChanged);
    }
    if route.state != "active" {
        return Err(ControlStateError::SessionEnded);
    }
    if route.lease_expires_at_ms <= crate::media_sessions::unix_ms() {
        return Err(ControlStateError::OwnerTransition);
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct LocalControlResult {
    pub disposition: ControlDisposition,
    pub accepted_sequence: u64,
    pub action: ControlAction,
    pub lease_expires_at_unix_ms: i64,
    pub lease_timeout_ms: u32,
    pub status: HlsSessionInfo,
    pub platform: ClientPlatform,
}

#[derive(Clone, Copy)]
pub(crate) struct LocalControlRequest<'a> {
    pub session_id: &'a str,
    pub generation: &'a str,
    pub owner_node_id: &'a str,
    pub owner_epoch: u64,
    pub client_instance_id: &'a str,
    pub sequence: u64,
    pub platform: Option<ClientPlatform>,
}

#[derive(Debug)]
pub(crate) struct ControlState {
    generation: Option<String>,
    owner_epoch: u64,
    client_instance_id: Option<uuid::Uuid>,
    client_platform: Option<ClientPlatform>,
    last_sequence: u64,
    last_accepted_at: Option<Instant>,
    prior_action: ControlAction,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            generation: None,
            owner_epoch: 0,
            client_instance_id: None,
            client_platform: None,
            last_sequence: 0,
            last_accepted_at: None,
            prior_action: ControlAction::None,
        }
    }
}

impl ControlState {
    /// Apply the second, owner-local fence after ingress or relay has proved
    /// the same tuple against the durable route. Advancing an epoch resets the
    /// client sequence space; an older epoch can never renew the new owner.
    pub(crate) fn accept(
        &mut self,
        generation: &str,
        owner_epoch: u64,
        client_instance_id: &str,
        sequence: u64,
        platform: Option<ClientPlatform>,
    ) -> Result<(ControlDisposition, u64, ControlAction, ClientPlatform), ControlStateError> {
        self.accept_at(
            Instant::now(),
            generation,
            owner_epoch,
            client_instance_id,
            sequence,
            platform,
        )
    }

    fn accept_at(
        &mut self,
        now: Instant,
        generation: &str,
        owner_epoch: u64,
        client_instance_id: &str,
        sequence: u64,
        platform: Option<ClientPlatform>,
    ) -> Result<(ControlDisposition, u64, ControlAction, ClientPlatform), ControlStateError> {
        let client_instance_id = uuid::Uuid::parse_str(client_instance_id)
            .map_err(|_| ControlStateError::StaleClient)?;
        match self.generation.as_deref() {
            None => self.generation = Some(generation.to_owned()),
            Some(existing) if existing != generation => {
                return Err(ControlStateError::StaleGeneration)
            }
            Some(_) => {}
        }
        if owner_epoch < self.owner_epoch {
            return Err(ControlStateError::OwnerChanged);
        }
        if owner_epoch > self.owner_epoch {
            self.owner_epoch = owner_epoch;
            self.client_instance_id = None;
            self.client_platform = None;
            self.last_sequence = 0;
            self.last_accepted_at = None;
            self.prior_action = ControlAction::None;
        }
        match self.client_instance_id {
            None => {
                if sequence != 1 {
                    return Err(ControlStateError::StaleSequence);
                }
                let platform = platform.ok_or(ControlStateError::StaleClient)?;
                self.client_instance_id = Some(client_instance_id);
                self.client_platform = Some(platform);
            }
            Some(existing) if existing != client_instance_id => {
                return Err(ControlStateError::StaleClient)
            }
            Some(_) => {}
        }
        if platform.is_some_and(|platform| Some(platform) != self.client_platform) {
            return Err(ControlStateError::StaleClient);
        }
        let client_platform = self.client_platform.ok_or(ControlStateError::StaleClient)?;
        if sequence < self.last_sequence {
            return Err(ControlStateError::StaleSequence);
        }
        if sequence == self.last_sequence {
            return Ok((
                ControlDisposition::Replay,
                self.last_sequence,
                self.prior_action.clone(),
                client_platform,
            ));
        }
        if let Some(accepted_at) = self.last_accepted_at {
            let elapsed = now.saturating_duration_since(accepted_at);
            if elapsed < MIN_CONTROL_INTERVAL {
                let remaining = MIN_CONTROL_INTERVAL.saturating_sub(elapsed);
                return Err(ControlStateError::RateLimited(
                    u32::try_from(remaining.as_millis())
                        .unwrap_or(u32::MAX)
                        .max(1),
                ));
            }
        }
        self.last_sequence = sequence;
        self.last_accepted_at = Some(now);
        self.prior_action = ControlAction::None;
        Ok((
            ControlDisposition::Accepted,
            self.last_sequence,
            self.prior_action.clone(),
            client_platform,
        ))
    }
}

fn node_hash(node_id: &str) -> String {
    let digest = Sha256::digest(node_id.as_bytes());
    format!("n-{}", hex::encode(&digest[..8]))
}

#[derive(Clone, Copy)]
pub(crate) enum MetricOutcome {
    Accepted = 0,
    Replay = 1,
    Invalid = 2,
    Stale = 3,
    OwnerChanged = 4,
    Transition = 5,
    Gone = 6,
    Unavailable = 7,
    RateLimited = 8,
}

static CONTROL_EXCHANGES: [AtomicU64; 9] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static CONTROL_PLATFORMS: [[AtomicU64; 3]; 2] = [const { [const { AtomicU64::new(0) }; 3] }; 2];
static CONTROL_RELAY_OUTCOMES: [AtomicU64; 3] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static CONTROL_RELAY_BUCKETS: [AtomicU64; RELAY_BUCKETS_MS.len() + 1] =
    [const { AtomicU64::new(0) }; RELAY_BUCKETS_MS.len() + 1];
static CONTROL_RELAY_DURATION_MICROS: AtomicU64 = AtomicU64::new(0);

const RELAY_VALID_RESPONSE: usize = 0;
const RELAY_TRANSPORT_ERROR: usize = 1;
const RELAY_INVALID_RESPONSE: usize = 2;

/// Records every outbound control relay even when `?` exits its caller. The
/// default outcome is transport failure; callers promote it only after a
/// bounded response has passed the protocol schema and tuple checks.
pub(crate) struct RelayMetricGuard {
    started: Instant,
    outcome: usize,
}

impl RelayMetricGuard {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            outcome: RELAY_TRANSPORT_ERROR,
        }
    }

    pub(crate) fn invalid_response(&mut self) {
        self.outcome = RELAY_INVALID_RESPONSE;
    }

    pub(crate) fn valid_response(&mut self) {
        self.outcome = RELAY_VALID_RESPONSE;
    }
}

impl Drop for RelayMetricGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        CONTROL_RELAY_OUTCOMES[self.outcome].fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let bucket = RELAY_BUCKETS_MS
            .iter()
            .position(|bound| elapsed_ms <= *bound)
            .unwrap_or(RELAY_BUCKETS_MS.len());
        CONTROL_RELAY_BUCKETS[bucket].fetch_add(1, Ordering::Relaxed);
        CONTROL_RELAY_DURATION_MICROS.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

pub(crate) fn record(outcome: MetricOutcome) {
    CONTROL_EXCHANGES[outcome as usize].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_platform(outcome: MetricOutcome, platform: ClientPlatform) {
    let outcome_index = match outcome {
        MetricOutcome::Accepted => 0,
        MetricOutcome::Replay => 1,
        _ => return,
    };
    let platform_index = match platform {
        ClientPlatform::Web => 0,
        ClientPlatform::Apple => 1,
        ClientPlatform::Android => 2,
    };
    CONTROL_PLATFORMS[outcome_index][platform_index].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn prometheus() -> String {
    let mut output = String::from(
        "# HELP plurx_playback_control_exchanges_total Playback-control exchanges by bounded outcome.\n\
         # TYPE plurx_playback_control_exchanges_total counter\n",
    );
    for (index, outcome) in [
        "accepted",
        "replay",
        "invalid",
        "stale",
        "owner_changed",
        "owner_transition",
        "session_gone",
        "unavailable",
        "rate_limited",
    ]
    .iter()
    .enumerate()
    {
        output.push_str(&format!(
            "plurx_playback_control_exchanges_total{{outcome=\"{outcome}\"}} {}\n",
            CONTROL_EXCHANGES[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(
        "# HELP plurx_playback_control_platform_exchanges_total Successful playback-control exchanges by bounded outcome and client platform.\n\
         # TYPE plurx_playback_control_platform_exchanges_total counter\n",
    );
    for (outcome_index, outcome) in ["accepted", "replay"].iter().enumerate() {
        for (platform_index, platform) in ["web", "apple", "android"].iter().enumerate() {
            output.push_str(&format!(
                "plurx_playback_control_platform_exchanges_total{{outcome=\"{outcome}\",platform=\"{platform}\"}} {}\n",
                CONTROL_PLATFORMS[outcome_index][platform_index].load(Ordering::Relaxed)
            ));
        }
    }
    output.push_str(
        "# HELP plurx_playback_control_relays_total Outbound playback-control relays by bounded result.\n\
         # TYPE plurx_playback_control_relays_total counter\n",
    );
    for (index, outcome) in ["valid_response", "transport_error", "invalid_response"]
        .iter()
        .enumerate()
    {
        output.push_str(&format!(
            "plurx_playback_control_relays_total{{outcome=\"{outcome}\"}} {}\n",
            CONTROL_RELAY_OUTCOMES[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(
        "# HELP plurx_playback_control_relay_seconds Outbound playback-control relay latency.\n\
         # TYPE plurx_playback_control_relay_seconds histogram\n",
    );
    let mut cumulative = 0_u64;
    for (bucket_index, bound_ms) in RELAY_BUCKETS_MS.iter().enumerate() {
        cumulative =
            cumulative.saturating_add(CONTROL_RELAY_BUCKETS[bucket_index].load(Ordering::Relaxed));
        output.push_str(&format!(
            "plurx_playback_control_relay_seconds_bucket{{le=\"{}\"}} {cumulative}\n",
            *bound_ms as f64 / 1_000.0
        ));
    }
    cumulative = cumulative
        .saturating_add(CONTROL_RELAY_BUCKETS[RELAY_BUCKETS_MS.len()].load(Ordering::Relaxed));
    let sum = CONTROL_RELAY_DURATION_MICROS.load(Ordering::Relaxed) as f64 / 1_000_000.0;
    output.push_str(&format!(
        "plurx_playback_control_relay_seconds_bucket{{le=\"+Inf\"}} {cumulative}\n\
         plurx_playback_control_relay_seconds_sum {sum}\n\
         plurx_playback_control_relay_seconds_count {cumulative}\n"
    ));
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ControlRequestV1 {
        ControlRequestV1 {
            protocol: PROTOCOL_V1.to_owned(),
            generation: uuid::Uuid::new_v4().to_string(),
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            demand: PlaybackDemand::Active,
            position_ms: 10_000,
            buffered_from_ms: Some(9_000),
            buffered_through_ms: 25_000,
            playback_rate: 1.0,
            render_state: RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: Some(8_000_000),
            selection: ClientSelection {
                quality: QualitySelection::Auto,
                audio_track: Some(0),
                subtitle: SubtitleSelection {
                    mode: SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: CodecPolicy::Auto,
                dynamic_range: DynamicRangePolicy::Auto,
            },
            capabilities: Some(DynamicCapabilities {
                platform: ClientPlatform::Web,
                max_height: 2160,
                codecs: vec![CodecPolicy::H264, CodecPolicy::Hevc],
                dynamic_ranges: vec![DynamicRangePolicy::Sdr, DynamicRangePolicy::Hdr10],
                dual_player_preparation: false,
            }),
            observation: None,
            acknowledgement: None,
        }
    }

    #[test]
    fn strict_request_validation_rejects_unknown_and_non_finite_values() {
        let mut json = serde_json::to_value(request()).expect("request json");
        json.as_object_mut()
            .expect("object")
            .insert("surprise".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<ControlRequestV1>(json).is_err());

        let mut invalid = request();
        invalid.playback_rate = f64::NAN;
        assert_eq!(invalid.validate(Some(60_000), 2_000), Err("playback_rate"));

        let mut no_capabilities = request();
        no_capabilities.capabilities = None;
        assert_eq!(
            no_capabilities.validate(Some(60_000), 2_000),
            Err("capabilities"),
            "the first sequence snapshots bounded client capabilities"
        );

        let mut behind_playhead = request();
        behind_playhead.buffered_through_ms = behind_playhead.position_ms - 1;
        assert_eq!(
            behind_playhead.validate(Some(60_000), 2_000),
            Err("buffered_through_ms")
        );

        let mut disconnected = request();
        disconnected.buffered_from_ms = Some(disconnected.position_ms + 2_001);
        assert_eq!(
            disconnected.validate(Some(60_000), 2_000),
            Err("buffered_from_ms")
        );

        let mut code_without_detail = request();
        code_without_detail.observation = Some(ClientObservation {
            dropped_frames: None,
            decoder_state: Some(DecoderState::Failed),
            error_code: Some(ClientErrorCode::Decoder),
            error_detail: None,
        });
        assert_eq!(code_without_detail.validate(Some(60_000), 2_000), Ok(()));
    }

    #[test]
    fn bootstrap_uses_the_owning_registry_timeout_and_preserves_it_on_takeover() {
        let session = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let rolling = ControlBootstrap::new(&session, &generation, 1, ROLLING_LEASE_TIMEOUT_MS)
            .expect("rolling bootstrap");
        assert_eq!(rolling.lease_timeout_ms, ROLLING_LEASE_TIMEOUT_MS);
        let successor = rolling
            .refreshed(&session, &generation, 2)
            .expect("takeover bootstrap");
        assert_eq!(successor.control_epoch, 2);
        assert_eq!(successor.lease_timeout_ms, ROLLING_LEASE_TIMEOUT_MS);

        let vod = ControlBootstrap::new(&session, &generation, 1, VOD_LEASE_TIMEOUT_MS)
            .expect("VOD bootstrap");
        assert_eq!(vod.lease_timeout_ms, VOD_LEASE_TIMEOUT_MS);
        assert!(ControlBootstrap::new(&session, &generation, 1, 42_000).is_none());
        assert!(ControlBootstrap::new(&session, &generation, 0, VOD_LEASE_TIMEOUT_MS).is_none());
    }

    #[test]
    fn equal_sequence_replays_but_lower_sequence_and_other_client_are_stale() {
        let request = request();
        let mut state = ControlState::default();
        let started = Instant::now();
        assert_eq!(
            state.accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                Some(ClientPlatform::Web),
            ),
            Ok((
                ControlDisposition::Accepted,
                1,
                ControlAction::None,
                ClientPlatform::Web,
            ))
        );
        assert_eq!(
            state.accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                None,
            ),
            Ok((
                ControlDisposition::Replay,
                1,
                ControlAction::None,
                ClientPlatform::Web,
            ))
        );
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                0,
                None,
            ),
            Err(ControlStateError::StaleSequence)
        );
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &uuid::Uuid::new_v4().to_string(),
                2,
                Some(ClientPlatform::Apple),
            ),
            Err(ControlStateError::StaleClient)
        );
    }

    #[test]
    fn owner_epoch_rollover_resets_client_sequence_and_fences_old_owner() {
        let request = request();
        let mut state = ControlState::default();
        let started = Instant::now();
        state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                Some(ClientPlatform::Web),
            )
            .expect("epoch one");
        state
            .accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                8,
                None,
            )
            .expect("epoch one advance");
        let successor_client = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                2,
                &successor_client,
                1,
                Some(ClientPlatform::Apple),
            ),
            Ok((
                ControlDisposition::Accepted,
                1,
                ControlAction::None,
                ClientPlatform::Apple,
            ))
        );
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                9,
                None,
            ),
            Err(ControlStateError::OwnerChanged)
        );
    }

    #[test]
    fn first_sequence_is_one_and_rate_limit_does_not_advance_state() {
        let request = request();
        let mut state = ControlState::default();
        let started = Instant::now();
        assert_eq!(
            state.accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                Some(ClientPlatform::Web),
            ),
            Err(ControlStateError::StaleSequence)
        );
        state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                Some(ClientPlatform::Web),
            )
            .expect("first sequence");
        assert!(matches!(
            state.accept_at(
                started + Duration::from_millis(10),
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                None,
            ),
            Err(ControlStateError::RateLimited(_))
        ));
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                None,
            ),
            Ok((
                ControlDisposition::Accepted,
                2,
                ControlAction::None,
                ClientPlatform::Web,
            ))
        );
    }

    #[test]
    fn first_owner_sequence_requires_platform_and_platform_cannot_change_in_place() {
        let request = request();
        let started = Instant::now();
        let mut state = ControlState::default();
        assert_eq!(
            state.accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                None,
            ),
            Err(ControlStateError::StaleClient)
        );
        state
            .accept_at(
                started,
                &request.generation,
                1,
                &request.client_instance_id,
                1,
                Some(ClientPlatform::Web),
            )
            .expect("capability snapshot");
        assert_eq!(
            state.accept_at(
                started + MIN_CONTROL_INTERVAL,
                &request.generation,
                1,
                &request.client_instance_id,
                2,
                Some(ClientPlatform::Apple),
            ),
            Err(ControlStateError::StaleClient)
        );
    }

    fn relay_request() -> ControlRelayRequest {
        let control = request();
        ControlRelayRequest {
            session_id: uuid::Uuid::new_v4().to_string(),
            generation: control.generation.clone(),
            expected_owner_node_id: "node-a".to_owned(),
            expected_owner_epoch: 1,
            deadline_unix_ms: crate::media_sessions::unix_ms().saturating_add(4_000),
            control,
        }
    }

    #[test]
    fn relayed_exchange_inherits_remaining_budget_instead_of_restarting_it() {
        assert_eq!(
            inherited_exchange_budget(14_000, 13_000),
            Some(Duration::from_secs(1))
        );
        assert_eq!(inherited_exchange_budget(14_000, 14_000), None);
        assert_eq!(inherited_exchange_budget(14_000, 15_000), None);
        assert_eq!(
            inherited_exchange_budget(60_000, 10_000),
            Some(EXCHANGE_DEADLINE),
            "even a malformed future deadline cannot extend the public budget"
        );
    }

    #[test]
    fn relay_answers_are_schema_bounded_and_tuple_bound() {
        let request = relay_request();
        let response = ControlResponseV1 {
            protocol: PROTOCOL_V1.to_owned(),
            generation: request.generation.clone(),
            control_epoch: 1,
            accepted_sequence: 1,
            server_time_unix_ms: 1_000_000,
            lease: PlaybackLeaseView {
                state: "active".to_owned(),
                renew_after_ms: NEXT_EXCHANGE_MS,
                expires_at_unix_ms: 1_060_000,
            },
            delivery: DeliveryView {
                presentation: "vod".to_owned(),
                producer_state: "complete".to_owned(),
                produced_through_ms: Some(7_200_000),
                fetched_through_ms: 25_000,
                delivered_bps: None,
                delivered_idle_ms: None,
                recent_producer_speed: None,
                client_runway_ms: 15_000,
                admitted: Some(true),
                hold_reason: None,
                owner_node_hash: "n-0123456789abcdef".to_owned(),
                owner_epoch: 1,
            },
            effective_selection: EffectiveSelection {
                quality_auto: true,
                height: 1080,
                audio_track: Some(0),
                subtitle_burn: None,
                audio_offset_ms: 0,
                codec: "source".to_owned(),
                dynamic_range: Some("sdr".to_owned()),
            },
            action: ControlAction::None,
        };
        assert!(response.is_valid_for(&request));

        let mut wrong_epoch = response.clone();
        wrong_epoch.control_epoch = 2;
        assert!(!wrong_epoch.is_valid_for(&request));
        let mut invented_state = response;
        invented_state.delivery.producer_state = "probably_running".to_owned();
        assert!(!invented_state.is_valid_for(&request));

        let unavailable = ControlErrorBody {
            code: "control_unavailable".to_owned(),
            message: "owner deadline".to_owned(),
            generation: None,
            control_epoch: None,
            retry_after_ms: Some(500),
            invalid_field: None,
        };
        assert!(unavailable.is_valid_for_status(503));
        assert!(!unavailable.is_valid_for_status(409));
    }

    #[test]
    fn metrics_expose_bounded_platform_and_relay_dimensions() {
        let metrics = prometheus();
        assert!(metrics.contains(
            "plurx_playback_control_platform_exchanges_total{outcome=\"accepted\",platform=\"web\"}"
        ));
        assert!(
            metrics.contains("plurx_playback_control_relays_total{outcome=\"invalid_response\"}")
        );
        assert!(metrics.contains("# TYPE plurx_playback_control_relay_seconds histogram"));
    }

    async fn activate_route(
        store: &plurx_core::store::SqliteStore,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> (String, String) {
        use plurx_core::store::MediaSessionStore as _;

        let incarnation = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let fingerprint = "a".repeat(64);
        store
            .claim_media_session_request(
                7,
                &incarnation,
                &fingerprint,
                "player-a",
                &incarnation,
                now_ms,
                now_ms.saturating_add(60_000),
            )
            .await
            .expect("claim route");
        assert!(store
            .assign_media_session_request_owner(7, &incarnation, &incarnation, "node-a", now_ms,)
            .await
            .expect("assign owner"));
        store
            .activate_media_session(&plurx_core::domain::MediaSessionActivation {
                incarnation_id: incarnation.clone(),
                session_id: session.clone(),
                user_id: 7,
                playback_id: "player-a".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: Some(incarnation.clone()),
                request_fingerprint: fingerprint,
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms,
                lease_expires_at_ms,
            })
            .await
            .expect("activate route")
            .expect("activation accepted");
        (incarnation, session)
    }

    #[tokio::test]
    async fn durable_authority_fences_generation_owner_expiry_and_end() {
        use plurx_core::store::{MediaSessionStore as _, SqliteStore};

        let store = SqliteStore::open_in_memory().expect("store");
        let now = crate::media_sessions::unix_ms();
        let (generation, session) = activate_route(&store, now, now + 60_000).await;
        assert_eq!(
            verify_authority(&store, &session, &generation, "node-a", 1).await,
            Ok(())
        );
        assert_eq!(
            verify_authority(
                &store,
                &session,
                &uuid::Uuid::new_v4().to_string(),
                "node-a",
                1
            )
            .await,
            Err(ControlStateError::StaleGeneration)
        );
        assert_eq!(
            verify_authority(&store, &session, &generation, "node-b", 1).await,
            Err(ControlStateError::OwnerChanged)
        );
        assert_eq!(
            verify_authority(&store, &session, &generation, "node-a", 2).await,
            Err(ControlStateError::OwnerChanged)
        );

        store
            .end_media_session(&session, now + 1)
            .await
            .expect("end route")
            .expect("route existed");
        assert_eq!(
            verify_authority(&store, &session, &generation, "node-a", 1).await,
            Err(ControlStateError::SessionEnded)
        );

        let expired_store = SqliteStore::open_in_memory().expect("expired store");
        let (expired_generation, expired_session) = activate_route(&expired_store, 1, 2).await;
        assert_eq!(
            verify_authority(
                &expired_store,
                &expired_session,
                &expired_generation,
                "node-a",
                1,
            )
            .await,
            Err(ControlStateError::OwnerTransition)
        );
    }
}
