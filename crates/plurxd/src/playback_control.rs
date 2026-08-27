//! Versioned playback-control messages and per-generation sequence fencing.
//!
//! M1 is deliberately behavior-neutral: accepted exchanges renew the legacy
//! delivery clock and return an observation with `action: none`.  Keeping the
//! wire contract and the mutation fence here prevents later policy work from
//! leaking into HTTP routing or reintroducing several independent recovery
//! owners.

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
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
pub(crate) const ROLLING_EXPLICIT_LEASE_TIMEOUT_MS: u32 = 30_000;
pub(crate) const VOD_LEASE_TIMEOUT_MS: u32 = 300_000;
/// Bounded idempotency window for an accepted terminal response. This is
/// retained delivery evidence, not a playback or owner lease.
pub(crate) const TERMINAL_ACK_REPLAY_TTL_MS: i64 = 60_000;

const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const MAX_OBSERVED_DOWNLOAD_BPS: u64 = 10_000_000_000_000;
const MAX_ERROR_DETAIL_BYTES: usize = 512;
const MAX_CAPABILITY_VALUES: usize = 8;
const MIN_CONTROL_INTERVAL: Duration = Duration::from_millis(250);
const RELAY_BUCKETS_MS: [u64; 9] = [10, 25, 50, 100, 250, 500, 1_000, 2_500, 4_000];
/// Conservative compatibility default while the later M4 policy-admission
/// slice moves the existing hardware/software/copy startup budgets into the
/// actor. This deadline is action-passive: it records an exact provisional due
/// coordinate but does not retry, kill, replace, or otherwise compete with
/// legacy recovery.
const PRODUCER_STARTUP_BUDGET: Duration = Duration::from_secs(30);
/// Behavior-compatible advancing-output horizon used by the M4 producer
/// ingress proof and the actor's action-passive deadline.
const PRODUCER_PROGRESS_BUDGET: Duration = Duration::from_secs(10);
/// Bound for the action-passive exact-exit classification phase. The later
/// classifier slice will publish its typed result through the same ingress.
const PRODUCER_EXIT_CLASSIFICATION_BUDGET: Duration = Duration::from_secs(5);

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
        if self.sequence == 0 || self.sequence > i64::MAX as u64 {
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

    /// Canonical digest for binding a retained terminal acknowledgement to
    /// the complete parsed request, not merely to its sequence number.
    pub(crate) fn fingerprint(&self) -> Option<String> {
        let encoded = serde_json::to_vec(self).ok()?;
        Some(hex::encode(Sha256::digest(encoded)))
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
        let terminal_end = request.control.demand == PlaybackDemand::End;
        self.protocol == PROTOCOL_V1
            && self.generation == request.generation
            && u64::try_from(request.expected_owner_epoch).ok() == Some(self.control_epoch)
            && self.accepted_sequence > 0
            && if terminal_end {
                self.accepted_sequence == request.control.sequence
            } else {
                self.accepted_sequence <= request.control.sequence
            }
            && self.server_time_unix_ms > 0
            && self.lease.state == if terminal_end { "ended" } else { "active" }
            && self.lease.renew_after_ms == NEXT_EXCHANGE_MS
            && if terminal_end {
                self.lease.expires_at_unix_ms == self.server_time_unix_ms
            } else {
                self.lease.expires_at_unix_ms >= self.server_time_unix_ms
                    && self.lease.expires_at_unix_ms
                        <= self
                            .server_time_unix_ms
                            .saturating_add(i64::from(VOD_LEASE_TIMEOUT_MS))
            }
            && matches!(self.delivery.presentation.as_str(), "live-recovery" | "vod")
            && matches!(
                self.delivery.producer_state.as_str(),
                "running" | "held" | "complete" | "exited" | "failed" | "waiting" | "vod"
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
                    "demand" | "time" | "bytes" | "global" | "ahead" | "working_set" | "no_room"
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
        let buffer_anchor_ms = request.seek_target_ms.unwrap_or(request.position_ms);
        let client_runway_ms = request
            .buffered_through_ms
            .saturating_sub(buffer_anchor_ms)
            .max(0);
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
                        crate::transcode::AheadHoldReason::Demand => "demand",
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
            && self.control.validate(None, 30_000).is_ok()
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
    pub lease_state: &'static str,
    pub status: HlsSessionInfo,
    pub platform: ClientPlatform,
    /// Rolling delivery only: keeps process cleanup fenced until the exact
    /// terminal acknowledgement has reached durable storage.
    pub terminal_handoff: Option<TerminalResponseHandoff>,
    /// Shared result of the one session-owned durable terminal continuation.
    pub terminal_commit: Option<TerminalCommitReceipt>,
}

#[derive(Clone)]
pub(crate) struct TerminalResponseHandoff {
    pending: Arc<AtomicBool>,
}

impl TerminalResponseHandoff {
    pub(crate) fn new(pending: Arc<AtomicBool>) -> Self {
        pending.store(true, Ordering::Release);
        Self { pending }
    }

    pub(crate) fn complete(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub(crate) fn restart(&self) {
        self.pending.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct TerminalCommitReceipt {
    result: tokio::sync::watch::Receiver<Option<Result<ControlResponseV1, ()>>>,
    retry: Option<Arc<TerminalCommitRetry>>,
    expiry: Arc<TerminalCommitExpiry>,
}

struct TerminalCommitExpiry {
    expires_at_unix_ms: AtomicI64,
    deadline: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl TerminalCommitExpiry {
    fn new(expires_at_unix_ms: Option<i64>) -> Self {
        let expiry = Self {
            expires_at_unix_ms: AtomicI64::new(0),
            deadline: std::sync::Mutex::new(None),
        };
        if let Some(expires_at_unix_ms) = expires_at_unix_ms {
            expiry.set(expires_at_unix_ms);
        }
        expiry
    }

    fn set(&self, expires_at_unix_ms: i64) {
        let previous = self
            .expires_at_unix_ms
            .compare_exchange(0, expires_at_unix_ms, Ordering::AcqRel, Ordering::Acquire)
            .unwrap_or_else(|existing| existing);
        debug_assert!(previous == 0 || previous == expires_at_unix_ms);
        if previous != 0 {
            return;
        }
        let remaining_ms = expires_at_unix_ms.saturating_sub(crate::media_sessions::unix_ms());
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(
                u64::try_from(remaining_ms).unwrap_or(0),
            ))
            .unwrap_or_else(tokio::time::Instant::now);
        *self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline);
    }

    fn unix_ms(&self) -> Option<i64> {
        let expires_at_unix_ms = self.expires_at_unix_ms.load(Ordering::Acquire);
        (expires_at_unix_ms > 0).then_some(expires_at_unix_ms)
    }

    fn expired(&self) -> bool {
        self.deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
    }
}

struct TerminalCommitRetry {
    result: tokio::sync::watch::Sender<Option<Result<ControlResponseV1, ()>>>,
    running: Arc<AtomicBool>,
    transition: Arc<std::sync::Mutex<()>>,
    start: Arc<dyn Fn(TerminalCommitAttempt) + Send + Sync>,
    expiry: Arc<TerminalCommitExpiry>,
}

pub(crate) struct TerminalCommitAttempt {
    result: tokio::sync::watch::Sender<Option<Result<ControlResponseV1, ()>>>,
    running: Arc<AtomicBool>,
    transition: Arc<std::sync::Mutex<()>>,
    expiry: Arc<TerminalCommitExpiry>,
}

impl TerminalCommitAttempt {
    pub(crate) fn set_expires_at_unix_ms(&self, expires_at_unix_ms: i64) {
        self.expiry.set(expires_at_unix_ms);
    }

    pub(crate) fn complete(self, mut outcome: Result<ControlResponseV1, ()>) {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if outcome.is_ok() && self.expiry.expired() {
            outcome = Err(());
        }
        self.running.store(false, Ordering::Release);
        let _ = self.result.send(Some(outcome));
    }
}

impl TerminalCommitRetry {
    fn start_if_needed(&self) {
        let transition = Arc::clone(&self.transition);
        let _transition = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.expiry.expired() {
            self.running.store(false, Ordering::Release);
            let _ = self.result.send(Some(Err(())));
            return;
        }
        if self.result.borrow().as_ref().is_some_and(Result::is_ok)
            || self.running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let _ = self.result.send(None);
        let attempt = TerminalCommitAttempt {
            result: self.result.clone(),
            running: Arc::clone(&self.running),
            transition: Arc::clone(&transition),
            expiry: Arc::clone(&self.expiry),
        };
        drop(_transition);
        (self.start)(attempt);
    }
}

impl TerminalCommitReceipt {
    pub(crate) fn pending() -> (
        Self,
        tokio::sync::watch::Sender<Option<Result<ControlResponseV1, ()>>>,
    ) {
        let (sender, result) = tokio::sync::watch::channel(None);
        (
            Self {
                result,
                retry: None,
                expiry: Arc::new(TerminalCommitExpiry::new(None)),
            },
            sender,
        )
    }

    fn retryable_inner(
        start: impl Fn(TerminalCommitAttempt) + Send + Sync + 'static,
        start_now: bool,
        expires_at_unix_ms: Option<i64>,
    ) -> Self {
        let (result, receiver) = tokio::sync::watch::channel(None);
        let expiry = Arc::new(TerminalCommitExpiry::new(expires_at_unix_ms));
        let retry = Arc::new(TerminalCommitRetry {
            result,
            running: Arc::new(AtomicBool::new(false)),
            transition: Arc::new(std::sync::Mutex::new(())),
            start: Arc::new(start),
            expiry: Arc::clone(&expiry),
        });
        let receipt = Self {
            result: receiver,
            retry: Some(Arc::clone(&retry)),
            expiry,
        };
        if start_now {
            retry.start_if_needed();
        }
        receipt
    }

    pub(crate) fn retryable_until(
        expires_at_unix_ms: i64,
        start: impl Fn(TerminalCommitAttempt) + Send + Sync + 'static,
    ) -> Self {
        Self::retryable_inner(start, true, Some(expires_at_unix_ms))
    }

    pub(crate) fn deferred_retryable(
        start: impl Fn(TerminalCommitAttempt) + Send + Sync + 'static,
    ) -> Self {
        Self::retryable_inner(start, false, None)
    }

    #[cfg(test)]
    fn deferred_retryable_until(
        expires_at_unix_ms: i64,
        start: impl Fn(TerminalCommitAttempt) + Send + Sync + 'static,
    ) -> Self {
        Self::retryable_inner(start, false, Some(expires_at_unix_ms))
    }

    pub(crate) fn expires_at_unix_ms(&self) -> Option<i64> {
        self.expiry.unix_ms()
    }

    pub(crate) fn is_expired(&self) -> bool {
        self.expiry.expired()
    }

    #[cfg(test)]
    pub(crate) fn deadline_for_test(&self) -> tokio::time::Instant {
        self.expiry
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expect("terminal receipt must carry an exact deadline")
    }

    pub(crate) fn retry(&self) {
        if let Some(retry) = &self.retry {
            retry.start_if_needed();
        }
    }

    pub(crate) async fn wait(&self) -> Result<ControlResponseV1, ()> {
        let mut result = self.result.clone();
        loop {
            if let Some(mut outcome) = result.borrow().clone() {
                if outcome.is_ok() && self.expiry.expired() {
                    outcome = Err(());
                }
                return outcome;
            }
            result.changed().await.map_err(|_| ())?;
        }
    }
}

pub(crate) trait TerminalControlCommitter: Send + Sync {
    /// Start the continuation synchronously. The returned receipt may be
    /// awaited by HTTP, but dropping every waiter cannot cancel the commit.
    fn start(&self, result: &LocalControlResult) -> TerminalCommitReceipt;
}

#[cfg(test)]
pub(crate) fn terminal_response_for_test(result: &LocalControlResult) -> ControlResponseV1 {
    ControlResponseV1 {
        protocol: PROTOCOL_V1.to_owned(),
        generation: "test-terminal-generation".to_owned(),
        control_epoch: 1,
        accepted_sequence: result.accepted_sequence,
        server_time_unix_ms: 1,
        lease: PlaybackLeaseView {
            state: "ended".to_owned(),
            renew_after_ms: NEXT_EXCHANGE_MS,
            expires_at_unix_ms: 1,
        },
        delivery: DeliveryView {
            presentation: "test".to_owned(),
            producer_state: "complete".to_owned(),
            produced_through_ms: None,
            fetched_through_ms: 0,
            delivered_bps: None,
            delivered_idle_ms: None,
            recent_producer_speed: None,
            client_runway_ms: 0,
            admitted: None,
            hold_reason: None,
            owner_node_hash: "n-test".to_owned(),
            owner_epoch: 1,
        },
        effective_selection: EffectiveSelection {
            quality_auto: true,
            height: 720,
            audio_track: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "test".to_owned(),
            dynamic_range: Some("sdr".to_owned()),
        },
        action: result.action.clone(),
    }
}

#[cfg(test)]
pub(crate) struct RecoveringTerminalCommitter {
    attempts: Arc<AtomicUsize>,
    retry_pause: Option<Arc<tokio::sync::Barrier>>,
}

#[cfg(test)]
impl RecoveringTerminalCommitter {
    pub(crate) fn with_retry_pause(retry_pause: Arc<tokio::sync::Barrier>) -> Arc<Self> {
        Arc::new(Self {
            attempts: Arc::new(AtomicUsize::new(0)),
            retry_pause: Some(retry_pause),
        })
    }

    pub(crate) fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Acquire)
    }
}

#[cfg(test)]
impl TerminalControlCommitter for RecoveringTerminalCommitter {
    fn start(&self, result: &LocalControlResult) -> TerminalCommitReceipt {
        let response = terminal_response_for_test(result);
        let attempts = Arc::clone(&self.attempts);
        let handoff = result.terminal_handoff.clone();
        let retry_pause = self.retry_pause.clone();
        TerminalCommitReceipt::retryable_until(
            crate::media_sessions::unix_ms().saturating_add(TERMINAL_ACK_REPLAY_TTL_MS),
            move |attempt| {
                if let Some(handoff) = &handoff {
                    handoff.restart();
                }
                let index = attempts.fetch_add(1, Ordering::AcqRel);
                if index == 1 {
                    if let Some(retry_pause) = retry_pause.clone() {
                        let response = response.clone();
                        let handoff = handoff.clone();
                        tokio::spawn(async move {
                            retry_pause.wait().await;
                            retry_pause.wait().await;
                            if let Some(handoff) = handoff {
                                handoff.complete();
                            }
                            attempt.complete(Ok(response));
                        });
                        return;
                    }
                }
                if let Some(handoff) = &handoff {
                    handoff.complete();
                }
                attempt.complete(if index == 0 {
                    Err(())
                } else {
                    Ok(response.clone())
                });
            },
        )
    }
}

#[derive(Clone)]
pub(crate) struct LocalControlRequest<'a> {
    pub session_id: &'a str,
    pub generation: &'a str,
    pub owner_node_id: &'a str,
    pub owner_epoch: u64,
    pub client_instance_id: &'a str,
    pub sequence: u64,
    pub snapshot: PlaybackDemandSnapshot,
}

/// The bounded client facts accepted with one control sequence.
///
/// The public request contains identity and mutation fences as well as these
/// observations.  Identity remains on [`LocalControlRequest`]; this owned
/// projection is what a rolling-session actor may retain after the HTTP
/// request has returned.  Keeping every policy input together prevents later
/// quality, subtitle, and handoff work from reconstructing state from media
/// fetches or from whichever legacy watchdog happened to fire.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlaybackDemandSnapshot {
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
}

impl From<&ControlRequestV1> for PlaybackDemandSnapshot {
    fn from(request: &ControlRequestV1) -> Self {
        Self {
            demand: request.demand,
            position_ms: request.position_ms,
            buffered_from_ms: request.buffered_from_ms,
            buffered_through_ms: request.buffered_through_ms,
            playback_rate: request.playback_rate,
            render_state: request.render_state,
            seek_target_ms: request.seek_target_ms,
            observed_download_bps: request.observed_download_bps,
            selection: request.selection.clone(),
            capabilities: request.capabilities.clone(),
            observation: request.observation.clone(),
        }
    }
}

impl PlaybackDemandSnapshot {
    pub(crate) fn buffer_anchor_ms(&self) -> i64 {
        self.seek_target_ms.unwrap_or(self.position_ms)
    }

    pub(crate) fn runway_ms(&self) -> i64 {
        self.buffered_through_ms
            .saturating_sub(self.buffer_anchor_ms())
            .max(0)
    }

    pub(crate) fn platform(&self) -> Option<ClientPlatform> {
        self.capabilities.as_ref().map(|caps| caps.platform)
    }

    #[cfg(test)]
    pub(crate) fn test_default(platform: ClientPlatform) -> Self {
        Self {
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
                platform,
                max_height: 2160,
                codecs: vec![CodecPolicy::H264, CodecPolicy::Hevc],
                dynamic_ranges: vec![DynamicRangePolicy::Sdr, DynamicRangePolicy::Hdr10],
                dual_player_preparation: false,
            }),
            observation: None,
        }
    }
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

    /// Recover the immutable result for the exact accepted identity/sequence
    /// without advancing any fence. Terminal sessions use this after their
    /// ordinary mutation path has closed so a lost `demand=end` response can
    /// still be retried idempotently.
    pub(crate) fn replay_exact(
        &self,
        generation: &str,
        owner_epoch: u64,
        client_instance_id: &str,
        sequence: u64,
        platform: Option<ClientPlatform>,
    ) -> Option<(ControlDisposition, u64, ControlAction, ClientPlatform)> {
        let client_instance_id = uuid::Uuid::parse_str(client_instance_id).ok()?;
        let client_platform = self.client_platform?;
        (self.generation.as_deref() == Some(generation)
            && self.owner_epoch == owner_epoch
            && self.client_instance_id == Some(client_instance_id)
            && self.last_sequence == sequence
            && platform.is_none_or(|platform| platform == client_platform))
        .then(|| {
            (
                ControlDisposition::Replay,
                self.last_sequence,
                self.prior_action.clone(),
                client_platform,
            )
        })
    }
}

const ROLLING_ACTOR_MAILBOX_CAPACITY: usize = 128;
/// One complete physical hold/resume cycle may wait behind actor dispatch.
/// A third transition waits before its syscall until the actor drains one of
/// these fixed barrier slots, then re-authorizes its exact producer attempt.
const ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY: usize = 2;
const ROLLING_LEGACY_LEASE_TIMEOUT: Duration =
    Duration::from_millis(ROLLING_LEASE_TIMEOUT_MS as u64);
const ROLLING_EXPLICIT_LEASE_TIMEOUT: Duration =
    Duration::from_millis(ROLLING_EXPLICIT_LEASE_TIMEOUT_MS as u64);

/// Use the runtime's monotonic clock for every actor-owned lease transition.
/// In production it shares `std::time::Instant`'s epoch; using the runtime
/// view also keeps the actor and its `sleep_until` timer on one clock when
/// Tokio time is paused or advanced in deterministic tests.
fn rolling_now() -> Instant {
    tokio::time::Instant::now().into_std()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RollingLeaseMode {
    Legacy,
    Explicit,
}

impl RollingLeaseMode {
    fn timeout(self) -> Duration {
        match self {
            Self::Legacy => ROLLING_LEGACY_LEASE_TIMEOUT,
            Self::Explicit => ROLLING_EXPLICIT_LEASE_TIMEOUT,
        }
    }

    pub(crate) fn timeout_ms(self) -> u32 {
        match self {
            Self::Legacy => ROLLING_LEASE_TIMEOUT_MS,
            Self::Explicit => ROLLING_EXPLICIT_LEASE_TIMEOUT_MS,
        }
    }
}

/// One actor-owned view of a rolling generation's liveness and demand.
///
/// `last_renewal_kind` is retained for the existing Activity/status contract,
/// but expiry is decided inside the actor.  Callers cannot inspect a timestamp
/// and later retire based on that stale observation; they must atomically
/// claim an expired lease through [`RollingControlHandle::claim_expiry`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RollingLeaseSnapshot {
    pub mode: RollingLeaseMode,
    pub idle_for: Duration,
    pub remaining: Duration,
    deadline: Instant,
    pub last_renewal_kind: &'static str,
    pub demand: Option<PlaybackDemandSnapshot>,
    pub delivery: RollingDeliverySnapshot,
    pub retired: bool,
    /// Immutable cause of the actor's first terminal transition. `None` is
    /// live; later end/fence events cannot relabel the winning cause.
    pub terminal: Option<RollingTerminalCause>,
    /// True only when the playback deadline, rather than an explicit
    /// lifecycle fence, performed the actor's terminal transition.
    pub expiration_claimed: bool,
    /// Bounded, actor-owned rolling producer truth. This is observation-only
    /// until the later M4 decision/executor cutover removes the compatibility
    /// recovery owners.
    pub producer_control: RollingProducerOperationalSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RollingProducerOperationalSnapshot {
    pub phase: &'static str,
    pub deadline_attempt: Option<u64>,
    pub deadline_mode: Option<&'static str>,
    pub deadline_remaining_ms: Option<i64>,
    pub due_attempt: Option<u64>,
    pub due_mode: Option<&'static str>,
    pub due_overdue_ms: Option<i64>,
    pub process_exit_attempt: Option<u64>,
    pub process_exit_due: bool,
    pub physical_flow: &'static str,
    /// Ingress coordinate of the last exact physical-flow acknowledgement.
    /// This is not the later desired-flow/action revision.
    pub last_flow_applied_sequence: u64,
    /// Global ingress coordinate of the last producer fact or command applied
    /// by the actor. It is ordering evidence, not a desired-flow revision.
    pub last_applied_sequence: u64,
    pub observation_only: bool,
    pub action_owner: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RollingTerminalCause {
    End,
    AuthorityFence,
    LeaseExpired,
}

impl RollingTerminalCause {
    pub(crate) fn status(self) -> &'static str {
        match self {
            Self::End => "ended",
            Self::AuthorityFence => "authority_fenced",
            Self::LeaseExpired => "expired",
        }
    }

    fn metric_index(self) -> usize {
        match self {
            Self::End => 0,
            Self::AuthorityFence => 1,
            Self::LeaseExpired => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RollingTerminalOutcome {
    Won(RollingTerminalCause),
    AlreadyTerminal(RollingTerminalCause),
}

impl RollingTerminalOutcome {
    #[cfg(test)]
    fn won(&self) -> bool {
        matches!(self, Self::Won(_))
    }

    pub(crate) fn cause(&self) -> RollingTerminalCause {
        match self {
            Self::Won(cause) | Self::AlreadyTerminal(cause) => *cause,
        }
    }
}

/// Actor-owned delivery facts for one rolling producer attempt.
///
/// These are observations only in M3c: the compatibility segment index still
/// drives pruning and flow actions until every producer event has entered the
/// actor. Keeping the frontiers here first makes status and later decisions a
/// single ordered snapshot instead of a collection of independently sampled
/// atomics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RollingDeliverySnapshot {
    pub producer_attempt: u64,
    /// Latest actor-accepted ffmpeg timeline coordinate for this exact
    /// attempt. Producer telemetry is deliberately distinct from publication:
    /// frames can still be moving while the current segment is incomplete.
    pub producer_out_time_ms: Option<i64>,
    /// Cumulative and recent encode rates, scaled by 1,000 to keep the actor
    /// state exact and equality-friendly (`1.85x` is `1850`).
    pub producer_speed_milli: Option<i64>,
    pub producer_recent_speed_milli: Option<i64>,
    /// Wall-clock age of the last advancing output timestamp. This begins at
    /// attempt admission, so a producer that never emits telemetry is measured
    /// by the same coordinate as one that later stops.
    pub producer_progress_idle_ms: i64,
    /// Exact-attempt terminal process observation. Recovery remains with the
    /// compatibility watchdog until M4; this is an ordered fact only.
    pub producer_exit: Option<RollingProducerExitSnapshot>,
    pub playlist_ready: bool,
    pub published_segment: Option<i64>,
    pub published_end_ms: Option<i64>,
    pub next_media_sequence: i64,
    pub fetched_segment: Option<i64>,
    pub fetched_end_ms: i64,
    pub pending_fetched_segment: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RollingProducerExitSnapshot {
    pub success: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub observed_idle_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RollingProducerProgressObservation {
    producer_attempt: u64,
    out_time_ms: Option<i64>,
    speed_milli: Option<i64>,
    recent_speed_milli: Option<i64>,
    observed_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RollingProducerExitObservation {
    producer_attempt: u64,
    success: bool,
    code: Option<i32>,
    signal: Option<i32>,
    observed_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RollingProducerFlowObservation {
    producer_attempt: u64,
    state: ProducerPhysicalFlowState,
}

/// The actor's sole rolling-producer candidate clock. This slice deliberately
/// records expiry without emitting a recovery action, so the compatibility
/// watchdog remains the only component that can still retry or replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerProgressDeadlineMode {
    Starting,
    Advancing,
    ClassifyingExit,
}

impl ProducerProgressDeadlineMode {
    fn metric(self) -> (usize, &'static str) {
        match self {
            Self::Starting => (0, "starting"),
            Self::Advancing => (1, "advancing"),
            Self::ClassifyingExit => (2, "classifying_exit"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerProgressDeadline {
    producer_attempt: u64,
    mode: ProducerProgressDeadlineMode,
    instant: Instant,
}

/// Provisional action-passive deadline observation retained when the exact
/// armed clock becomes due. It is deliberately not a `ProducerDecision`:
/// commands and producer facts now share ingress order, but the later M4
/// decision/executor slice still owns whether this observation causes an
/// action. Terminal or exact physical-flow facts may still revoke it first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerDeadlineDue {
    producer_attempt: u64,
    mode: ProducerProgressDeadlineMode,
    deadline: Instant,
}

/// Immediate typed observation for a non-success producer exit. The later
/// decision/publication slice turns this into `ProducerDecision::Fail`; this
/// slice records no action and leaves every compatibility owner untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerProcessExitDue {
    producer_attempt: u64,
    published_at: Instant,
    code: Option<i32>,
    signal: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerExitAcceptance {
    Rejected,
    Duplicate,
    Accepted,
}

impl ProducerExitAcceptance {
    #[cfg(test)]
    fn accepted(self) -> bool {
        !matches!(self, Self::Rejected)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerPhysicalFlowState {
    Running,
    Held,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerFlowApplied {
    revision: u64,
    producer_attempt: u64,
    state: ProducerPhysicalFlowState,
    published_at: Instant,
}

/// Synchronous boundary shared by lease/producer cutoff and the sole process
/// owner. A successful signal acknowledgement is written while the PID is
/// still authorized, before the guard is released.
pub(crate) struct RollingProducerTransitionFence {
    lease_deadline: Instant,
}

impl RollingProducerTransitionFence {
    fn new(lease_deadline: Instant) -> Self {
        Self { lease_deadline }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RollingPublicationObservation {
    pub producer_attempt: u64,
    pub playlist_ready: bool,
    pub published_segment: Option<i64>,
    pub published_end_ms: Option<i64>,
    pub next_media_sequence: i64,
    /// End time for the actor's currently pending fetched segment, when the
    /// refreshed playlist has now supplied its EXTINF.
    pub resolved_fetched_segment: Option<i64>,
    pub resolved_fetched_end_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProducerAttemptRejection {
    SessionEnded,
    PlaylistPublished,
    StaleAttempt,
    AttemptExhausted,
    ControlUnavailable,
}

impl RollingLeaseSnapshot {
    pub(crate) fn expired(&self) -> bool {
        !self.retired && self.remaining.is_zero()
    }

    pub(crate) fn timeout_ms(&self) -> u32 {
        self.mode.timeout_ms()
    }

    pub(crate) fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms_at(rolling_now(), crate::media_sessions::unix_ms())
    }

    fn expires_at_unix_ms_at(&self, now: Instant, now_unix_ms: i64) -> i64 {
        let remaining = self.deadline.saturating_duration_since(now);
        let remaining_ms = i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX);
        now_unix_ms.saturating_add(remaining_ms)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RollingExpiryClaim {
    Live,
    Claimed(RollingLeaseSnapshot),
    Retired(RollingLeaseSnapshot),
}

#[derive(Clone, Copy)]
enum RollingRenewalSource {
    Media,
    Internal,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RollingControlOutcome {
    pub disposition: ControlDisposition,
    pub accepted_sequence: u64,
    pub action: ControlAction,
    pub platform: ClientPlatform,
    pub lease: RollingLeaseSnapshot,
    pub flow_ticket: u64,
}

/// A session-owned continuation installed synchronously by the rolling actor
/// after it has accepted (or exactly replayed) `demand=end`, but before the
/// actor attempts to answer the request waiter.  This is the terminal
/// ownership boundary: losing the HTTP future or its oneshot reply cannot
/// leave an accepted End without a cleanup and durable-ack owner.
pub(crate) trait RollingTerminalAdmission: Send + Sync {
    fn accepted(&self, outcome: RollingControlOutcome);
}

struct RollingFlowSync {
    requested: AtomicU64,
    applied: AtomicU64,
    request_notify: tokio::sync::Notify,
    applied_notify: tokio::sync::Notify,
    #[cfg(test)]
    wait_after_check: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl RollingFlowSync {
    fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            request_notify: tokio::sync::Notify::new(),
            applied_notify: tokio::sync::Notify::new(),
            #[cfg(test)]
            wait_after_check: std::sync::Mutex::new(None),
        }
    }

    fn request(&self) -> u64 {
        let ticket = self.requested.fetch_add(1, Ordering::AcqRel) + 1;
        self.request_notify.notify_one();
        ticket
    }

    async fn next(&self, handled: u64) -> u64 {
        loop {
            let requested = self.requested.load(Ordering::Acquire);
            if requested > handled {
                return requested;
            }
            self.request_notify.notified().await;
        }
    }

    fn complete(&self, ticket: u64) {
        self.applied.fetch_max(ticket, Ordering::AcqRel);
        self.applied_notify.notify_waiters();
    }

    async fn wait_for(&self, ticket: u64) {
        loop {
            let notified = self.applied_notify.notified();
            tokio::pin!(notified);
            // `notify_waiters` does not retain a permit. Register this waiter
            // before checking the monotonic generation so completion cannot
            // land in the check-to-await gap and strand a control response.
            notified.as_mut().enable();
            if self.applied.load(Ordering::Acquire) >= ticket {
                return;
            }
            #[cfg(test)]
            let pause = self
                .wait_after_check
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            #[cfg(test)]
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
            notified.as_mut().await;
        }
    }
}

/// A bounded, coalescing producer-event ingress coordinated with the actor's
/// bounded command mailbox.
///
/// ffmpeg progress is read from a pipe that must never stop draining because
/// a control request or filesystem observation filled the mailbox. Publishing
/// here takes one short synchronous mutex and wakes the actor; it never awaits.
/// Progress is retained as a constant-space deadline chain instead of only a
/// first/latest sample: a delayed drain can therefore prove that every A-B-C
/// link arrived inside the preceding progress horizon, while the first missing
/// link is retained and later telemetry cannot bridge it. Exit and successful
/// exact-attempt physical-flow acknowledgements seal the open batch into
/// barrier blocks, so progress cannot fold across process death or a physical
/// hold/resume transition. Command publication seals all preceding blocks
/// under the transition plus ingress fence and assigns the next coordinate;
/// later producer facts therefore cannot leapfrog a queued command. This
/// shared ordering remains action-passive until the later M4 cutover.
struct RollingProducerIngress {
    state: std::sync::Mutex<RollingProducerIngressState>,
    notify: tokio::sync::Notify,
    flow_capacity_available: tokio::sync::Notify,
    #[cfg(test)]
    flow_capacity_wait_started: tokio::sync::Notify,
}

#[derive(Default)]
struct RollingProducerIngressState {
    next_sequence: u64,
    /// Greatest timeline coordinate already admitted to some current/open
    /// batch for this exact attempt. It deliberately survives actor drains:
    /// a repeated timestamp in the next batch is speed telemetry, not the
    /// first unconsumed increase that may establish another deadline chain.
    progress_watermark_attempt: u64,
    progress_watermark_out_time_ms: Option<i64>,
    progress: Option<ProgressCoverageBatch>,
    exit: Option<SequencedProducerBarrier>,
    flow: std::collections::VecDeque<SequencedProducerBarrier>,
    flow_reservations: usize,
    /// Applied flow barriers moved into queued command envelopes still occupy
    /// their fixed slots until the actor consumes that envelope.
    sealed_flow_barriers: usize,
    #[cfg(test)]
    last_flow_applied: Option<RollingProducerFlowObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SequencedProducerEvent {
    sequence: u64,
    published_at: Instant,
    event: RollingProducerEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RollingProducerEvent {
    Progress(RollingProducerProgressObservation),
    Exit(RollingProducerExitObservation),
    FlowApplied(RollingProducerFlowObservation),
}

impl RollingProducerEvent {
    fn producer_attempt(&self) -> u64 {
        match self {
            Self::Progress(observation) => observation.producer_attempt,
            Self::Exit(observation) => observation.producer_attempt,
            Self::FlowApplied(observation) => observation.producer_attempt,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishedProgress {
    sequence: u64,
    published_at: Instant,
    observation: RollingProducerProgressObservation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressCoverageBatch {
    producer_attempt: u64,
    first_advancing: Option<PublishedProgress>,
    covered_last: Option<PublishedProgress>,
    covered_deadline: Option<Instant>,
    first_gap: Option<PublishedProgress>,
    latest_progress: Option<PublishedProgress>,
    latest_telemetry: PublishedProgress,
}

impl ProgressCoverageBatch {
    fn new(sample: PublishedProgress) -> Self {
        let producer_attempt = sample.observation.producer_attempt;
        let advancing = sample
            .observation
            .out_time_ms
            .is_some_and(|out_time_ms| out_time_ms >= 0);
        let first_advancing = advancing.then(|| sample.clone());
        let covered_last = first_advancing.clone();
        let latest_progress = first_advancing.clone();
        let covered_deadline = covered_last.as_ref().map(|progress| {
            progress
                .published_at
                .checked_add(PRODUCER_PROGRESS_BUDGET)
                .unwrap_or(progress.published_at)
        });
        Self {
            producer_attempt,
            first_advancing,
            covered_last,
            covered_deadline,
            first_gap: None,
            latest_progress,
            latest_telemetry: sample,
        }
    }

    fn first_sequence(&self) -> u64 {
        self.first_advancing
            .as_ref()
            .map_or(self.latest_telemetry.sequence, |progress| progress.sequence)
    }

    fn first_published_at(&self) -> Instant {
        self.first_advancing
            .as_ref()
            .map_or(self.latest_telemetry.published_at, |progress| {
                progress.published_at
            })
    }

    fn last_sequence(&self) -> u64 {
        self.latest_telemetry.sequence
    }

    fn push(&mut self, mut sample: PublishedProgress) {
        debug_assert_eq!(self.producer_attempt, sample.observation.producer_attempt);
        sample.observation.speed_milli = sample
            .observation
            .speed_milli
            .or(self.latest_telemetry.observation.speed_milli);
        sample.observation.recent_speed_milli = sample
            .observation
            .recent_speed_milli
            .or(self.latest_telemetry.observation.recent_speed_milli);
        let advancing = sample
            .observation
            .out_time_ms
            .filter(|out_time_ms| *out_time_ms >= 0)
            .is_some_and(|out_time_ms| {
                self.covered_last
                    .as_ref()
                    .and_then(|progress| progress.observation.out_time_ms)
                    .is_none_or(|covered| out_time_ms > covered)
            });

        if advancing {
            self.latest_progress = Some(sample.clone());
            if self.first_advancing.is_none() {
                self.first_advancing = Some(sample.clone());
                self.covered_last = Some(sample.clone());
                self.covered_deadline = Some(
                    sample
                        .published_at
                        .checked_add(PRODUCER_PROGRESS_BUDGET)
                        .unwrap_or(sample.published_at),
                );
            } else if self.first_gap.is_none()
                && self
                    .covered_deadline
                    .is_some_and(|deadline| sample.published_at <= deadline)
            {
                self.covered_last = Some(sample.clone());
                self.covered_deadline = Some(
                    sample
                        .published_at
                        .checked_add(PRODUCER_PROGRESS_BUDGET)
                        .unwrap_or(sample.published_at),
                );
            } else if self.first_gap.is_none() {
                self.first_gap = Some(sample.clone());
            }
        }
        self.latest_telemetry = sample;
    }

    /// Preserve today's one-coalesced-observation actor behavior while the
    /// active deadline and due-first cutoff are introduced in the next slice.
    /// The complete deadline-chain evidence remains in the batch until this
    /// projection is made at actor drain.
    fn into_observations(self) -> Vec<RollingProducerProgressObservation> {
        let latest = self.latest_telemetry.observation;
        let mut observation = self
            .latest_progress
            .or(self.first_gap)
            .or(self.covered_last)
            .or(self.first_advancing)
            .map_or_else(|| latest.clone(), |progress| progress.observation);
        observation.speed_milli = latest.speed_milli;
        observation.recent_speed_milli = latest.recent_speed_milli;
        vec![observation]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SequencedProducerBarrier {
    preceding_progress: Option<ProgressCoverageBatch>,
    event: SequencedProducerEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RollingProducerIngressBlock {
    Progress(ProgressCoverageBatch),
    Barrier(SequencedProducerBarrier),
}

impl RollingProducerIngressBlock {
    fn order_key(&self) -> (u64, Instant) {
        match self {
            Self::Progress(batch) => (batch.first_sequence(), batch.first_published_at()),
            Self::Barrier(barrier) => (barrier.event.sequence, barrier.event.published_at),
        }
    }

    #[cfg(test)]
    fn into_events(self) -> Vec<RollingProducerEvent> {
        match self {
            Self::Progress(batch) => batch
                .into_observations()
                .into_iter()
                .map(RollingProducerEvent::Progress)
                .collect(),
            Self::Barrier(barrier) => {
                let mut events = barrier
                    .preceding_progress
                    .into_iter()
                    .flat_map(ProgressCoverageBatch::into_observations)
                    .map(RollingProducerEvent::Progress)
                    .collect::<Vec<_>>();
                events.push(barrier.event.event);
                events
            }
        }
    }
}

impl RollingProducerIngress {
    fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(RollingProducerIngressState::default()),
            notify: tokio::sync::Notify::new(),
            flow_capacity_available: tokio::sync::Notify::new(),
            #[cfg(test)]
            flow_capacity_wait_started: tokio::sync::Notify::new(),
        }
    }

    fn publish_progress(&self, observation: RollingProducerProgressObservation) {
        self.publish(RollingProducerEvent::Progress(observation), false);
    }

    fn publish_exit(&self, observation: RollingProducerExitObservation) {
        self.publish(RollingProducerEvent::Exit(observation), true);
    }

    fn reserve_flow_barrier(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.flow.len() + state.flow_reservations + state.sealed_flow_barriers
            >= ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY
        {
            ROLLING_PRODUCER_FLOW_DEFERRALS.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        state.flow_reservations += 1;
        true
    }

    /// Wait until a fixed flow-barrier slot is available. The caller must
    /// re-acquire the transition fence and reserve after this returns: space
    /// is only a wake condition, never authorization for a process syscall.
    async fn wait_for_flow_barrier_capacity(
        &self,
        retired: &AtomicBool,
        actor: &tokio::sync::mpsc::Sender<RollingControlEnvelope>,
    ) -> bool {
        loop {
            let notified = self.flow_capacity_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if retired.load(Ordering::Acquire) || actor.is_closed() {
                return false;
            }
            let has_capacity = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.flow.len() + state.flow_reservations + state.sealed_flow_barriers
                    < ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY
            };
            if has_capacity {
                return true;
            }
            #[cfg(test)]
            self.flow_capacity_wait_started.notify_one();
            tokio::select! {
                _ = notified.as_mut() => {}
                _ = actor.closed() => return false,
            }
        }
    }

    fn notify_flow_capacity_waiters(&self) {
        self.flow_capacity_available.notify_waiters();
    }

    fn finish_reserved_flow_barrier(
        &self,
        observation: RollingProducerFlowObservation,
        applied: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.flow_reservations > 0);
        state.flow_reservations = state.flow_reservations.saturating_sub(1);
        if !applied {
            drop(state);
            self.flow_capacity_available.notify_one();
            return;
        }
        let published_at = rolling_now();
        Self::push_flow_barrier(&mut state, observation, published_at);
        drop(state);
        self.notify.notify_one();
    }

    fn push_flow_barrier(
        state: &mut RollingProducerIngressState,
        observation: RollingProducerFlowObservation,
        published_at: Instant,
    ) {
        debug_assert!(state.flow.len() < ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY);
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;
        let preceding_progress = state.progress.take();
        #[cfg(test)]
        {
            state.last_flow_applied = Some(observation);
        }
        state.flow.push_back(SequencedProducerBarrier {
            preceding_progress,
            event: SequencedProducerEvent {
                sequence,
                published_at,
                event: RollingProducerEvent::FlowApplied(observation),
            },
        });
    }

    #[cfg(test)]
    fn publish_flow_at(&self, observation: RollingProducerFlowObservation, published_at: Instant) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.flow.len() < ROLLING_PRODUCER_FLOW_BARRIER_CAPACITY);
        Self::push_flow_barrier(&mut state, observation, published_at);
        drop(state);
        self.notify.notify_one();
    }

    fn publish(&self, event: RollingProducerEvent, is_exit: bool) {
        self.publish_with_timestamp(event, is_exit, None);
    }

    #[cfg(test)]
    fn publish_at(&self, event: RollingProducerEvent, is_exit: bool, published_at: Instant) {
        self.publish_with_timestamp(event, is_exit, Some(published_at));
    }

    fn publish_with_timestamp(
        &self,
        mut event: RollingProducerEvent,
        is_exit: bool,
        published_at: Option<Instant>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The publication coordinate is allocated only after the ingress
        // fence is held. A producer task cannot obtain a pre-deadline stamp,
        // lose the CPU, and insert the event after that deadline.
        let published_at = published_at.unwrap_or_else(rolling_now);
        let metric_index = usize::from(is_exit);
        ROLLING_PRODUCER_EVENT_INGRESS[metric_index].fetch_add(1, Ordering::Relaxed);
        if let RollingProducerEvent::Progress(observation) = &mut event {
            let attempt = observation.producer_attempt;
            if attempt < state.progress_watermark_attempt {
                ROLLING_PRODUCER_EVENT_COALESCED[metric_index].fetch_add(1, Ordering::Relaxed);
                return;
            }
            if attempt > state.progress_watermark_attempt {
                state.progress_watermark_attempt = attempt;
                state.progress_watermark_out_time_ms = None;
            }
            if let Some(out_time_ms) = observation.out_time_ms.filter(|value| *value >= 0) {
                if state
                    .progress_watermark_out_time_ms
                    .is_some_and(|watermark| out_time_ms <= watermark)
                {
                    // Repeated or regressing output is telemetry, not a new
                    // deadline link. Keep speed/status in the open batch but
                    // do not let this sample initialize or extend coverage.
                    observation.out_time_ms = None;
                } else {
                    state.progress_watermark_out_time_ms = Some(out_time_ms);
                }
            }
        }
        let incoming_attempt = event.producer_attempt();
        if is_exit {
            if let Some(pending) = state.exit.as_ref() {
                // An attempt has exactly one terminal outcome. Preserve the
                // first observation even if a contradictory waiter or test
                // source reports another; only a later attempt can replace
                // a pending predecessor.
                if pending.event.event.producer_attempt() >= incoming_attempt {
                    ROLLING_PRODUCER_EVENT_COALESCED[metric_index].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                ROLLING_PRODUCER_EVENT_COALESCED[metric_index].fetch_add(1, Ordering::Relaxed);
            }
        } else if let Some(pending_attempt) = state
            .progress
            .as_ref()
            .map(|pending| pending.producer_attempt)
        {
            if pending_attempt > incoming_attempt {
                return;
            }
            ROLLING_PRODUCER_EVENT_COALESCED[metric_index].fetch_add(1, Ordering::Relaxed);
            if pending_attempt == incoming_attempt {
                let RollingProducerEvent::Progress(incoming) = event else {
                    unreachable!("progress slot accepts only progress events");
                };
                state.next_sequence = state.next_sequence.saturating_add(1);
                let sequence = state.next_sequence;
                state
                    .progress
                    .as_mut()
                    .expect("merged progress batch")
                    .push(PublishedProgress {
                        sequence,
                        published_at,
                        observation: incoming,
                    });
                drop(state);
                self.notify.notify_one();
                return;
            }
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;
        if is_exit {
            let preceding_progress = state.progress.take();
            state.exit = Some(SequencedProducerBarrier {
                preceding_progress,
                event: SequencedProducerEvent {
                    sequence,
                    published_at,
                    event,
                },
            });
        } else {
            let RollingProducerEvent::Progress(observation) = event else {
                unreachable!("progress publication contains a progress event");
            };
            state.progress = Some(ProgressCoverageBatch::new(PublishedProgress {
                sequence,
                published_at,
                observation,
            }));
        }
        drop(state);
        self.notify.notify_one();
    }

    fn take_blocks(state: &mut RollingProducerIngressState) -> Vec<RollingProducerIngressBlock> {
        let mut pending = Vec::with_capacity(2 + state.flow.len());
        if let Some(exit) = state.exit.take() {
            pending.push(RollingProducerIngressBlock::Barrier(exit));
        }
        pending.extend(
            state
                .flow
                .drain(..)
                .map(RollingProducerIngressBlock::Barrier),
        );
        if let Some(progress) = state.progress.take() {
            pending.push(RollingProducerIngressBlock::Progress(progress));
        }
        pending.sort_by_key(RollingProducerIngressBlock::order_key);
        pending
    }

    fn drain_blocks(&self) -> Vec<RollingProducerIngressBlock> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let freed_flow_capacity = !state.flow.is_empty();
        let blocks = Self::take_blocks(&mut state);
        drop(state);
        if freed_flow_capacity {
            self.flow_capacity_available.notify_waiters();
        }
        blocks
    }

    /// Seal all producer evidence that linearized before one command, then
    /// allocate the command's ingress coordinate while the caller holds the
    /// shared producer-transition fence. Later producer publications must
    /// therefore begin after this envelope and cannot be drained ahead of it.
    fn seal_command(&self, command: RollingControlCommand) -> RollingControlEnvelope {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sealed_flow_barriers = state.flow.len();
        state.sealed_flow_barriers = state
            .sealed_flow_barriers
            .saturating_add(sealed_flow_barriers);
        let preceding_producer = Self::take_blocks(&mut state);
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;
        // Stamp only after both ingress fences are held. A publisher cannot
        // carry an earlier time across a scheduling gap into a later order.
        let published_at = rolling_now();
        RollingControlEnvelope {
            sequence,
            published_at,
            preceding_producer,
            sealed_flow_barriers,
            command,
        }
    }

    fn release_sealed_flow_barriers(&self, count: usize) {
        if count == 0 {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.sealed_flow_barriers >= count);
        state.sealed_flow_barriers = state.sealed_flow_barriers.saturating_sub(count);
        drop(state);
        self.flow_capacity_available.notify_waiters();
    }

    #[cfg(test)]
    fn drain(&self) -> Vec<RollingProducerEvent> {
        self.drain_blocks()
            .into_iter()
            .flat_map(RollingProducerIngressBlock::into_events)
            .collect()
    }

    async fn wait_for_activity(&self) {
        // Producer publication and exact physical-flow acknowledgement both
        // use this retained permit. The actor drains only after it owns the
        // transition and ingress fences together.
        self.notify.notified().await;
    }

    #[cfg(test)]
    async fn next(&self) -> Vec<RollingProducerEvent> {
        self.wait_for_activity().await;
        self.drain_blocks()
            .into_iter()
            .flat_map(RollingProducerIngressBlock::into_events)
            .collect()
    }
}

#[derive(Clone)]
pub(crate) struct RollingControlHandle {
    sender: tokio::sync::mpsc::Sender<RollingControlEnvelope>,
    retired: Arc<AtomicBool>,
    producer_attempt: Arc<AtomicU64>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_abort: Option<tokio::task::AbortHandle>,
    #[cfg(test)]
    actor_exit_fence_started: Arc<tokio::sync::Notify>,
}

struct OwnedLocalControlRequest {
    generation: String,
    owner_epoch: u64,
    client_instance_id: String,
    sequence: u64,
    snapshot: PlaybackDemandSnapshot,
}

enum RollingControlCommand {
    #[cfg(test)]
    Renew {
        kind: &'static str,
        source: RollingRenewalSource,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Control {
        request: Box<OwnedLocalControlRequest>,
        deadline_unix_ms: i64,
        terminal_admission: Option<Arc<dyn RollingTerminalAdmission>>,
        reply: tokio::sync::oneshot::Sender<Result<RollingControlOutcome, ControlStateError>>,
    },
    BeginProducerAttempt {
        reply: tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
    },
    AuthorizeProducerInstall {
        producer_attempt: u64,
        reply: tokio::sync::oneshot::Sender<Result<(), ProducerAttemptRejection>>,
    },
    ObservePublication {
        observation: RollingPublicationObservation,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    CommitMedia {
        kind: &'static str,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Snapshot {
        reply: tokio::sync::oneshot::Sender<RollingLeaseSnapshot>,
    },
    ClaimExpiry {
        reply: tokio::sync::oneshot::Sender<RollingExpiryClaim>,
    },
    Terminal {
        cause: RollingTerminalCause,
        reply: tokio::sync::oneshot::Sender<RollingTerminalOutcome>,
    },
    #[cfg(test)]
    SetRenewalForTest {
        at: Instant,
        kind: &'static str,
        reply: tokio::sync::oneshot::Sender<()>,
    },
}

impl RollingControlCommand {
    fn metric_index(&self) -> Option<usize> {
        match self {
            #[cfg(test)]
            Self::Renew { .. } | Self::SetRenewalForTest { .. } => None,
            Self::Control { .. } => Some(0),
            Self::BeginProducerAttempt { .. } => Some(1),
            Self::AuthorizeProducerInstall { .. } => Some(2),
            Self::ObservePublication { .. } => Some(3),
            Self::CommitMedia { .. } => Some(4),
            Self::Snapshot { .. } => Some(5),
            Self::ClaimExpiry { .. } => Some(6),
            Self::Terminal { .. } => Some(7),
        }
    }
}

/// One bounded actor command linearized with producer progress, exit, and
/// physical-flow facts. The client control sequence remains an independent
/// replay/idempotency coordinate; this sequence orders actor ingress only.
struct RollingControlEnvelope {
    sequence: u64,
    published_at: Instant,
    preceding_producer: Vec<RollingProducerIngressBlock>,
    sealed_flow_barriers: usize,
    command: RollingControlCommand,
}

struct RollingActorRuntime {
    retired_fence: Arc<AtomicBool>,
    producer_attempt: Arc<AtomicU64>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_exit_fence_started: Arc<tokio::sync::Notify>,
}

/// Publishes actor unavailability under the same transition fence used by
/// every process signal. This local is created inside `run`, so Rust drops it
/// before the receiver and actor parameters on return, cancellation, or
/// unwind; `Sender::closed()` therefore cannot become externally visible
/// before the fail-closed retirement fence has linearized.
struct RollingActorExitFence {
    retired: Arc<AtomicBool>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    #[cfg(test)]
    started: Arc<tokio::sync::Notify>,
}

impl Drop for RollingActorExitFence {
    fn drop(&mut self) {
        #[cfg(test)]
        self.started.notify_one();
        let _transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.retired.store(true, Ordering::Release);
        self.producer_events.notify_flow_capacity_waiters();
        self.flow_sync.request();
    }
}

struct RollingControlActor {
    control: ControlState,
    last_renewal: Instant,
    last_renewal_kind: &'static str,
    mode: RollingLeaseMode,
    demand: Option<PlaybackDemandSnapshot>,
    delivery: RollingDeliverySnapshot,
    producer_progress_at: Option<Instant>,
    producer_exit_at: Option<Instant>,
    producer_progress_deadline: Option<ProducerProgressDeadline>,
    producer_deadline_due: Option<ProducerDeadlineDue>,
    producer_process_exit_due: Option<ProducerProcessExitDue>,
    producer_flow_revision: u64,
    producer_physical_flow: ProducerPhysicalFlowState,
    last_applied_ingress_sequence: u64,
    retired: bool,
    terminal: Option<RollingTerminalCause>,
    expiration_claimed: bool,
    retired_fence: Arc<AtomicBool>,
    producer_attempt: Arc<AtomicU64>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    last_flow_ticket: u64,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_exit_fence_started: Arc<tokio::sync::Notify>,
}

impl RollingControlActor {
    #[cfg(test)]
    fn new(now: Instant, initial_kind: &'static str, retired_fence: Arc<AtomicBool>) -> Self {
        Self::with_runtime(
            now,
            initial_kind,
            RollingActorRuntime {
                retired_fence,
                producer_attempt: Arc::new(AtomicU64::new(0)),
                producer_transition: Arc::new(std::sync::Mutex::new(
                    RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
                )),
                flow_sync: Arc::new(RollingFlowSync::new()),
                producer_events: Arc::new(RollingProducerIngress::new()),
                producer_attempt_reply_pause: Arc::new(std::sync::Mutex::new(None)),
                actor_exit_fence_started: Arc::new(tokio::sync::Notify::new()),
            },
        )
    }

    fn with_runtime(
        now: Instant,
        initial_kind: &'static str,
        runtime: RollingActorRuntime,
    ) -> Self {
        let RollingActorRuntime {
            retired_fence,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_exit_fence_started,
        } = runtime;
        Self {
            control: ControlState::default(),
            last_renewal: now,
            last_renewal_kind: initial_kind,
            mode: RollingLeaseMode::Legacy,
            demand: None,
            delivery: RollingDeliverySnapshot::default(),
            producer_progress_at: None,
            producer_exit_at: None,
            producer_progress_deadline: None,
            producer_deadline_due: None,
            producer_process_exit_due: None,
            producer_flow_revision: 0,
            producer_physical_flow: ProducerPhysicalFlowState::Running,
            last_applied_ingress_sequence: 0,
            retired: false,
            terminal: None,
            expiration_claimed: false,
            retired_fence,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            last_flow_ticket: 0,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_exit_fence_started,
        }
    }

    fn snapshot_at(&self, now: Instant) -> RollingLeaseSnapshot {
        let idle_for = now.saturating_duration_since(self.last_renewal);
        let timeout = self.mode.timeout();
        let deadline = self.deadline();
        let mut delivery = self.delivery.clone();
        delivery.producer_progress_idle_ms = self.producer_progress_at.map_or(0, |observed_at| {
            i64::try_from(now.saturating_duration_since(observed_at).as_millis())
                .unwrap_or(i64::MAX)
        });
        if let (Some(exit), Some(observed_at)) =
            (delivery.producer_exit.as_mut(), self.producer_exit_at)
        {
            exit.observed_idle_ms =
                i64::try_from(now.saturating_duration_since(observed_at).as_millis())
                    .unwrap_or(i64::MAX);
        }
        RollingLeaseSnapshot {
            mode: self.mode,
            idle_for,
            remaining: timeout.saturating_sub(idle_for),
            deadline,
            last_renewal_kind: self.last_renewal_kind,
            demand: self.demand.clone(),
            delivery,
            retired: self.retired,
            terminal: self.terminal,
            expiration_claimed: self.expiration_claimed,
            producer_control: self.producer_operational_snapshot_at(now),
        }
    }

    fn producer_operational_snapshot_at(&self, now: Instant) -> RollingProducerOperationalSnapshot {
        let deadline_mode = self
            .producer_progress_deadline
            .map(|deadline| match deadline.mode {
                ProducerProgressDeadlineMode::Starting => "starting",
                ProducerProgressDeadlineMode::Advancing => "advancing",
                ProducerProgressDeadlineMode::ClassifyingExit => "classifying_exit",
            });
        let deadline_remaining_ms = self.producer_progress_deadline.map(|deadline| {
            i64::try_from(deadline.instant.saturating_duration_since(now).as_millis())
                .unwrap_or(i64::MAX)
        });
        let due_mode = self.producer_deadline_due.map(|due| match due.mode {
            ProducerProgressDeadlineMode::Starting => "starting",
            ProducerProgressDeadlineMode::Advancing => "advancing",
            ProducerProgressDeadlineMode::ClassifyingExit => "classifying_exit",
        });
        let due_overdue_ms = self.producer_deadline_due.map(|due| {
            i64::try_from(now.saturating_duration_since(due.deadline).as_millis())
                .unwrap_or(i64::MAX)
        });
        let phase = if self.retired {
            "terminal"
        } else if self.producer_deadline_due.is_some() || self.producer_process_exit_due.is_some() {
            "due"
        } else if self.producer_physical_flow == ProducerPhysicalFlowState::Held {
            "held"
        } else {
            match self
                .producer_progress_deadline
                .map(|deadline| deadline.mode)
            {
                Some(ProducerProgressDeadlineMode::Starting) => "starting",
                Some(ProducerProgressDeadlineMode::Advancing) => "running",
                Some(ProducerProgressDeadlineMode::ClassifyingExit) => "classifying_exit",
                None => "unarmed",
            }
        };
        RollingProducerOperationalSnapshot {
            phase,
            deadline_attempt: self
                .producer_progress_deadline
                .map(|deadline| deadline.producer_attempt),
            deadline_mode,
            deadline_remaining_ms,
            due_attempt: self.producer_deadline_due.map(|due| due.producer_attempt),
            due_mode,
            due_overdue_ms,
            process_exit_attempt: self
                .producer_process_exit_due
                .map(|due| due.producer_attempt),
            process_exit_due: self.producer_process_exit_due.is_some(),
            physical_flow: match self.producer_physical_flow {
                ProducerPhysicalFlowState::Running => "running",
                ProducerPhysicalFlowState::Held => "held",
            },
            last_flow_applied_sequence: self.producer_flow_revision,
            last_applied_sequence: self.last_applied_ingress_sequence,
            observation_only: true,
            action_owner: "legacy_compatibility",
        }
    }

    fn deadline(&self) -> Instant {
        self.last_renewal
            .checked_add(self.mode.timeout())
            .unwrap_or(self.last_renewal)
    }

    fn next_deadline(&self) -> Instant {
        self.producer_progress_deadline.map_or_else(
            || self.deadline(),
            |producer| self.deadline().min(producer.instant),
        )
    }

    fn arm_producer_deadline(
        &mut self,
        producer_attempt: u64,
        mode: ProducerProgressDeadlineMode,
        instant: Instant,
    ) {
        if self.retired
            || producer_attempt != self.delivery.producer_attempt
            || (self.producer_physical_flow == ProducerPhysicalFlowState::Held
                && !matches!(mode, ProducerProgressDeadlineMode::ClassifyingExit))
            || self
                .producer_process_exit_due
                .is_some_and(|due| due.producer_attempt == producer_attempt)
            || self
                .producer_deadline_due
                .is_some_and(|due| due.producer_attempt == producer_attempt)
        {
            return;
        }
        self.producer_progress_deadline = Some(ProducerProgressDeadline {
            producer_attempt,
            mode,
            instant,
        });
    }

    /// Record the actor's exact provisional producer timeout without taking
    /// recovery authority from the legacy path. Returning the retained due
    /// coordinate makes scheduler-delay tests assert the armed instant rather
    /// than dispatch time and prevents an ordinary late event from rearming.
    fn settle_producer_deadline_at(&mut self, now: Instant) -> Option<ProducerDeadlineDue> {
        if self.producer_physical_flow == ProducerPhysicalFlowState::Held
            && self.producer_progress_deadline.is_some_and(|deadline| {
                !matches!(deadline.mode, ProducerProgressDeadlineMode::ClassifyingExit)
            })
        {
            self.producer_progress_deadline = None;
            return None;
        }
        let deadline = self
            .producer_progress_deadline
            .filter(|deadline| now >= deadline.instant)?;
        self.producer_progress_deadline = None;
        let due = ProducerDeadlineDue {
            producer_attempt: deadline.producer_attempt,
            mode: deadline.mode,
            deadline: deadline.instant,
        };
        self.producer_deadline_due = Some(due);
        let (mode_index, _) = deadline.mode.metric();
        ROLLING_PRODUCER_DEADLINE_OBSERVATIONS[mode_index].fetch_add(1, Ordering::Relaxed);
        Some(due)
    }

    /// Session lifecycle is always the higher-priority clock. Only a live
    /// session may record an action-passive producer deadline observation.
    #[cfg(test)]
    fn settle_due_deadlines_at(&mut self, now: Instant) -> Option<ProducerDeadlineDue> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return None;
        }
        self.settle_producer_deadline_at(now)
    }

    /// Apply one exact sequenced physical-flow acknowledgement. A timely Hold
    /// discards the running budget; a late Hold preserves the already-won due
    /// coordinate. Resume grants a fresh full budget only after an eligible
    /// Held state was actually applied.
    fn apply_producer_flow_applied_at(&mut self, now: Instant, applied: ProducerFlowApplied) {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return;
        }
        if !(applied.revision > self.producer_flow_revision
            && applied.producer_attempt == self.delivery.producer_attempt)
        {
            return;
        }
        self.producer_flow_revision = applied.revision;
        let previous = self.producer_physical_flow;
        match applied.state {
            ProducerPhysicalFlowState::Held => {
                if self.producer_progress_deadline.is_some_and(|deadline| {
                    deadline.producer_attempt == applied.producer_attempt
                        && !matches!(deadline.mode, ProducerProgressDeadlineMode::ClassifyingExit)
                        && applied.published_at > deadline.instant
                }) {
                    let _ = self.settle_producer_deadline_at(applied.published_at);
                }
                self.producer_physical_flow = ProducerPhysicalFlowState::Held;
                if self.producer_deadline_due.is_none()
                    && self.producer_progress_deadline.is_some_and(|deadline| {
                        deadline.producer_attempt == applied.producer_attempt
                            && !matches!(
                                deadline.mode,
                                ProducerProgressDeadlineMode::ClassifyingExit
                            )
                            && applied.published_at <= deadline.instant
                    })
                {
                    self.producer_progress_deadline = None;
                }
            }
            ProducerPhysicalFlowState::Running => {
                self.producer_physical_flow = ProducerPhysicalFlowState::Running;
                if previous == ProducerPhysicalFlowState::Held
                    && !self.retired
                    && self.delivery.producer_exit.is_none()
                    && self.producer_deadline_due.is_none()
                    && self.producer_process_exit_due.is_none()
                {
                    self.arm_producer_deadline(
                        applied.producer_attempt,
                        ProducerProgressDeadlineMode::Advancing,
                        applied
                            .published_at
                            .checked_add(PRODUCER_PROGRESS_BUDGET)
                            .unwrap_or(applied.published_at),
                    );
                }
            }
        }
    }

    fn renew_at(&mut self, now: Instant, kind: &'static str, source: RollingRenewalSource) -> bool {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return false;
        }
        self.last_renewal = now;
        self.last_renewal_kind = kind;
        let source_index = match source {
            RollingRenewalSource::Media => 1,
            RollingRenewalSource::Internal => 2,
        };
        ROLLING_LEASE_RENEWALS[source_index].fetch_add(1, Ordering::Relaxed);
        true
    }

    fn control_at(
        &mut self,
        now: Instant,
        request: OwnedLocalControlRequest,
    ) -> Result<RollingControlOutcome, ControlStateError> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            let replay = (self.terminal == Some(RollingTerminalCause::End)
                && self.demand.as_ref().is_some_and(|snapshot| {
                    snapshot.demand == PlaybackDemand::End && snapshot == &request.snapshot
                }))
            .then(|| {
                self.control.replay_exact(
                    &request.generation,
                    request.owner_epoch,
                    &request.client_instance_id,
                    request.sequence,
                    request.snapshot.platform(),
                )
            })
            .flatten();
            let Some((disposition, accepted_sequence, action, platform)) = replay else {
                return Err(ControlStateError::SessionEnded);
            };
            return Ok(RollingControlOutcome {
                disposition,
                accepted_sequence,
                action,
                platform,
                lease: self.snapshot_at(now),
                flow_ticket: self.last_flow_ticket,
            });
        }
        let (disposition, accepted_sequence, action, platform) = self.control.accept_at(
            now,
            &request.generation,
            request.owner_epoch,
            &request.client_instance_id,
            request.sequence,
            request.snapshot.platform(),
        )?;
        let accepted_end = disposition == ControlDisposition::Accepted
            && request.snapshot.demand == PlaybackDemand::End;
        if disposition == ControlDisposition::Accepted {
            self.mode = RollingLeaseMode::Explicit;
            self.demand = Some(request.snapshot);
            if accepted_end {
                self.last_renewal_kind = "control-end";
            } else {
                self.last_renewal = now;
                self.last_renewal_kind = "control";
                ROLLING_LEASE_RENEWALS[0].fetch_add(1, Ordering::Relaxed);
            }
        }
        // Accepted mutation and its producer-policy wake are one actor
        // transaction. A replay returns the SAME ticket: the detached worker
        // survives a lost response, and minting new work for an unthrottled
        // equal sequence would turn replay into a flow-control DoS surface.
        let flow_ticket = if accepted_end {
            let RollingTerminalOutcome::Won(RollingTerminalCause::End) =
                self.terminate(RollingTerminalCause::End)
            else {
                unreachable!("fresh accepted end must win while the actor is live");
            };
            self.last_flow_ticket
        } else if disposition == ControlDisposition::Accepted {
            let ticket = self.flow_sync.request();
            self.last_flow_ticket = ticket;
            ticket
        } else {
            debug_assert!(self.last_flow_ticket > 0);
            self.last_flow_ticket
        };
        Ok(RollingControlOutcome {
            disposition,
            accepted_sequence,
            action,
            platform,
            lease: self.snapshot_at(now),
            flow_ticket,
        })
    }

    fn begin_producer_attempt_at(&mut self, now: Instant) -> Result<u64, ProducerAttemptRejection> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if self.delivery.playlist_ready {
            return Err(ProducerAttemptRejection::PlaylistPublished);
        }
        let attempt = self
            .delivery
            .producer_attempt
            .checked_add(1)
            .ok_or(ProducerAttemptRejection::AttemptExhausted)?;
        self.delivery = RollingDeliverySnapshot {
            producer_attempt: attempt,
            ..RollingDeliverySnapshot::default()
        };
        self.producer_progress_at = Some(now);
        self.producer_exit_at = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.producer_physical_flow = ProducerPhysicalFlowState::Running;
        self.arm_producer_deadline(
            attempt,
            ProducerProgressDeadlineMode::Starting,
            now.checked_add(PRODUCER_STARTUP_BUDGET).unwrap_or(now),
        );
        self.producer_attempt.store(attempt, Ordering::Release);
        Ok(attempt)
    }

    fn update_producer_progress_at(
        &mut self,
        now: Instant,
        observation: RollingProducerProgressObservation,
    ) -> (bool, bool) {
        if self.retired
            || observation.producer_attempt != self.delivery.producer_attempt
            || self.delivery.producer_exit.is_some()
        {
            return (false, false);
        }
        let mut advanced = false;
        if let Some(out_time_ms) = observation.out_time_ms.filter(|value| *value >= 0) {
            if self
                .delivery
                .producer_out_time_ms
                .is_none_or(|current| out_time_ms > current)
            {
                self.delivery.producer_out_time_ms = Some(out_time_ms);
                self.producer_progress_at = Some(observation.observed_at.min(now));
                advanced = true;
            }
        }
        if let Some(speed_milli) = observation.speed_milli.filter(|value| *value >= 0) {
            self.delivery.producer_speed_milli = Some(speed_milli);
        }
        if let Some(recent_speed_milli) = observation.recent_speed_milli.filter(|value| *value >= 0)
        {
            self.delivery.producer_recent_speed_milli = Some(recent_speed_milli);
        }
        (
            advanced
                || observation.speed_milli.is_some()
                || observation.recent_speed_milli.is_some(),
            advanced,
        )
    }

    fn observe_producer_progress_at(
        &mut self,
        now: Instant,
        observation: RollingProducerProgressObservation,
    ) -> bool {
        let producer_attempt = observation.producer_attempt;
        let deadline_accepts_progress = self.producer_progress_deadline.is_some_and(|deadline| {
            deadline.producer_attempt == producer_attempt && now <= deadline.instant
        });
        if !deadline_accepts_progress {
            let _ = self.settle_producer_deadline_at(now);
        }
        let (accepted, advanced) = self.update_producer_progress_at(now, observation);
        if advanced && deadline_accepts_progress {
            self.arm_producer_deadline(
                producer_attempt,
                ProducerProgressDeadlineMode::Advancing,
                now.checked_add(PRODUCER_PROGRESS_BUDGET).unwrap_or(now),
            );
        }
        accepted
    }

    fn observe_producer_exit_at(
        &mut self,
        published_at: Instant,
        observation: RollingProducerExitObservation,
    ) -> ProducerExitAcceptance {
        if self.retired || observation.producer_attempt != self.delivery.producer_attempt {
            return ProducerExitAcceptance::Rejected;
        }
        if self.producer_progress_deadline.is_some_and(|deadline| {
            deadline.producer_attempt == observation.producer_attempt
                && published_at > deadline.instant
        }) {
            let _ = self.settle_producer_deadline_at(published_at);
        }
        if let Some(exit) = &self.delivery.producer_exit {
            return if exit.success == observation.success
                && exit.code == observation.code
                && exit.signal == observation.signal
            {
                ProducerExitAcceptance::Duplicate
            } else {
                ProducerExitAcceptance::Rejected
            };
        }
        self.delivery.producer_exit = Some(RollingProducerExitSnapshot {
            success: observation.success,
            code: observation.code,
            signal: observation.signal,
            observed_idle_ms: 0,
        });
        self.producer_exit_at = Some(observation.observed_at.min(published_at));
        self.producer_progress_deadline = None;
        if !observation.success
            && self
                .producer_deadline_due
                .is_none_or(|due| due.producer_attempt != observation.producer_attempt)
        {
            self.producer_process_exit_due = Some(ProducerProcessExitDue {
                producer_attempt: observation.producer_attempt,
                published_at,
                code: observation.code,
                signal: observation.signal,
            });
        } else if observation.success
            && self
                .producer_deadline_due
                .is_none_or(|due| due.producer_attempt != observation.producer_attempt)
        {
            self.arm_producer_deadline(
                observation.producer_attempt,
                ProducerProgressDeadlineMode::ClassifyingExit,
                published_at
                    .checked_add(PRODUCER_EXIT_CLASSIFICATION_BUDGET)
                    .unwrap_or(published_at),
            );
        }
        ProducerExitAcceptance::Accepted
    }

    /// Apply one constant-space ingress proof against the deadline armed
    /// immediately before its first advancing publication. Timely contiguous
    /// coverage may extend the clock through the covered tail; a late first
    /// sample, first gap, or speed-only tail can only leave telemetry behind.
    fn observe_producer_progress_batch_at(
        &mut self,
        now: Instant,
        batch: ProgressCoverageBatch,
    ) -> bool {
        let producer_attempt = batch.producer_attempt;
        let first_advancing_at = batch
            .first_advancing
            .as_ref()
            .map(|sample| sample.published_at);
        let covered_deadline = batch.covered_deadline;
        let first_gap_at = batch.first_gap.as_ref().map(|sample| sample.published_at);
        let latest_published_at = batch.latest_telemetry.published_at;
        let initial_deadline = self.producer_progress_deadline.filter(|deadline| {
            deadline.producer_attempt == producer_attempt
                && !matches!(deadline.mode, ProducerProgressDeadlineMode::ClassifyingExit)
        });
        let first_is_timely = first_advancing_at
            .zip(initial_deadline)
            .is_some_and(|(published_at, deadline)| published_at <= deadline.instant);

        if let (Some(published_at), Some(deadline)) = (first_advancing_at, initial_deadline) {
            if published_at > deadline.instant {
                let _ = self.settle_producer_deadline_at(published_at);
            }
        } else if first_advancing_at.is_none()
            && initial_deadline.is_some_and(|deadline| latest_published_at > deadline.instant)
        {
            let _ = self.settle_producer_deadline_at(latest_published_at);
        }

        let observation = batch
            .into_observations()
            .into_iter()
            .next()
            .expect("a progress batch always retains telemetry");
        let (accepted, advanced) = self.update_producer_progress_at(now, observation);
        if accepted && advanced && first_is_timely {
            if let Some(deadline) = covered_deadline {
                self.arm_producer_deadline(
                    producer_attempt,
                    ProducerProgressDeadlineMode::Advancing,
                    deadline,
                );
            }
        }

        let post_coverage_at = first_gap_at.unwrap_or(latest_published_at);
        if self.producer_progress_deadline.is_some_and(|deadline| {
            deadline.producer_attempt == producer_attempt && post_coverage_at > deadline.instant
        }) {
            let _ = self.settle_producer_deadline_at(post_coverage_at);
        }
        accepted
    }

    fn handle_sequenced_producer_event_at(
        &mut self,
        now: Instant,
        sequenced: SequencedProducerEvent,
    ) {
        if self.producer_progress_deadline.is_some_and(|deadline| {
            deadline.producer_attempt == sequenced.event.producer_attempt()
                && sequenced.published_at > deadline.instant
        }) {
            let _ = self.settle_producer_deadline_at(sequenced.published_at);
        }
        match sequenced.event {
            RollingProducerEvent::Progress(observation) => {
                let accepted =
                    self.observe_producer_progress_at(sequenced.published_at, observation);
                ROLLING_PRODUCER_EVENT_OUTCOMES[usize::from(!accepted)]
                    .fetch_add(1, Ordering::Relaxed);
            }
            RollingProducerEvent::Exit(observation) => {
                let accepted = self.observe_producer_exit_at(sequenced.published_at, observation);
                ROLLING_PRODUCER_EVENT_OUTCOMES
                    [2 + usize::from(!matches!(accepted, ProducerExitAcceptance::Accepted))]
                .fetch_add(1, Ordering::Relaxed);
            }
            RollingProducerEvent::FlowApplied(observation) => {
                self.apply_producer_flow_applied_at(
                    now,
                    ProducerFlowApplied {
                        revision: sequenced.sequence,
                        producer_attempt: observation.producer_attempt,
                        state: observation.state,
                        published_at: sequenced.published_at,
                    },
                );
            }
        }
    }

    fn handle_producer_block_at(&mut self, now: Instant, block: RollingProducerIngressBlock) {
        match block {
            RollingProducerIngressBlock::Progress(batch) => {
                self.last_applied_ingress_sequence = self
                    .last_applied_ingress_sequence
                    .max(batch.last_sequence());
                let accepted = self.observe_producer_progress_batch_at(now, batch);
                ROLLING_PRODUCER_EVENT_OUTCOMES[usize::from(!accepted)]
                    .fetch_add(1, Ordering::Relaxed);
            }
            RollingProducerIngressBlock::Barrier(barrier) => {
                if let Some(batch) = barrier.preceding_progress {
                    self.last_applied_ingress_sequence = self
                        .last_applied_ingress_sequence
                        .max(batch.last_sequence());
                    let accepted = self.observe_producer_progress_batch_at(now, batch);
                    ROLLING_PRODUCER_EVENT_OUTCOMES[usize::from(!accepted)]
                        .fetch_add(1, Ordering::Relaxed);
                }
                self.last_applied_ingress_sequence = self
                    .last_applied_ingress_sequence
                    .max(barrier.event.sequence);
                self.handle_sequenced_producer_event_at(now, barrier.event);
            }
        }
    }

    fn handle_producer_blocks_at(
        &mut self,
        now: Instant,
        blocks: Vec<RollingProducerIngressBlock>,
    ) {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            for block in blocks {
                self.handle_producer_block_at(now, block);
            }
            return;
        }
        for block in blocks {
            self.handle_producer_block_at(now, block);
        }
        let _ = self.settle_producer_deadline_at(now);
    }

    #[cfg(test)]
    fn run_due_first_cutoff_at(&mut self, now: Instant) {
        let transition = Arc::clone(&self.producer_transition);
        let _transition = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let producer_events = Arc::clone(&self.producer_events);
        let mut ingress = producer_events
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Keep both fences through the complete fold and its single final
        // settlement. A producer cannot publish B between capture of A and
        // the due observation; its publication coordinate is allocated only
        // after this ingress guard is released.
        let freed_flow_capacity = !ingress.flow.is_empty();
        let blocks = RollingProducerIngress::take_blocks(&mut ingress);
        self.handle_producer_blocks_at(now, blocks);
        drop(ingress);
        drop(_transition);
        if freed_flow_capacity {
            producer_events.flow_capacity_available.notify_waiters();
        }
    }

    #[cfg(test)]
    fn handle_producer_event(&mut self, event: RollingProducerEvent) {
        let now = rolling_now();
        let (metric_index, accepted) = match event {
            RollingProducerEvent::Progress(observation) => {
                (0, self.observe_producer_progress_at(now, observation))
            }
            RollingProducerEvent::Exit(observation) => (
                2,
                matches!(
                    self.observe_producer_exit_at(now, observation),
                    ProducerExitAcceptance::Accepted
                ),
            ),
            RollingProducerEvent::FlowApplied(observation) => {
                self.apply_producer_flow_applied_at(
                    now,
                    ProducerFlowApplied {
                        revision: self.producer_flow_revision.saturating_add(1),
                        producer_attempt: observation.producer_attempt,
                        state: observation.state,
                        published_at: now,
                    },
                );
                return;
            }
        };
        ROLLING_PRODUCER_EVENT_OUTCOMES[metric_index + usize::from(!accepted)]
            .fetch_add(1, Ordering::Relaxed);
    }

    fn authorize_producer_install_at(
        &mut self,
        now: Instant,
        producer_attempt: u64,
    ) -> Result<(), ProducerAttemptRejection> {
        if producer_attempt != self.delivery.producer_attempt {
            return Err(ProducerAttemptRejection::StaleAttempt);
        }
        self.renew_at(now, "producer-install", RollingRenewalSource::Internal)
            .then_some(())
            .ok_or(ProducerAttemptRejection::SessionEnded)
    }

    fn observe_publication_at(
        &mut self,
        now: Instant,
        observation: RollingPublicationObservation,
    ) -> bool {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live)
            || observation.producer_attempt != self.delivery.producer_attempt
        {
            return false;
        }
        self.delivery.playlist_ready |= observation.playlist_ready;
        if observation.published_segment >= self.delivery.published_segment {
            self.delivery.published_segment = observation.published_segment;
        }
        if observation.published_end_ms >= self.delivery.published_end_ms {
            self.delivery.published_end_ms = observation.published_end_ms;
        }
        self.delivery.next_media_sequence = self
            .delivery
            .next_media_sequence
            .max(observation.next_media_sequence);
        if let (Some(pending), Some(resolved), Some(end_ms)) = (
            self.delivery.pending_fetched_segment,
            observation.resolved_fetched_segment,
            observation.resolved_fetched_end_ms,
        ) {
            if pending == resolved {
                self.delivery.fetched_segment = Some(
                    self.delivery
                        .fetched_segment
                        .map_or(pending, |current| current.max(pending)),
                );
                self.delivery.fetched_end_ms = self.delivery.fetched_end_ms.max(end_ms);
                self.delivery.pending_fetched_segment = None;
            }
        }
        true
    }

    fn commit_media_at(
        &mut self,
        now: Instant,
        kind: &'static str,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
    ) -> bool {
        if producer_attempt != self.delivery.producer_attempt
            || !self.renew_at(now, kind, RollingRenewalSource::Media)
        {
            return false;
        }
        let Some(segment_index) = segment_index else {
            return true;
        };
        if let Some(current) = self.delivery.fetched_segment {
            if segment_index < current {
                return true;
            }
            if segment_index == current {
                if self.delivery.pending_fetched_segment == Some(segment_index) {
                    if let Some(end_ms) = segment_end_ms {
                        self.delivery.fetched_end_ms = self.delivery.fetched_end_ms.max(end_ms);
                        self.delivery.pending_fetched_segment = None;
                    }
                }
                return true;
            }
        }
        self.delivery.fetched_segment = Some(segment_index);
        if let Some(end_ms) = segment_end_ms {
            self.delivery.fetched_end_ms = self.delivery.fetched_end_ms.max(end_ms);
            self.delivery.pending_fetched_segment = None;
        } else {
            self.delivery.pending_fetched_segment = Some(segment_index);
        }
        true
    }

    /// Commit the actor's one terminal transition and retain its first cause.
    /// The shared atomic is only a compatibility projection for synchronous
    /// serving paths; this actor state is the lifecycle source of truth.
    fn terminate(&mut self, cause: RollingTerminalCause) -> RollingTerminalOutcome {
        let metric_base = cause.metric_index() * 2;
        if self.retired {
            ROLLING_TERMINAL_EVENT_OUTCOMES[metric_base + 1].fetch_add(1, Ordering::Relaxed);
            return RollingTerminalOutcome::AlreadyTerminal(
                self.terminal
                    .expect("retired actor must retain its terminal cause"),
            );
        }
        self.retired = true;
        self.terminal = Some(cause);
        self.expiration_claimed = cause == RollingTerminalCause::LeaseExpired;
        self.producer_progress_deadline = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.retired_fence.store(true, Ordering::Release);
        self.producer_events.notify_flow_capacity_waiters();
        self.last_flow_ticket = self.flow_sync.request();
        ROLLING_TERMINAL_EVENT_OUTCOMES[metric_base].fetch_add(1, Ordering::Relaxed);
        match cause {
            RollingTerminalCause::LeaseExpired => {
                ROLLING_LEASE_EXPIRATIONS.fetch_add(1, Ordering::Relaxed);
            }
            RollingTerminalCause::End | RollingTerminalCause::AuthorityFence => {
                ROLLING_LEASE_RETIREMENTS.fetch_add(1, Ordering::Relaxed);
            }
        }
        RollingTerminalOutcome::Won(cause)
    }

    fn claim_expiry_at(&mut self, now: Instant) -> RollingExpiryClaim {
        let snapshot = self.snapshot_at(now);
        if self.retired {
            RollingExpiryClaim::Retired(snapshot)
        } else if snapshot.expired() {
            let RollingTerminalOutcome::Won(RollingTerminalCause::LeaseExpired) =
                self.terminate(RollingTerminalCause::LeaseExpired)
            else {
                unreachable!("a live expired actor must win its terminal transition");
            };
            // Report the committed transition, not the pre-claim observation.
            // The claimant may be the reaper, a snapshot reader, or the exact
            // timer; all of them must see the same terminal facts.
            RollingExpiryClaim::Claimed(self.snapshot_at(now))
        } else {
            RollingExpiryClaim::Live
        }
    }

    async fn handle_command(&mut self, envelope: RollingControlEnvelope) {
        let RollingControlEnvelope {
            sequence,
            published_at,
            preceding_producer,
            sealed_flow_barriers,
            command,
        } = envelope;
        // Keep the non-Send transition guard inside a lexical scope rather
        // than relying on an explicit `drop` for async Send analysis. Only the
        // test reply tuple may cross the later await.
        let _deferred_begin_reply = {
            let transition = Arc::clone(&self.producer_transition);
            let mut transition = transition
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.handle_producer_blocks_at(published_at, preceding_producer);
            self.last_applied_ingress_sequence = self.last_applied_ingress_sequence.max(sequence);
            if let Some(metric_index) = command.metric_index() {
                ROLLING_CONTROL_COMMANDS[metric_index].fetch_add(1, Ordering::Relaxed);
            }
            #[cfg(test)]
            let mut deferred_begin_reply: Option<(
                tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
                Result<u64, ProducerAttemptRejection>,
                Option<Arc<tokio::sync::Barrier>>,
            )> = None;
            match command {
                #[cfg(test)]
                RollingControlCommand::Renew {
                    kind,
                    source,
                    reply,
                } => {
                    let renewed = self.renew_at(published_at, kind, source);
                    if renewed {
                        transition.lease_deadline = self.deadline();
                    }
                    let _ = reply.send(renewed);
                }
                RollingControlCommand::Control {
                    request,
                    deadline_unix_ms,
                    terminal_admission,
                    reply,
                } => {
                    // A queued nonterminal command is not permission to mutate
                    // after its caller has gone away or its inherited exchange
                    // deadline has expired.  The final closed check and mutation
                    // are consecutive actor operations with no suspension point.
                    if reply.is_closed() {
                        // Cancellation before actor application consumes the
                        // ordered barrier but must not mutate control state.
                    } else {
                        let outcome = if crate::media_sessions::unix_ms() >= deadline_unix_ms {
                            Err(ControlStateError::Unavailable)
                        } else {
                            self.control_at(published_at, *request)
                        };
                        if outcome.as_ref().is_ok_and(|outcome| {
                            outcome.disposition == ControlDisposition::Accepted
                                && !outcome.lease.retired
                        }) {
                            transition.lease_deadline = self.deadline();
                        }
                        if let (Some(admission), Ok(outcome)) =
                            (terminal_admission, outcome.as_ref())
                        {
                            if outcome.lease.terminal == Some(RollingTerminalCause::End) {
                                // Transfer ownership before the fallible reply send.
                                admission.accepted(outcome.clone());
                            }
                        }
                        let _ = reply.send(outcome);
                    }
                }
                RollingControlCommand::BeginProducerAttempt { reply } => {
                    // Installation holds this exact fence through synchronous
                    // child/registry publication. A newer attempt must not pass
                    // that linearization point and make the just-published owner
                    // stale before the publication itself completes.
                    let outcome = self.begin_producer_attempt_at(published_at);
                    #[cfg(test)]
                    let reply_pause = self
                        .producer_attempt_reply_pause
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    #[cfg(test)]
                    {
                        deferred_begin_reply = Some((reply, outcome, reply_pause));
                    }
                    #[cfg(not(test))]
                    let _ = reply.send(outcome);
                }
                RollingControlCommand::AuthorizeProducerInstall {
                    producer_attempt,
                    reply,
                } => {
                    let authorized =
                        self.authorize_producer_install_at(published_at, producer_attempt);
                    if authorized.is_ok() {
                        transition.lease_deadline = self.deadline();
                    }
                    let _ = reply.send(authorized);
                }
                RollingControlCommand::ObservePublication { observation, reply } => {
                    let _ = reply.send(self.observe_publication_at(published_at, observation));
                }
                RollingControlCommand::CommitMedia {
                    kind,
                    producer_attempt,
                    segment_index,
                    segment_end_ms,
                    reply,
                } => {
                    let committed = self.commit_media_at(
                        published_at,
                        kind,
                        producer_attempt,
                        segment_index,
                        segment_end_ms,
                    );
                    if committed {
                        transition.lease_deadline = self.deadline();
                    }
                    let _ = reply.send(committed);
                }
                RollingControlCommand::Snapshot { reply } => {
                    // A snapshot is an actor command, not an advisory timestamp
                    // read. If it reaches the mailbox at the exact deadline while
                    // both select branches are ready, it must linearize expiry
                    // before any caller can use the returned state to authorize a
                    // producer signal.
                    let snapshot = match self.claim_expiry_at(published_at) {
                        RollingExpiryClaim::Live => {
                            let _ = self.settle_producer_deadline_at(published_at);
                            self.snapshot_at(published_at)
                        }
                        RollingExpiryClaim::Claimed(snapshot)
                        | RollingExpiryClaim::Retired(snapshot) => snapshot,
                    };
                    let _ = reply.send(snapshot);
                }
                RollingControlCommand::ClaimExpiry { reply } => {
                    let _ = reply.send(self.claim_expiry_at(published_at));
                }
                RollingControlCommand::Terminal { cause, reply } => {
                    // Publication time is the lifecycle linearization point. An
                    // exact-deadline terminal command loses to expiry, while a
                    // command published before the deadline is not relabelled by
                    // actor scheduling delay.
                    let _ = self.claim_expiry_at(published_at);
                    let outcome = self.terminate(cause);
                    let _ = reply.send(outcome);
                }
                #[cfg(test)]
                RollingControlCommand::SetRenewalForTest { at, kind, reply } => {
                    self.last_renewal = at;
                    self.last_renewal_kind = kind;
                    transition.lease_deadline = self.deadline();
                    let _ = reply.send(());
                }
            }
            drop(transition);
            self.producer_events
                .release_sealed_flow_barriers(sealed_flow_barriers);
            #[cfg(test)]
            deferred_begin_reply
        };
        #[cfg(test)]
        if let Some((reply, outcome, reply_pause)) = _deferred_begin_reply {
            if let Some(reply_pause) = reply_pause {
                reply_pause.wait().await;
                reply_pause.wait().await;
            }
            let _ = reply.send(outcome);
        }
    }

    #[cfg(test)]
    async fn handle_command_for_test(&mut self, command: RollingControlCommand) {
        let transition = Arc::clone(&self.producer_transition);
        let transition = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope = self.producer_events.seal_command(command);
        drop(transition);
        self.handle_command(envelope).await;
    }

    /// If a command published before this cutoff is already queued, return it
    /// without draining producer facts that followed it. The shared
    /// transition fence closes the missed-wakeup race: a publisher either
    /// completed its send before `try_recv`, or receives a later timestamp
    /// after this cutoff releases the fence.
    fn command_before_cutoff(
        &mut self,
        receiver: &mut tokio::sync::mpsc::Receiver<RollingControlEnvelope>,
    ) -> Option<RollingControlEnvelope> {
        let transition = Arc::clone(&self.producer_transition);
        let _transition = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(envelope) = receiver.try_recv() {
            return Some(envelope);
        }
        let producer_events = Arc::clone(&self.producer_events);
        let mut ingress = producer_events
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let freed_flow_capacity = !ingress.flow.is_empty();
        let blocks = RollingProducerIngress::take_blocks(&mut ingress);
        let now = rolling_now();
        self.handle_producer_blocks_at(now, blocks);
        drop(ingress);
        drop(_transition);
        if freed_flow_capacity {
            producer_events.flow_capacity_available.notify_waiters();
        }
        None
    }

    async fn run(mut self, mut receiver: tokio::sync::mpsc::Receiver<RollingControlEnvelope>) {
        let _exit_fence = RollingActorExitFence {
            retired: Arc::clone(&self.retired_fence),
            producer_transition: Arc::clone(&self.producer_transition),
            flow_sync: Arc::clone(&self.flow_sync),
            producer_events: Arc::clone(&self.producer_events),
            #[cfg(test)]
            started: Arc::clone(&self.actor_exit_fence_started),
        };
        loop {
            if self.retired {
                let producer_events = Arc::clone(&self.producer_events);
                tokio::select! {
                    command = receiver.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        self.handle_command(command).await;
                    }
                    _ = producer_events.wait_for_activity() => {
                        if let Some(command) = self.command_before_cutoff(&mut receiver) {
                            self.handle_command(command).await;
                        }
                    }
                }
                continue;
            }
            let deadline = self.next_deadline();
            let producer_events = Arc::clone(&self.producer_events);
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    if let Some(command) = self.command_before_cutoff(&mut receiver) {
                        self.handle_command(command).await;
                    }
                }
                command = receiver.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    self.handle_command(command).await;
                }
                _ = producer_events.wait_for_activity() => {
                    if let Some(command) = self.command_before_cutoff(&mut receiver) {
                        self.handle_command(command).await;
                    }
                }
            }
        }
    }
}

impl RollingControlHandle {
    /// Publish one command through the same bounded sequence as producer
    /// facts. Capacity is reserved before either synchronous fence is taken;
    /// after the transition fence is acquired there is no await or fallible
    /// mailbox operation before the sealed envelope is sent.
    async fn enqueue_command(&self, command: RollingControlCommand) -> Result<(), ()> {
        let permit = self.sender.reserve().await.map_err(|_| ())?;
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope = self.producer_events.seal_command(command);
        permit.send(envelope);
        drop(transition);
        Ok(())
    }

    #[cfg(test)]
    fn try_enqueue_command_for_test(&self, command: RollingControlCommand) -> bool {
        let Ok(permit) = self.sender.try_reserve() else {
            return false;
        };
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope = self.producer_events.seal_command(command);
        permit.send(envelope);
        drop(transition);
        true
    }

    pub(crate) fn spawn(initial_kind: &'static str) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(ROLLING_ACTOR_MAILBOX_CAPACITY);
        let retired = Arc::new(AtomicBool::new(false));
        let producer_attempt = Arc::new(AtomicU64::new(0));
        let now = rolling_now();
        let producer_transition = Arc::new(std::sync::Mutex::new(
            RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
        ));
        let flow_sync = Arc::new(RollingFlowSync::new());
        let producer_events = Arc::new(RollingProducerIngress::new());
        #[cfg(test)]
        let producer_attempt_reply_pause = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let actor_exit_fence_started = Arc::new(tokio::sync::Notify::new());
        let actor = RollingControlActor::with_runtime(
            now,
            initial_kind,
            RollingActorRuntime {
                retired_fence: Arc::clone(&retired),
                producer_attempt: Arc::clone(&producer_attempt),
                producer_transition: Arc::clone(&producer_transition),
                flow_sync: Arc::clone(&flow_sync),
                producer_events: Arc::clone(&producer_events),
                #[cfg(test)]
                producer_attempt_reply_pause: Arc::clone(&producer_attempt_reply_pause),
                #[cfg(test)]
                actor_exit_fence_started: Arc::clone(&actor_exit_fence_started),
            },
        );
        let actor_task = tokio::spawn(actor.run(receiver));
        #[cfg(test)]
        let actor_abort = Some(actor_task.abort_handle());
        drop(actor_task);
        Self {
            sender,
            retired,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_abort,
            #[cfg(test)]
            actor_exit_fence_started,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(ROLLING_ACTOR_MAILBOX_CAPACITY);
        drop(receiver);
        let now = rolling_now();
        Self {
            sender,
            retired: Arc::new(AtomicBool::new(false)),
            producer_attempt: Arc::new(AtomicU64::new(0)),
            producer_transition: Arc::new(std::sync::Mutex::new(
                RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
            )),
            flow_sync: Arc::new(RollingFlowSync::new()),
            producer_events: Arc::new(RollingProducerIngress::new()),
            producer_attempt_reply_pause: Arc::new(std::sync::Mutex::new(None)),
            actor_abort: None,
            actor_exit_fence_started: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[cfg(test)]
    async fn renew(&self, kind: &'static str, source: RollingRenewalSource) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::Renew {
                kind,
                source,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) async fn renew_media(&self, kind: &'static str) -> bool {
        self.renew(kind, RollingRenewalSource::Media).await
    }

    pub(crate) fn current_producer_attempt(&self) -> u64 {
        self.producer_attempt.load(Ordering::Acquire)
    }

    /// Publish the latest ffmpeg telemetry without awaiting actor mailbox
    /// capacity. The coalescer retains the newest fact for this attempt and
    /// refuses a late predecessor observation to evict successor telemetry.
    pub(crate) fn observe_producer_progress(
        &self,
        producer_attempt: u64,
        out_time_ms: Option<i64>,
        speed_milli: Option<i64>,
        recent_speed_milli: Option<i64>,
    ) {
        self.producer_events
            .publish_progress(RollingProducerProgressObservation {
                producer_attempt,
                out_time_ms,
                speed_milli,
                recent_speed_milli,
                observed_at: rolling_now(),
            });
    }

    /// Publish one exact-attempt terminal process fact without making a
    /// recovery decision. The actor rejects stale attempts and preserves the
    /// first terminal observation for the current one.
    pub(crate) fn observe_producer_exit(
        &self,
        producer_attempt: u64,
        success: bool,
        code: Option<i32>,
        signal: Option<i32>,
    ) {
        let _transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Attempt admission, process signalling, actor exit, and terminal
        // publication share this fence. A predecessor that loses to Begin(B)
        // cannot take B's open progress batch and later disappear when B's
        // own exit replaces it.
        if self.retired.load(Ordering::Acquire)
            || self.sender.is_closed()
            || self.current_producer_attempt() != producer_attempt
        {
            ROLLING_PRODUCER_EVENT_INGRESS[1].fetch_add(1, Ordering::Relaxed);
            ROLLING_PRODUCER_EVENT_COALESCED[1].fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.producer_events
            .publish_exit(RollingProducerExitObservation {
                producer_attempt,
                success,
                code,
                signal,
                observed_at: rolling_now(),
            });
    }

    pub(crate) async fn begin_producer_attempt(&self) -> Result<u64, ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::BeginProducerAttempt { reply })
            .await
            .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    #[cfg(test)]
    pub(crate) fn pause_producer_attempt_reply(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .producer_attempt_reply_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    pub(crate) async fn authorize_producer_install(
        &self,
        producer_attempt: u64,
    ) -> Result<(), ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::AuthorizeProducerInstall {
                producer_attempt,
                reply,
            })
            .await
            .is_err()
        {
            return Err(ProducerAttemptRejection::ControlUnavailable);
        }
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    /// Hold the actor's exact producer-deadline fence across a synchronous
    /// ownership publication. Callers renew through
    /// [`Self::authorize_producer_install`] first, then acquire this guard
    /// immediately before assigning a child or inserting its session.
    pub(crate) fn lock_authorized_producer_install(
        &self,
        producer_attempt: u64,
    ) -> Result<std::sync::MutexGuard<'_, RollingProducerTransitionFence>, ProducerAttemptRejection>
    {
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.sender.is_closed() {
            return Err(ProducerAttemptRejection::ControlUnavailable);
        }
        if self.retired.load(Ordering::Acquire) || rolling_now() >= transition.lease_deadline {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if self.producer_attempt.load(Ordering::Acquire) != producer_attempt {
            return Err(ProducerAttemptRejection::StaleAttempt);
        }
        Ok(transition)
    }

    pub(crate) async fn observe_publication(
        &self,
        observation: RollingPublicationObservation,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::ObservePublication { observation, reply })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    pub(crate) async fn commit_media(
        &self,
        kind: &'static str,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::CommitMedia {
                kind,
                producer_attempt,
                segment_index,
                segment_end_ms,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) async fn control(
        &self,
        request: LocalControlRequest<'_>,
    ) -> Result<RollingControlOutcome, ControlStateError> {
        self.control_before(request, i64::MAX, None).await
    }

    pub(crate) async fn control_before(
        &self,
        request: LocalControlRequest<'_>,
        deadline_unix_ms: i64,
        terminal_admission: Option<Arc<dyn RollingTerminalAdmission>>,
    ) -> Result<RollingControlOutcome, ControlStateError> {
        let request = OwnedLocalControlRequest {
            generation: request.generation.to_owned(),
            owner_epoch: request.owner_epoch,
            client_instance_id: request.client_instance_id.to_owned(),
            sequence: request.sequence,
            snapshot: request.snapshot,
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::Control {
            request: Box::new(request),
            deadline_unix_ms,
            terminal_admission,
            reply,
        })
        .await
        .map_err(|_| ControlStateError::Unavailable)?;
        response.await.map_err(|_| ControlStateError::Unavailable)?
    }

    pub(crate) async fn snapshot(&self) -> Option<RollingLeaseSnapshot> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::Snapshot { reply })
            .await
            .ok()?;
        response.await.ok()
    }

    pub(crate) async fn claim_expiry(&self) -> Result<RollingExpiryClaim, ControlStateError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::ClaimExpiry { reply })
            .await
            .map_err(|_| ControlStateError::Unavailable)?;
        response.await.map_err(|_| ControlStateError::Unavailable)
    }

    async fn terminate(
        &self,
        cause: RollingTerminalCause,
    ) -> Result<RollingTerminalOutcome, ControlStateError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::Terminal { cause, reply })
            .await
            .map_err(|_| ControlStateError::Unavailable)?;
        response.await.map_err(|_| ControlStateError::Unavailable)
    }

    pub(crate) async fn end(&self) -> Result<RollingTerminalOutcome, ControlStateError> {
        self.terminate(RollingTerminalCause::End).await
    }

    pub(crate) async fn authority_fence(
        &self,
    ) -> Result<RollingTerminalOutcome, ControlStateError> {
        self.terminate(RollingTerminalCause::AuthorityFence).await
    }

    pub(crate) fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    pub(crate) fn lock_producer_transition(
        &self,
    ) -> std::sync::MutexGuard<'_, RollingProducerTransitionFence> {
        self.producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn producer_transition_guard_is_held_for_test(&self) -> bool {
        match self.producer_transition.try_lock() {
            Ok(_guard) => false,
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(error)) => {
                panic!("producer transition guard is poisoned: {error}")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn producer_flow_applied_for_test(&self) -> Option<(u64, bool)> {
        self.producer_events
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_flow_applied
            .map(|applied| {
                (
                    applied.producer_attempt,
                    applied.state == ProducerPhysicalFlowState::Held,
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn reserve_producer_flow_capacity_for_test(&self) -> bool {
        self.producer_events.reserve_flow_barrier()
    }

    #[cfg(test)]
    pub(crate) fn release_producer_flow_capacity_for_test(&self) {
        self.producer_events.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: self.current_producer_attempt(),
                state: ProducerPhysicalFlowState::Running,
            },
            false,
        );
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_producer_flow_deferral_for_test(&self) {
        self.producer_events
            .flow_capacity_wait_started
            .notified()
            .await;
    }

    #[cfg(test)]
    pub(crate) fn abort_actor_for_test(&self) {
        self.actor_abort
            .as_ref()
            .expect("spawned actor abort handle")
            .abort();
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_actor_exit_fence_for_test(&self) {
        self.actor_exit_fence_started.notified().await;
    }

    /// Authorize the one physical producer transition guarded by `guard`.
    ///
    /// The actor timer and this check share the same mutex and deadline. A
    /// signal that acquires it before the deadline linearizes before expiry;
    /// one that acquires it at or after the deadline is denied even if the
    /// async timer task has not yet published the retired fence.
    pub(crate) fn producer_transition_is_live(
        &self,
        guard: &std::sync::MutexGuard<'_, RollingProducerTransitionFence>,
    ) -> bool {
        self.producer_transition_is_live_at(guard, rolling_now())
    }

    /// Reserve one of the fixed flow-barrier slots before the process syscall.
    /// A full slot set defers the physical transition rather than applying a
    /// state change whose acknowledgement the actor could not retain.
    pub(crate) fn reserve_producer_flow_applied(
        &self,
        _guard: &std::sync::MutexGuard<'_, RollingProducerTransitionFence>,
        producer_attempt: u64,
    ) -> bool {
        if self.is_retired()
            || self.sender.is_closed()
            || self.current_producer_attempt() != producer_attempt
        {
            return false;
        }
        self.producer_events.reserve_flow_barrier()
    }

    /// Settle a reserved physical transition before releasing the exact PID
    /// authorization fence. Success seals and preserves preceding progress as
    /// a sequenced barrier; failure only releases the reservation.
    pub(crate) fn finish_producer_flow_applied(
        &self,
        _guard: &std::sync::MutexGuard<'_, RollingProducerTransitionFence>,
        producer_attempt: u64,
        held: bool,
        applied: bool,
    ) {
        self.producer_events.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt,
                state: if held {
                    ProducerPhysicalFlowState::Held
                } else {
                    ProducerPhysicalFlowState::Running
                },
            },
            applied,
        );
    }

    /// Await actor-drained flow-barrier capacity without holding the exact
    /// attempt transition fence. The child owner rechecks that fence after
    /// waking, so replacement or retirement always wins over the deferred
    /// signal.
    pub(crate) async fn wait_for_producer_flow_capacity(&self) -> bool {
        let available = self
            .producer_events
            .wait_for_flow_barrier_capacity(&self.retired, &self.sender)
            .await;
        if !available && !self.is_retired() {
            self.fence_unavailable();
        }
        available
    }

    fn producer_transition_is_live_at(
        &self,
        guard: &std::sync::MutexGuard<'_, RollingProducerTransitionFence>,
        now: Instant,
    ) -> bool {
        !self.is_retired() && !self.sender.is_closed() && now < guard.lease_deadline
    }

    /// A closed mailbox cannot accept another renewal. The repair loop uses
    /// this fail-closed fence before removing the orphaned session.
    pub(crate) fn fence_unavailable(&self) {
        let _transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.retired.store(true, Ordering::Release);
        self.producer_events.notify_flow_capacity_waiters();
        self.flow_sync.request();
    }

    pub(crate) fn request_flow(&self) -> u64 {
        self.flow_sync.request()
    }

    pub(crate) async fn next_flow_request(&self, handled: u64) -> u64 {
        self.flow_sync.next(handled).await
    }

    pub(crate) fn complete_flow(&self, ticket: u64) {
        self.flow_sync.complete(ticket);
    }

    pub(crate) async fn wait_for_flow(&self, ticket: u64) {
        self.flow_sync.wait_for(ticket).await;
    }

    #[cfg(test)]
    pub(crate) async fn set_renewal_for_test(&self, at: Instant, kind: &'static str) {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::SetRenewalForTest { at, kind, reply })
            .await
            .expect("rolling actor available");
        response.await.expect("rolling actor reply");
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
static ROLLING_LEASE_RENEWALS: [AtomicU64; 3] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static ROLLING_LEASE_EXPIRATIONS: AtomicU64 = AtomicU64::new(0);
static ROLLING_LEASE_RETIREMENTS: AtomicU64 = AtomicU64::new(0);
static ROLLING_PRODUCER_HOLDS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static ROLLING_PRODUCER_RESUMES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static ROLLING_PRODUCER_EVENT_INGRESS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static ROLLING_PRODUCER_EVENT_COALESCED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// Progress accepted/rejected, then exit accepted/rejected.
static ROLLING_PRODUCER_EVENT_OUTCOMES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static ROLLING_PRODUCER_FLOW_DEFERRALS: AtomicU64 = AtomicU64::new(0);
static ROLLING_PRODUCER_DEADLINE_OBSERVATIONS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static ROLLING_CONTROL_COMMANDS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// End won/already-terminal, authority-fence won/already-terminal, then lease
/// expiry won/already-terminal.
static ROLLING_TERMINAL_EVENT_OUTCOMES: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

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

fn hold_reason_index(reason: crate::transcode::AheadHoldReason) -> usize {
    match reason {
        crate::transcode::AheadHoldReason::Demand => 0,
        crate::transcode::AheadHoldReason::Time => 1,
        crate::transcode::AheadHoldReason::Bytes => 2,
        crate::transcode::AheadHoldReason::Global => 3,
    }
}

pub(crate) fn record_producer_hold(reason: crate::transcode::AheadHoldReason) {
    ROLLING_PRODUCER_HOLDS[hold_reason_index(reason)].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_producer_resume(reason: crate::transcode::AheadHoldReason) {
    ROLLING_PRODUCER_RESUMES[hold_reason_index(reason)].fetch_add(1, Ordering::Relaxed);
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
    output.push_str(
        "# HELP plurx_playback_rolling_lease_renewals_total Rolling-session lease renewals accepted by the single control actor.\n\
         # TYPE plurx_playback_rolling_lease_renewals_total counter\n",
    );
    for (index, source) in ["control", "media", "internal"].iter().enumerate() {
        output.push_str(&format!(
            "plurx_playback_rolling_lease_renewals_total{{source=\"{source}\"}} {}\n",
            ROLLING_LEASE_RENEWALS[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(&format!(
        "# HELP plurx_playback_rolling_lease_expirations_total Rolling-session leases atomically claimed after all renewal sources stopped.\n\
         # TYPE plurx_playback_rolling_lease_expirations_total counter\n\
         plurx_playback_rolling_lease_expirations_total {}\n\
         # HELP plurx_playback_rolling_lease_retirements_total Rolling-session actors retired for non-expiry lifecycle reasons.\n\
         # TYPE plurx_playback_rolling_lease_retirements_total counter\n\
         plurx_playback_rolling_lease_retirements_total {}\n",
        ROLLING_LEASE_EXPIRATIONS.load(Ordering::Relaxed),
        ROLLING_LEASE_RETIREMENTS.load(Ordering::Relaxed)
    ));
    output.push_str(
        "# HELP plurx_playback_rolling_terminal_events_total Rolling-session terminal events by bounded cause and immutable first-winner outcome.\n\
         # TYPE plurx_playback_rolling_terminal_events_total counter\n",
    );
    for (event_index, event) in ["end", "authority_fence", "lease_expired"]
        .iter()
        .enumerate()
    {
        for (outcome_index, outcome) in ["won", "already_terminal"].iter().enumerate() {
            output.push_str(&format!(
                "plurx_playback_rolling_terminal_events_total{{event=\"{event}\",outcome=\"{outcome}\"}} {}\n",
                ROLLING_TERMINAL_EVENT_OUTCOMES[event_index * 2 + outcome_index]
                    .load(Ordering::Relaxed)
            ));
        }
    }
    output.push_str(
        "# HELP plurx_playback_rolling_producer_transitions_total Rolling producer hold and resume transitions by actor-owned reason.\n\
         # TYPE plurx_playback_rolling_producer_transitions_total counter\n",
    );
    for (index, reason) in ["demand", "time", "bytes", "global"].iter().enumerate() {
        output.push_str(&format!(
            "plurx_playback_rolling_producer_transitions_total{{transition=\"hold\",reason=\"{reason}\"}} {}\n\
             plurx_playback_rolling_producer_transitions_total{{transition=\"resume\",reason=\"{reason}\"}} {}\n",
            ROLLING_PRODUCER_HOLDS[index].load(Ordering::Relaxed),
            ROLLING_PRODUCER_RESUMES[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(
        "# HELP plurx_playback_rolling_producer_event_ingress_total Nonblocking rolling producer observations submitted, including coalesced samples.\n\
         # TYPE plurx_playback_rolling_producer_event_ingress_total counter\n\
         # HELP plurx_playback_rolling_producer_event_coalesced_total Rolling producer observations folded or deduplicated into bounded same-attempt ingress evidence, or superseded by a newer attempt before actor drain.\n\
         # TYPE plurx_playback_rolling_producer_event_coalesced_total counter\n",
    );
    for (index, event) in ["progress", "exit"].iter().enumerate() {
        output.push_str(&format!(
            "plurx_playback_rolling_producer_event_ingress_total{{event=\"{event}\"}} {}\n\
             plurx_playback_rolling_producer_event_coalesced_total{{event=\"{event}\"}} {}\n",
            ROLLING_PRODUCER_EVENT_INGRESS[index].load(Ordering::Relaxed),
            ROLLING_PRODUCER_EVENT_COALESCED[index].load(Ordering::Relaxed),
        ));
    }
    output.push_str(
        "# HELP plurx_playback_rolling_producer_event_outcomes_total Rolling producer observations accepted or rejected by the exact-attempt actor fence.\n\
         # TYPE plurx_playback_rolling_producer_event_outcomes_total counter\n",
    );
    for (event_index, event) in ["progress", "exit"].iter().enumerate() {
        for (outcome_index, outcome) in ["accepted", "rejected"].iter().enumerate() {
            output.push_str(&format!(
                "plurx_playback_rolling_producer_event_outcomes_total{{event=\"{event}\",outcome=\"{outcome}\"}} {}\n",
                ROLLING_PRODUCER_EVENT_OUTCOMES[event_index * 2 + outcome_index]
                    .load(Ordering::Relaxed),
            ));
        }
    }
    output.push_str(&format!(
        "# HELP plurx_playback_rolling_producer_flow_deferrals_total Physical STOP or CONT transitions deferred before the syscall while bounded actor-ingress barrier capacity was full.\n\
         # TYPE plurx_playback_rolling_producer_flow_deferrals_total counter\n\
         plurx_playback_rolling_producer_flow_deferrals_total {}\n",
        ROLLING_PRODUCER_FLOW_DEFERRALS.load(Ordering::Relaxed),
    ));
    output.push_str(
        "# HELP plurx_playback_rolling_producer_deadline_observations_total Action-passive rolling producer deadlines observed due by bounded mode.\n\
         # TYPE plurx_playback_rolling_producer_deadline_observations_total counter\n",
    );
    for mode in [
        ProducerProgressDeadlineMode::Starting,
        ProducerProgressDeadlineMode::Advancing,
        ProducerProgressDeadlineMode::ClassifyingExit,
    ] {
        let (index, label) = mode.metric();
        output.push_str(&format!(
            "plurx_playback_rolling_producer_deadline_observations_total{{mode=\"{label}\"}} {}\n",
            ROLLING_PRODUCER_DEADLINE_OBSERVATIONS[index].load(Ordering::Relaxed),
        ));
    }
    output.push_str(
        "# HELP plurx_playback_rolling_control_commands_total Sequenced rolling actor commands dequeued by bounded kind.\n\
         # TYPE plurx_playback_rolling_control_commands_total counter\n",
    );
    for (index, kind) in [
        "control",
        "begin_attempt",
        "authorize_install",
        "observe_publication",
        "commit_media",
        "snapshot",
        "claim_expiry",
        "terminal",
    ]
    .iter()
    .enumerate()
    {
        output.push_str(&format!(
            "plurx_playback_rolling_control_commands_total{{kind=\"{kind}\"}} {}\n",
            ROLLING_CONTROL_COMMANDS[index].load(Ordering::Relaxed),
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn terminal_commit_retry_starts_before_but_not_at_or_after_exact_expiry() {
        fn receipt(starts: Arc<AtomicUsize>) -> TerminalCommitReceipt {
            TerminalCommitReceipt::deferred_retryable_until(
                crate::media_sessions::unix_ms().saturating_add(60_000),
                move |attempt| {
                    starts.fetch_add(1, Ordering::AcqRel);
                    attempt.complete(Err(()));
                },
            )
        }

        fn deadline(receipt: &TerminalCommitReceipt) -> tokio::time::Instant {
            receipt
                .expiry
                .deadline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("exact terminal deadline")
        }

        let before_starts = Arc::new(AtomicUsize::new(0));
        let before = receipt(Arc::clone(&before_starts));
        tokio::time::advance(
            deadline(&before)
                .duration_since(tokio::time::Instant::now())
                .saturating_sub(Duration::from_millis(1)),
        )
        .await;
        before.retry();
        assert_eq!(before_starts.load(Ordering::Acquire), 1);
        assert!(before.wait().await.is_err());

        let at_starts = Arc::new(AtomicUsize::new(0));
        let at = receipt(Arc::clone(&at_starts));
        tokio::time::advance(deadline(&at).duration_since(tokio::time::Instant::now())).await;
        at.retry();
        assert_eq!(at_starts.load(Ordering::Acquire), 0);
        assert!(at.wait().await.is_err());

        let after_starts = Arc::new(AtomicUsize::new(0));
        let after = receipt(Arc::clone(&after_starts));
        tokio::time::advance(
            deadline(&after).duration_since(tokio::time::Instant::now()) + Duration::from_millis(1),
        )
        .await;
        after.retry();
        assert_eq!(after_starts.load(Ordering::Acquire), 0);
        assert!(after.wait().await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_commit_success_completion_and_waiter_cannot_cross_exact_expiry() {
        fn response() -> ControlResponseV1 {
            ControlResponseV1 {
                protocol: PROTOCOL_V1.to_owned(),
                generation: "test-terminal-generation".to_owned(),
                control_epoch: 1,
                accepted_sequence: 1,
                server_time_unix_ms: 1,
                lease: PlaybackLeaseView {
                    state: "ended".to_owned(),
                    renew_after_ms: NEXT_EXCHANGE_MS,
                    expires_at_unix_ms: 1,
                },
                delivery: DeliveryView {
                    presentation: "test".to_owned(),
                    producer_state: "complete".to_owned(),
                    produced_through_ms: None,
                    fetched_through_ms: 0,
                    delivered_bps: None,
                    delivered_idle_ms: None,
                    recent_producer_speed: None,
                    client_runway_ms: 0,
                    admitted: None,
                    hold_reason: None,
                    owner_node_hash: "n-test".to_owned(),
                    owner_epoch: 1,
                },
                effective_selection: EffectiveSelection {
                    quality_auto: true,
                    height: 720,
                    audio_track: None,
                    subtitle_burn: None,
                    audio_offset_ms: 0,
                    codec: "test".to_owned(),
                    dynamic_range: Some("sdr".to_owned()),
                },
                action: ControlAction::None,
            }
        }

        let pending_attempt = Arc::new(std::sync::Mutex::new(None::<TerminalCommitAttempt>));
        let completing = TerminalCommitReceipt::deferred_retryable_until(
            crate::media_sessions::unix_ms().saturating_add(60_000),
            {
                let pending_attempt = Arc::clone(&pending_attempt);
                move |attempt| {
                    *pending_attempt
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(attempt);
                }
            },
        );
        let completion_deadline = completing.deadline_for_test();
        tokio::time::advance(
            completion_deadline
                .duration_since(tokio::time::Instant::now())
                .saturating_sub(Duration::from_millis(1)),
        )
        .await;
        completing.retry();
        assert!(pending_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some());
        tokio::time::advance(Duration::from_millis(1)).await;
        pending_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("attempt started before expiry")
            .complete(Ok(response()));
        assert!(
            completing.wait().await.is_err(),
            "an attempt completing at expiry cannot publish success"
        );

        let stored = TerminalCommitReceipt::retryable_until(
            crate::media_sessions::unix_ms().saturating_add(60_000),
            |attempt| attempt.complete(Ok(response())),
        );
        assert!(
            stored.wait().await.is_ok(),
            "success is visible before expiry"
        );
        tokio::time::advance(
            stored
                .deadline_for_test()
                .duration_since(tokio::time::Instant::now()),
        )
        .await;
        assert!(
            stored.wait().await.is_err(),
            "a delayed waiter cannot consume stored success at expiry"
        );
    }

    struct DropReplyOnTerminalAdmission {
        receiver: std::sync::Mutex<
            Option<
                tokio::sync::oneshot::Receiver<Result<RollingControlOutcome, ControlStateError>>,
            >,
        >,
        accepted: AtomicUsize,
    }

    impl RollingTerminalAdmission for DropReplyOnTerminalAdmission {
        fn accepted(&self, _outcome: RollingControlOutcome) {
            self.accepted.fetch_add(1, Ordering::AcqRel);
            drop(
                self.receiver
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take(),
            );
        }
    }

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

        let mut oversized_sequence = request();
        oversized_sequence.sequence = i64::MAX as u64 + 1;
        assert_eq!(
            oversized_sequence.validate(Some(60_000), 2_000),
            Err("sequence")
        );

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

    fn owned_control(request: &ControlRequestV1) -> OwnedLocalControlRequest {
        OwnedLocalControlRequest {
            generation: request.generation.clone(),
            owner_epoch: request.control_epoch,
            client_instance_id: request.client_instance_id.clone(),
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(request),
        }
    }

    #[test]
    fn rolling_actor_keeps_the_complete_demand_and_replay_never_renews() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let request = request();
        let accepted = actor
            .control_at(started + Duration::from_secs(1), owned_control(&request))
            .expect("first control accepted");
        assert_eq!(accepted.disposition, ControlDisposition::Accepted);
        assert_eq!(accepted.lease.mode, RollingLeaseMode::Explicit);
        let accepted_flow_ticket = accepted.flow_ticket;
        let demand = accepted.lease.demand.expect("demand snapshot");
        assert_eq!(demand.position_ms, request.position_ms);
        assert_eq!(demand.buffered_from_ms, request.buffered_from_ms);
        assert_eq!(demand.buffered_through_ms, request.buffered_through_ms);
        assert_eq!(demand.render_state, request.render_state);
        assert_eq!(demand.selection, request.selection);
        assert_eq!(demand.capabilities, request.capabilities);

        let replay = actor
            .control_at(started + Duration::from_secs(11), owned_control(&request))
            .expect("equal sequence is replayed");
        assert_eq!(replay.disposition, ControlDisposition::Replay);
        assert_eq!(replay.lease.idle_for, Duration::from_secs(10));
        assert_eq!(replay.lease.remaining, Duration::from_secs(20));
        assert_eq!(replay.lease.timeout_ms(), ROLLING_EXPLICIT_LEASE_TIMEOUT_MS);
        assert_eq!(replay.lease.last_renewal_kind, "control");
        assert_eq!(replay.flow_ticket, accepted_flow_ticket);
        for _ in 0..100 {
            let replay = actor
                .control_at(started + Duration::from_secs(11), owned_control(&request))
                .expect("repeated equal sequence is replayed");
            assert_eq!(replay.flow_ticket, accepted_flow_ticket);
        }
        assert_eq!(
            actor.flow_sync.requested.load(Ordering::Acquire),
            accepted_flow_ticket,
            "replay waits on accepted convergence without scheduling new flow work"
        );

        let mut stale = request;
        stale.sequence = 0;
        assert_eq!(
            actor.control_at(started + Duration::from_secs(12), owned_control(&stale)),
            Err(ControlStateError::StaleSequence)
        );
        assert_eq!(
            actor
                .snapshot_at(started + Duration::from_secs(12))
                .idle_for,
            Duration::from_secs(11)
        );
    }

    fn publication(
        producer_attempt: u64,
        playlist_ready: bool,
        published_segment: i64,
        published_end_ms: i64,
        resolved_fetched_segment: Option<i64>,
    ) -> RollingPublicationObservation {
        RollingPublicationObservation {
            producer_attempt,
            playlist_ready,
            published_segment: Some(published_segment),
            published_end_ms: Some(published_end_ms),
            next_media_sequence: published_segment + 1,
            resolved_fetched_segment,
            resolved_fetched_end_ms: resolved_fetched_segment.map(|_| published_end_ms),
        }
    }

    #[test]
    fn rolling_delivery_attempt_fences_stale_publication_and_fetch() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let first = actor
            .begin_producer_attempt_at(started)
            .expect("initial producer attempt");
        assert_eq!(first, 1);
        assert!(actor.observe_publication_at(started, publication(first, false, 0, 2_000, None),));

        let second = actor
            .begin_producer_attempt_at(started + Duration::from_secs(1))
            .expect("prepublication replacement");
        assert_eq!(second, 2);
        assert_eq!(actor.delivery.published_segment, None);
        assert!(!actor.observe_publication_at(
            started + Duration::from_secs(2),
            publication(first, true, 8, 18_000, None),
        ));
        assert!(!actor.commit_media_at(
            started + Duration::from_secs(2),
            "segment",
            first,
            Some(8),
            Some(18_000),
        ));
        assert_eq!(
            actor.delivery,
            RollingDeliverySnapshot {
                producer_attempt: second,
                ..RollingDeliverySnapshot::default()
            }
        );

        assert!(actor.observe_publication_at(
            started + Duration::from_secs(2),
            publication(second, true, 1, 4_000, None),
        ));
        assert_eq!(actor.delivery.published_segment, Some(1));
        assert_eq!(actor.delivery.published_end_ms, Some(4_000));
        assert_eq!(actor.delivery.next_media_sequence, 2);
        assert_eq!(
            actor.begin_producer_attempt_at(started + Duration::from_secs(3)),
            Err(ProducerAttemptRejection::PlaylistPublished),
            "client-visible publication returns an exact rejection cause"
        );
    }

    fn producer_progress(
        producer_attempt: u64,
        out_time_ms: i64,
        speed_milli: i64,
        recent_speed_milli: i64,
        observed_at: Instant,
    ) -> RollingProducerProgressObservation {
        RollingProducerProgressObservation {
            producer_attempt,
            out_time_ms: Some(out_time_ms),
            speed_milli: Some(speed_milli),
            recent_speed_milli: Some(recent_speed_milli),
            observed_at,
        }
    }

    fn producer_exit(
        producer_attempt: u64,
        success: bool,
        code: Option<i32>,
        signal: Option<i32>,
        observed_at: Instant,
    ) -> RollingProducerExitObservation {
        RollingProducerExitObservation {
            producer_attempt,
            success,
            code,
            signal,
            observed_at,
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum TerminalModelEvent {
        ProducerExit,
        LeaseExpired,
        ControlEnd,
        AuthorityFence,
        Publication,
        Replacement,
        Control,
    }

    #[derive(Debug)]
    struct TerminalModel {
        terminal: Option<RollingTerminalCause>,
        terminal_wins: usize,
        producer_attempt: u64,
        playlist_ready: bool,
        producer_exit: bool,
        flow_requests: u64,
    }

    impl TerminalModel {
        fn new() -> Self {
            Self {
                terminal: None,
                terminal_wins: 0,
                producer_attempt: 1,
                playlist_ready: false,
                producer_exit: false,
                flow_requests: 0,
            }
        }

        fn terminal(&mut self, cause: RollingTerminalCause) -> bool {
            if self.terminal.is_some() {
                return false;
            }
            self.terminal = Some(cause);
            self.terminal_wins += 1;
            self.flow_requests += 1;
            true
        }
    }

    fn assert_terminal_model_order(order: &[TerminalModelEvent]) {
        let started = Instant::now();
        let retired_fence = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
        let initial_attempt = actor
            .begin_producer_attempt_at(started)
            .expect("model initial producer");
        assert_eq!(initial_attempt, 1);
        let mut model = TerminalModel::new();
        let control = request();

        for (index, event) in order.iter().copied().enumerate() {
            let step = MIN_CONTROL_INTERVAL + Duration::from_millis(1);
            let now = started + step.saturating_mul((index + 1) as u32);
            match event {
                TerminalModelEvent::ProducerExit => {
                    let expected = model.terminal.is_none()
                        && model.producer_attempt == initial_attempt
                        && !model.producer_exit;
                    let accepted = actor
                        .observe_producer_exit_at(
                            now,
                            producer_exit(initial_attempt, false, Some(7), None, now),
                        )
                        .accepted();
                    assert_eq!(accepted, expected, "order {order:?}");
                    model.producer_exit |= expected;
                }
                TerminalModelEvent::LeaseExpired => {
                    let expected = model.terminal(RollingTerminalCause::LeaseExpired);
                    if expected {
                        actor.last_renewal = now - actor.mode.timeout();
                    }
                    let claim = actor.claim_expiry_at(now);
                    let snapshot = match claim {
                        RollingExpiryClaim::Claimed(snapshot) if expected => snapshot,
                        RollingExpiryClaim::Retired(snapshot) if !expected => snapshot,
                        other => panic!("unexpected expiry result {other:?} for order {order:?}"),
                    };
                    assert_eq!(snapshot.terminal, model.terminal, "order {order:?}");
                    assert_eq!(actor.terminal, model.terminal, "order {order:?}");
                }
                TerminalModelEvent::ControlEnd => {
                    let expected = model.terminal(RollingTerminalCause::End);
                    let mut end = control.clone();
                    end.sequence = actor.control.last_sequence.saturating_add(1);
                    end.demand = PlaybackDemand::End;
                    end.playback_rate = 0.0;
                    end.render_state = RenderState::Ended;
                    let outcome = actor.control_at(now, owned_control(&end));
                    assert_eq!(outcome.is_ok(), expected, "order {order:?}");
                    if let Ok(outcome) = outcome {
                        assert_eq!(outcome.disposition, ControlDisposition::Accepted);
                        assert_eq!(outcome.lease.terminal, Some(RollingTerminalCause::End));
                    }
                }
                TerminalModelEvent::AuthorityFence => {
                    let expected = model.terminal(RollingTerminalCause::AuthorityFence);
                    let outcome = actor.terminate(RollingTerminalCause::AuthorityFence);
                    assert_eq!(outcome.won(), expected, "order {order:?}");
                    assert_eq!(
                        outcome.cause(),
                        model.terminal.expect("terminal cause"),
                        "late fence must report the retained winner for order {order:?}"
                    );
                }
                TerminalModelEvent::Publication => {
                    let expected =
                        model.terminal.is_none() && model.producer_attempt == initial_attempt;
                    let accepted = actor.observe_publication_at(
                        now,
                        publication(initial_attempt, true, 1, 4_000, None),
                    );
                    assert_eq!(accepted, expected, "order {order:?}");
                    model.playlist_ready |= expected;
                }
                TerminalModelEvent::Replacement => {
                    let expected = model.terminal.is_none() && !model.playlist_ready;
                    let admitted = actor.begin_producer_attempt_at(now).is_ok();
                    assert_eq!(admitted, expected, "order {order:?}");
                    if expected {
                        model.producer_attempt += 1;
                        model.playlist_ready = false;
                        model.producer_exit = false;
                    }
                }
                TerminalModelEvent::Control => {
                    let expected = model.terminal.is_none();
                    let mut active = control.clone();
                    active.sequence = actor.control.last_sequence.saturating_add(1);
                    let accepted = actor.control_at(now, owned_control(&active)).is_ok();
                    assert_eq!(accepted, expected, "order {order:?}");
                    if expected {
                        model.flow_requests += 1;
                    }
                }
            }

            assert_eq!(actor.terminal, model.terminal, "order {order:?}");
            assert_eq!(actor.retired, model.terminal.is_some(), "order {order:?}");
            assert_eq!(
                actor.expiration_claimed,
                model.terminal == Some(RollingTerminalCause::LeaseExpired),
                "order {order:?}"
            );
            assert_eq!(
                retired_fence.load(Ordering::Acquire),
                model.terminal.is_some(),
                "order {order:?}"
            );
            assert_eq!(
                actor.delivery.producer_attempt, model.producer_attempt,
                "order {order:?}"
            );
            assert_eq!(
                actor.delivery.playlist_ready, model.playlist_ready,
                "order {order:?}"
            );
            assert_eq!(
                actor.delivery.producer_exit.is_some(),
                model.producer_exit,
                "order {order:?}"
            );
            assert_eq!(
                actor.flow_sync.requested.load(Ordering::Acquire),
                model.flow_requests,
                "order {order:?}"
            );
            assert_eq!(model.terminal_wins.min(1), model.terminal_wins);
        }
        assert_eq!(model.terminal_wins, 1, "order {order:?}");
    }

    fn explore_terminal_model_orders(events: &mut [TerminalModelEvent], from: usize) -> usize {
        if from == events.len() {
            assert_terminal_model_order(events);
            return 1;
        }
        let mut explored = 0;
        for index in from..events.len() {
            events.swap(from, index);
            explored += explore_terminal_model_orders(events, from + 1);
            events.swap(from, index);
        }
        explored
    }

    #[test]
    fn terminal_model_explores_every_event_order_with_one_immutable_winner() {
        let mut events = [
            TerminalModelEvent::ProducerExit,
            TerminalModelEvent::LeaseExpired,
            TerminalModelEvent::ControlEnd,
            TerminalModelEvent::AuthorityFence,
            TerminalModelEvent::Publication,
            TerminalModelEvent::Replacement,
            TerminalModelEvent::Control,
        ];
        assert_eq!(explore_terminal_model_orders(&mut events, 0), 5_040);
    }

    #[tokio::test]
    async fn concurrent_end_and_authority_fence_have_one_actor_winner() {
        let handle = RollingControlHandle::spawn("session-start");
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let end = {
            let handle = handle.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                handle.end().await.expect("end verdict")
            })
        };
        let fence = {
            let handle = handle.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                handle.authority_fence().await.expect("fence verdict")
            })
        };
        barrier.wait().await;
        let end = end.await.expect("end task");
        let fence = fence.await.expect("fence task");
        assert_eq!(usize::from(end.won()) + usize::from(fence.won()), 1);
        assert_eq!(end.cause(), fence.cause());
        assert!(matches!(
            end.cause(),
            RollingTerminalCause::End | RollingTerminalCause::AuthorityFence
        ));
        assert_eq!(
            handle.snapshot().await.expect("terminal snapshot").terminal,
            Some(end.cause())
        );
    }

    #[tokio::test]
    async fn accepted_control_end_is_terminal_and_exactly_replayable() {
        let handle = RollingControlHandle::spawn("session-start");
        let mut end = request();
        end.demand = PlaybackDemand::End;
        end.playback_rate = 0.0;
        end.render_state = RenderState::Ended;
        let local = || LocalControlRequest {
            session_id: "unused",
            generation: &end.generation,
            owner_node_id: "node-a",
            owner_epoch: end.control_epoch,
            client_instance_id: &end.client_instance_id,
            sequence: end.sequence,
            snapshot: PlaybackDemandSnapshot::from(&end),
        };

        let accepted = handle.control(local()).await.expect("accepted end");
        assert_eq!(accepted.disposition, ControlDisposition::Accepted);
        assert_eq!(accepted.lease.terminal, Some(RollingTerminalCause::End));
        assert_eq!(accepted.lease.last_renewal_kind, "control-end");
        assert_eq!(
            accepted.lease.demand.as_ref().map(|state| state.demand),
            Some(PlaybackDemand::End)
        );
        assert!(accepted.lease.retired);
        assert!(handle.is_retired());

        let replay = handle.control(local()).await.expect("exact end replay");
        assert_eq!(replay.disposition, ControlDisposition::Replay);
        assert_eq!(replay.accepted_sequence, accepted.accepted_sequence);
        assert_eq!(replay.flow_ticket, accepted.flow_ticket);
        assert_eq!(replay.lease.terminal, Some(RollingTerminalCause::End));

        let mut active_same_sequence = end.clone();
        active_same_sequence.demand = PlaybackDemand::Active;
        active_same_sequence.playback_rate = 1.0;
        active_same_sequence.render_state = RenderState::Rendering;
        assert_eq!(
            handle
                .control(LocalControlRequest {
                    session_id: "unused",
                    generation: &active_same_sequence.generation,
                    owner_node_id: "node-a",
                    owner_epoch: active_same_sequence.control_epoch,
                    client_instance_id: &active_same_sequence.client_instance_id,
                    sequence: active_same_sequence.sequence,
                    snapshot: PlaybackDemandSnapshot::from(&active_same_sequence),
                })
                .await,
            Err(ControlStateError::SessionEnded),
            "a sequence replay cannot substitute a different demand payload"
        );

        let _ = local;
        end.sequence += 1;
        assert_eq!(
            handle
                .control(LocalControlRequest {
                    session_id: "unused",
                    generation: &end.generation,
                    owner_node_id: "node-a",
                    owner_epoch: end.control_epoch,
                    client_instance_id: &end.client_instance_id,
                    sequence: end.sequence,
                    snapshot: PlaybackDemandSnapshot::from(&end),
                })
                .await,
            Err(ControlStateError::SessionEnded),
            "only the exact accepted terminal sequence may replay"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_commands_claim_due_expiry_before_end_or_authority_fence() {
        for cause in [
            RollingTerminalCause::End,
            RollingTerminalCause::AuthorityFence,
        ] {
            let handle = RollingControlHandle::spawn("session-start");
            handle
                .set_renewal_for_test(
                    rolling_now() - ROLLING_LEGACY_LEASE_TIMEOUT,
                    "deadline-race",
                )
                .await;
            let outcome = match cause {
                RollingTerminalCause::End => handle.end().await,
                RollingTerminalCause::AuthorityFence => handle.authority_fence().await,
                RollingTerminalCause::LeaseExpired => unreachable!(),
            }
            .expect("terminal verdict");
            assert_eq!(
                outcome,
                RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::LeaseExpired)
            );
            let snapshot = handle.snapshot().await.expect("terminal snapshot");
            assert_eq!(snapshot.terminal, Some(RollingTerminalCause::LeaseExpired));
            assert!(snapshot.expiration_claimed);
        }
    }

    #[tokio::test]
    async fn late_terminal_commands_return_the_immutable_winning_cause() {
        let handle = RollingControlHandle::spawn("session-start");
        assert_eq!(
            handle.end().await.expect("end verdict"),
            RollingTerminalOutcome::Won(RollingTerminalCause::End)
        );
        assert_eq!(
            handle.authority_fence().await.expect("late fence verdict"),
            RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::End)
        );
    }

    #[test]
    fn rolling_producer_progress_and_exit_are_exact_attempt_terminal_facts() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let predecessor = actor
            .begin_producer_attempt_at(started)
            .expect("predecessor attempt");
        assert!(actor.observe_producer_progress_at(
            started + Duration::from_secs(1),
            producer_progress(
                predecessor,
                1_000,
                900,
                800,
                started + Duration::from_secs(1),
            ),
        ));
        let moving = actor.snapshot_at(started + Duration::from_secs(3)).delivery;
        assert_eq!(moving.producer_out_time_ms, Some(1_000));
        assert_eq!(moving.producer_speed_milli, Some(900));
        assert_eq!(moving.producer_recent_speed_milli, Some(800));
        assert_eq!(moving.producer_progress_idle_ms, 2_000);

        assert!(actor.observe_producer_progress_at(
            started + Duration::from_secs(4),
            producer_progress(predecessor, 900, 700, 600, started + Duration::from_secs(4),),
        ));
        let non_regressing = actor.snapshot_at(started + Duration::from_secs(5)).delivery;
        assert_eq!(non_regressing.producer_out_time_ms, Some(1_000));
        assert_eq!(non_regressing.producer_progress_idle_ms, 4_000);
        assert_eq!(non_regressing.producer_speed_milli, Some(700));

        let successor = actor
            .begin_producer_attempt_at(started + Duration::from_secs(5))
            .expect("successor attempt");
        assert!(!actor.observe_producer_progress_at(
            started + Duration::from_secs(6),
            producer_progress(
                predecessor,
                20_000,
                2_000,
                2_000,
                started + Duration::from_secs(6),
            ),
        ));
        assert!(!actor
            .observe_producer_exit_at(
                started + Duration::from_secs(6),
                producer_exit(
                    predecessor,
                    false,
                    Some(1),
                    None,
                    started + Duration::from_secs(6),
                ),
            )
            .accepted());
        assert!(actor.observe_producer_progress_at(
            started + Duration::from_secs(6),
            producer_progress(
                successor,
                500,
                1_100,
                1_000,
                started + Duration::from_secs(6),
            ),
        ));
        let exit = producer_exit(
            successor,
            false,
            None,
            Some(9),
            started + Duration::from_secs(7),
        );
        assert!(actor
            .observe_producer_exit_at(started + Duration::from_secs(7), exit.clone())
            .accepted());
        assert!(
            actor
                .observe_producer_exit_at(started + Duration::from_secs(8), exit)
                .accepted(),
            "an identical re-observation is idempotent"
        );
        assert!(!actor
            .observe_producer_exit_at(
                started + Duration::from_secs(8),
                producer_exit(
                    successor,
                    true,
                    Some(0),
                    None,
                    started + Duration::from_secs(8),
                ),
            )
            .accepted());
        assert!(!actor.observe_producer_progress_at(
            started + Duration::from_secs(8),
            producer_progress(
                successor,
                1_000,
                1_200,
                1_100,
                started + Duration::from_secs(8),
            ),
        ));
        let terminal = actor.snapshot_at(started + Duration::from_secs(9)).delivery;
        assert_eq!(terminal.producer_out_time_ms, Some(500));
        assert_eq!(
            terminal.producer_exit,
            Some(RollingProducerExitSnapshot {
                success: false,
                code: None,
                signal: Some(9),
                observed_idle_ms: 2_000,
            })
        );
        assert!(actor.terminate(RollingTerminalCause::End).won());
        assert!(!actor.observe_producer_progress_at(
            started + Duration::from_secs(10),
            producer_progress(
                successor,
                2_000,
                1_300,
                1_200,
                started + Duration::from_secs(10),
            ),
        ));
        assert!(!actor
            .observe_producer_exit_at(
                started + Duration::from_secs(10),
                producer_exit(
                    successor,
                    false,
                    None,
                    Some(9),
                    started + Duration::from_secs(10),
                ),
            )
            .accepted());
    }

    #[tokio::test]
    async fn producer_ingress_coalesces_without_mailbox_capacity_or_stale_eviction() {
        let ingress = RollingProducerIngress::new();
        let started = Instant::now();
        ingress.publish_progress(producer_progress(2, 100, 900, 800, started));
        for out_time_ms in 101..=10_000 {
            ingress.publish_progress(producer_progress(2, out_time_ms, 900, 800, started));
        }
        ingress.publish_progress(producer_progress(
            2,
            9_000,
            700,
            600,
            started + Duration::from_secs(1),
        ));
        ingress.publish_progress(producer_progress(1, 99_999, 9_000, 9_000, started));
        ingress.publish_exit(producer_exit(2, false, Some(7), None, started));
        ingress.publish_exit(producer_exit(
            2,
            true,
            Some(0),
            None,
            started + Duration::from_secs(1),
        ));

        let events = ingress.next().await;
        assert_eq!(events.len(), 2, "one progress slot and one exit slot");
        assert!(matches!(
            &events[0],
            RollingProducerEvent::Progress(RollingProducerProgressObservation {
                producer_attempt: 2,
                out_time_ms: Some(10_000),
                speed_milli: Some(700),
                recent_speed_milli: Some(600),
                ..
            })
        ));
        assert!(matches!(
            &events[1],
            RollingProducerEvent::Exit(RollingProducerExitObservation {
                producer_attempt: 2,
                success: false,
                code: Some(7),
                ..
            })
        ));
    }

    #[test]
    fn producer_progress_batch_proves_contiguous_deadlines_and_freezes_the_first_gap() {
        let started = Instant::now();
        let contiguous = RollingProducerIngress::new();
        for (out_time_ms, seconds) in [(100, 0), (200, 9), (300, 18)] {
            contiguous.publish_at(
                RollingProducerEvent::Progress(producer_progress(
                    1,
                    out_time_ms,
                    1_000,
                    900,
                    started + Duration::from_secs(seconds),
                )),
                false,
                started + Duration::from_secs(seconds),
            );
        }
        let contiguous_blocks = contiguous.drain_blocks();
        let [RollingProducerIngressBlock::Progress(contiguous)] = contiguous_blocks.as_slice()
        else {
            panic!("one contiguous progress block");
        };
        assert_eq!(
            contiguous
                .first_advancing
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(100)
        );
        assert_eq!(
            contiguous
                .covered_last
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(300)
        );
        assert_eq!(
            contiguous.covered_deadline,
            Some(started + Duration::from_secs(28))
        );
        assert_eq!(contiguous.first_gap, None);

        let gapped = RollingProducerIngress::new();
        for (out_time_ms, seconds) in [(100, 0), (200, 11), (300, 12)] {
            gapped.publish_at(
                RollingProducerEvent::Progress(producer_progress(
                    1,
                    out_time_ms,
                    1_000,
                    900,
                    started + Duration::from_secs(seconds),
                )),
                false,
                started + Duration::from_secs(seconds),
            );
        }
        let gapped_blocks = gapped.drain_blocks();
        let [RollingProducerIngressBlock::Progress(gapped)] = gapped_blocks.as_slice() else {
            panic!("one gapped progress block");
        };
        assert_eq!(
            gapped
                .covered_last
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(100),
            "late progress cannot extend the proved prefix"
        );
        assert_eq!(
            gapped
                .first_gap
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(200),
            "the first missed link is immutable"
        );
        assert_eq!(
            gapped.latest_telemetry.observation.out_time_ms,
            Some(300),
            "later telemetry remains visible without bridging the gap"
        );

        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        for event in gapped_blocks
            .clone()
            .into_iter()
            .flat_map(RollingProducerIngressBlock::into_events)
        {
            actor.handle_producer_event(event);
        }
        assert_eq!(
            actor
                .snapshot_at(started + Duration::from_secs(13))
                .delivery
                .producer_out_time_ms,
            Some(300),
            "the inactive deadline slice preserves the old latest-progress status"
        );
    }

    #[test]
    fn producer_deadline_starts_exactly_and_retains_scheduler_delayed_coordinate() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                instant: started + PRODUCER_STARTUP_BUDGET,
            })
        );
        assert_eq!(
            actor.settle_due_deadlines_at(
                started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(1)
            ),
            None
        );

        let dispatched_at = started + PRODUCER_STARTUP_BUDGET + Duration::from_secs(7);
        assert_eq!(
            actor.settle_due_deadlines_at(dispatched_at),
            Some(ProducerDeadlineDue {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                deadline: started + PRODUCER_STARTUP_BUDGET,
            }),
            "scheduler delay cannot move the verdict coordinate"
        );
        assert_eq!(actor.producer_progress_deadline, None);
        assert_eq!(actor.settle_due_deadlines_at(dispatched_at), None);
        assert_eq!(
            actor.producer_deadline_due,
            Some(ProducerDeadlineDue {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                deadline: started + PRODUCER_STARTUP_BUDGET,
            }),
            "repeated cutoff cannot replace the original due coordinate"
        );
    }

    #[test]
    fn producer_progress_rearms_from_fenced_publication_and_late_progress_cannot_rescue() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                100,
                1_000,
                900,
                started + Duration::from_secs(60),
            )),
            false,
            started + Duration::from_secs(5),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(6), ingress.drain_blocks());
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(15),
            }),
            "the fenced publication time, not delayed source telemetry, owns rearm"
        );

        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                200,
                1_100,
                1_000,
                started + Duration::from_secs(7),
            )),
            false,
            started + Duration::from_secs(16),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(16), ingress.drain_blocks());
        assert_eq!(
            actor.producer_deadline_due,
            Some(ProducerDeadlineDue {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                deadline: started + Duration::from_secs(15),
            })
        );
        assert_eq!(actor.producer_progress_deadline, None);
        assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
    }

    #[test]
    fn exact_boundary_progress_is_applied_before_the_due_cutoff() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
            false,
            started + PRODUCER_STARTUP_BUDGET,
        );

        actor.handle_producer_blocks_at(
            started + PRODUCER_STARTUP_BUDGET + Duration::from_secs(5),
            ingress.drain_blocks(),
        );
        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + PRODUCER_STARTUP_BUDGET + PRODUCER_PROGRESS_BUDGET,
            })
        );
    }

    #[test]
    fn producer_deadline_accepts_contiguous_coverage_and_rejects_the_first_gap() {
        let started = Instant::now();
        let mut contiguous_actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(contiguous_actor.begin_producer_attempt_at(started), Ok(1));
        let contiguous = RollingProducerIngress::new();
        for (out_time_ms, seconds) in [(100, 25), (200, 34), (300, 43)] {
            contiguous.publish_at(
                RollingProducerEvent::Progress(producer_progress(
                    1,
                    out_time_ms,
                    1_000,
                    900,
                    started + Duration::from_secs(seconds),
                )),
                false,
                started + Duration::from_secs(seconds),
            );
        }
        contiguous_actor.handle_producer_blocks_at(
            started + Duration::from_secs(44),
            contiguous.drain_blocks(),
        );
        assert_eq!(
            contiguous_actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(53),
            })
        );

        let mut gapped_actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(gapped_actor.begin_producer_attempt_at(started), Ok(1));
        let gapped = RollingProducerIngress::new();
        for (out_time_ms, seconds) in [(100, 25), (200, 36), (300, 37)] {
            gapped.publish_at(
                RollingProducerEvent::Progress(producer_progress(
                    1,
                    out_time_ms,
                    1_000,
                    900,
                    started + Duration::from_secs(seconds),
                )),
                false,
                started + Duration::from_secs(seconds),
            );
        }
        gapped_actor
            .handle_producer_blocks_at(started + Duration::from_secs(38), gapped.drain_blocks());
        assert_eq!(
            gapped_actor.producer_deadline_due,
            Some(ProducerDeadlineDue {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                deadline: started + Duration::from_secs(35),
            })
        );
        assert_eq!(gapped_actor.producer_progress_deadline, None);
        assert_eq!(gapped_actor.delivery.producer_out_time_ms, Some(300));
    }

    #[test]
    fn non_success_exit_classifies_immediately_and_later_progress_cannot_rearm() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
            false,
            started + Duration::from_secs(1),
        );
        ingress.publish_at(
            RollingProducerEvent::Exit(producer_exit(
                1,
                false,
                Some(7),
                None,
                started + Duration::from_secs(2),
            )),
            true,
            started + Duration::from_secs(2),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                200,
                1_100,
                1_000,
                started + Duration::from_secs(3),
            )),
            false,
            started + Duration::from_secs(3),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(3), ingress.drain_blocks());
        assert_eq!(actor.delivery.producer_out_time_ms, Some(100));
        assert_eq!(
            actor.producer_process_exit_due,
            Some(ProducerProcessExitDue {
                producer_attempt: 1,
                published_at: started + Duration::from_secs(2),
                code: Some(7),
                signal: None,
            })
        );
        assert_eq!(actor.producer_progress_deadline, None);
        assert_eq!(actor.producer_deadline_due, None);
    }

    #[test]
    fn non_success_exit_at_exact_deadline_wins_over_generic_timeout() {
        let started = Instant::now();
        let deadline = started + PRODUCER_STARTUP_BUDGET;
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Exit(producer_exit(1, false, Some(9), None, deadline)),
            true,
            deadline,
        );
        actor.handle_producer_blocks_at(deadline + Duration::from_secs(1), ingress.drain_blocks());

        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(
            actor.producer_process_exit_due,
            Some(ProducerProcessExitDue {
                producer_attempt: 1,
                published_at: deadline,
                code: Some(9),
                signal: None,
            })
        );
    }

    #[test]
    fn duplicate_progress_updates_telemetry_without_moving_the_deadline() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
            false,
            started + Duration::from_secs(5),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(6), ingress.drain_blocks());
        let armed = actor.producer_progress_deadline;

        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 1_100, 1_000, started)),
            false,
            started + Duration::from_secs(8),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(9), ingress.drain_blocks());
        assert_eq!(actor.producer_progress_deadline, armed);
        assert_eq!(actor.delivery.producer_speed_milli, Some(1_100));
    }

    #[test]
    fn successful_exit_uses_publication_time_and_duplicate_cannot_extend_classification() {
        let started = Instant::now();
        let exit_at = started + PRODUCER_STARTUP_BUDGET - Duration::from_secs(1);
        let classification_at = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
        let exit = producer_exit(1, true, Some(0), None, exit_at);

        let mut before_classification =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(
            before_classification.begin_producer_attempt_at(started),
            Ok(1)
        );
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(RollingProducerEvent::Exit(exit.clone()), true, exit_at);
        before_classification.handle_producer_blocks_at(
            classification_at - Duration::from_millis(1),
            ingress.drain_blocks(),
        );
        assert_eq!(
            before_classification.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::ClassifyingExit,
                instant: classification_at,
            }),
            "delayed dispatch cannot turn a timely exit into startup timeout"
        );

        assert_eq!(
            before_classification
                .observe_producer_exit_at(exit_at + Duration::from_secs(1), exit.clone(),),
            ProducerExitAcceptance::Duplicate
        );
        assert_eq!(
            before_classification.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::ClassifyingExit,
                instant: classification_at,
            }),
            "an idempotent exit cannot move the original classification deadline"
        );

        let mut after_classification =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(
            after_classification.begin_producer_attempt_at(started),
            Ok(1)
        );
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(RollingProducerEvent::Exit(exit), true, exit_at);
        after_classification.handle_producer_blocks_at(
            classification_at + Duration::from_secs(1),
            ingress.drain_blocks(),
        );
        assert_eq!(
            after_classification.producer_deadline_due,
            Some(ProducerDeadlineDue {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::ClassifyingExit,
                deadline: classification_at,
            })
        );
    }

    #[test]
    fn duplicate_exit_before_one_drain_is_counted_as_coalesced_and_preserves_first() {
        let started = Instant::now();
        let first = producer_exit(1, true, Some(0), None, started);
        let before = ROLLING_PRODUCER_EVENT_COALESCED[1].load(Ordering::Relaxed);
        let ingress = RollingProducerIngress::new();

        ingress.publish_at(
            RollingProducerEvent::Exit(first.clone()),
            true,
            started + Duration::from_secs(1),
        );
        ingress.publish_at(
            RollingProducerEvent::Exit(first.clone()),
            true,
            started + Duration::from_secs(2),
        );

        assert_eq!(
            ingress.drain(),
            vec![RollingProducerEvent::Exit(first)],
            "the first exact terminal observation owns the exit barrier"
        );
        assert!(
            ROLLING_PRODUCER_EVENT_COALESCED[1].load(Ordering::Relaxed) > before,
            "a duplicate removed before actor delivery is accounted as coalesced"
        );
    }

    #[test]
    fn stale_predecessor_exit_cannot_settle_before_timely_successor_progress() {
        let started = Instant::now();
        let successor_started = started + Duration::from_secs(10);
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert_eq!(actor.begin_producer_attempt_at(successor_started), Ok(2));

        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Exit(producer_exit(
                1,
                false,
                Some(7),
                None,
                started + Duration::from_secs(31),
            )),
            true,
            started + Duration::from_secs(31),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                2,
                100,
                1_000,
                900,
                started + Duration::from_secs(35),
            )),
            false,
            started + Duration::from_secs(35),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(43), ingress.drain_blocks());

        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 2,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(45),
            })
        );
        assert_eq!(actor.delivery.producer_out_time_ms, Some(100));
    }

    #[test]
    fn cutoff_captures_progress_published_while_waiting_for_the_transition_fence() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = Arc::clone(&actor.producer_events);
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
            false,
            started + Duration::from_secs(25),
        );

        let transition = Arc::clone(&actor.producer_transition);
        let transition_guard = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (started_cutoff, cutoff_started) = std::sync::mpsc::channel();
        let cutoff = std::thread::spawn(move || {
            started_cutoff.send(()).expect("announce cutoff");
            actor.run_due_first_cutoff_at(started + Duration::from_secs(40));
            actor
        });
        cutoff_started.recv().expect("cutoff started");
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 200, 1_100, 1_000, started)),
            false,
            started + Duration::from_secs(34),
        );
        drop(transition_guard);
        let actor = cutoff.join().expect("cutoff actor");

        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(44),
            })
        );
    }

    #[test]
    fn exact_physical_hold_disarms_and_resume_grants_a_fresh_progress_budget() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(25),
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(25),
            },
        );
        assert_eq!(
            actor.settle_due_deadlines_at(started + Duration::from_secs(40)),
            None,
            "intentional physical suspension cannot become a producer timeout"
        );
        assert_eq!(actor.producer_progress_deadline, None);

        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(45),
            ProducerFlowApplied {
                revision: 2,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
                published_at: started + Duration::from_secs(45),
            },
        );
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(55),
            })
        );

        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 1_000, 900, started)),
            false,
            started + Duration::from_secs(50),
        );
        actor.handle_producer_blocks_at(started + Duration::from_secs(51), ingress.drain_blocks());
        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(60),
            })
        );
    }

    #[test]
    fn coalesced_hold_then_resume_still_grants_the_resume_budget() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = Arc::clone(&actor.producer_events);
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
            false,
            started + Duration::from_secs(20),
        );
        ingress.publish_flow_at(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
            },
            started + Duration::from_secs(25),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 200, 900, 800, started)),
            false,
            started + Duration::from_secs(30),
        );
        ingress.publish_flow_at(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
            },
            started + Duration::from_secs(45),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 300, 900, 800, started)),
            false,
            started + Duration::from_secs(50),
        );

        actor.run_due_first_cutoff_at(started + Duration::from_secs(51));
        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(actor.delivery.producer_out_time_ms, Some(300));
        assert_eq!(
            actor.producer_physical_flow,
            ProducerPhysicalFlowState::Running
        );
        assert_eq!(
            actor.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Advancing,
                instant: started + Duration::from_secs(60),
            })
        );
    }

    #[test]
    fn stale_flow_revision_or_attempt_cannot_change_state_or_rearm_deadline() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(5),
            ProducerFlowApplied {
                revision: 2,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(5),
            },
        );
        assert_eq!(actor.producer_flow_revision, 2);
        assert_eq!(
            actor.producer_physical_flow,
            ProducerPhysicalFlowState::Held
        );
        assert_eq!(actor.producer_progress_deadline, None);

        for applied in [
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
                published_at: started + Duration::from_secs(6),
            },
            ProducerFlowApplied {
                revision: 3,
                producer_attempt: 0,
                state: ProducerPhysicalFlowState::Running,
                published_at: started + Duration::from_secs(7),
            },
        ] {
            actor.apply_producer_flow_applied_at(applied.published_at, applied);
            assert_eq!(actor.producer_flow_revision, 2);
            assert_eq!(
                actor.producer_physical_flow,
                ProducerPhysicalFlowState::Held
            );
            assert_eq!(actor.producer_progress_deadline, None);
        }
    }

    #[tokio::test]
    async fn flow_barrier_capacity_defers_then_wakes_a_third_signal_before_application() {
        let ingress = RollingProducerIngress::new();
        assert!(ingress.reserve_flow_barrier());
        assert!(ingress.reserve_flow_barrier());
        assert!(!ingress.reserve_flow_barrier());
        let retired = AtomicBool::new(false);
        let (actor, _receiver) = tokio::sync::mpsc::channel(1);

        let wait_then_reserve = async {
            assert!(
                ingress
                    .wait_for_flow_barrier_capacity(&retired, &actor)
                    .await
            );
            assert!(ingress.reserve_flow_barrier());
            ingress.finish_reserved_flow_barrier(
                RollingProducerFlowObservation {
                    producer_attempt: 1,
                    state: ProducerPhysicalFlowState::Held,
                },
                false,
            );
        };
        let release_capacity = async {
            tokio::task::yield_now().await;
            ingress.finish_reserved_flow_barrier(
                RollingProducerFlowObservation {
                    producer_attempt: 1,
                    state: ProducerPhysicalFlowState::Held,
                },
                false,
            );
        };
        tokio::join!(wait_then_reserve, release_capacity);
        ingress.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
            },
            false,
        );
        assert!(ingress.drain_blocks().is_empty());
    }

    #[test]
    fn command_envelope_retains_flow_capacity_until_actor_application() {
        let ingress = RollingProducerIngress::new();
        for held in [true, false] {
            assert!(ingress.reserve_flow_barrier());
            ingress.finish_reserved_flow_barrier(
                RollingProducerFlowObservation {
                    producer_attempt: 1,
                    state: if held {
                        ProducerPhysicalFlowState::Held
                    } else {
                        ProducerPhysicalFlowState::Running
                    },
                },
                true,
            );
        }
        let (reply, _response) = tokio::sync::oneshot::channel();
        let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
        assert_eq!(envelope.sealed_flow_barriers, 2);
        assert!(
            !ingress.reserve_flow_barrier(),
            "moving barriers into the bounded mailbox must not authorize another syscall"
        );
        ingress.release_sealed_flow_barriers(envelope.sealed_flow_barriers);
        assert!(ingress.reserve_flow_barrier());
        ingress.finish_reserved_flow_barrier(
            RollingProducerFlowObservation {
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Running,
            },
            false,
        );
    }

    #[tokio::test]
    async fn closed_actor_mailbox_fences_and_releases_a_full_capacity_waiter() {
        let control = RollingControlHandle::unavailable_for_test();
        assert!(control.reserve_producer_flow_capacity_for_test());
        assert!(control.reserve_producer_flow_capacity_for_test());

        assert!(!control.wait_for_producer_flow_capacity().await);
        assert!(control.is_retired());

        control.release_producer_flow_capacity_for_test();
        control.release_producer_flow_capacity_for_test();
    }

    #[test]
    fn flow_application_never_outranks_exact_session_expiry() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

        actor.apply_producer_flow_applied_at(
            started + ROLLING_LEGACY_LEASE_TIMEOUT,
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(1),
            },
        );

        assert!(actor.retired);
        assert_eq!(actor.terminal, Some(RollingTerminalCause::LeaseExpired));
        assert_eq!(actor.producer_flow_revision, 0);
        assert_eq!(
            actor.producer_physical_flow,
            ProducerPhysicalFlowState::Running
        );
    }

    #[test]
    fn physical_hold_before_or_at_deadline_disarms_but_late_hold_preserves_due() {
        let started = Instant::now();
        for (offset_ms, due) in [(-1_i64, false), (0, false), (1, true)] {
            let mut actor = RollingControlActor::new(
                started,
                "session-start",
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
            let published_at = if offset_ms < 0 {
                started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(offset_ms.unsigned_abs())
            } else {
                started + PRODUCER_STARTUP_BUDGET + Duration::from_millis(offset_ms as u64)
            };
            actor.apply_producer_flow_applied_at(
                published_at,
                ProducerFlowApplied {
                    revision: 1,
                    producer_attempt: 1,
                    state: ProducerPhysicalFlowState::Held,
                    published_at,
                },
            );
            assert_eq!(actor.producer_deadline_due.is_some(), due);
            assert_eq!(actor.producer_progress_deadline, None);

            actor.apply_producer_flow_applied_at(
                published_at + Duration::from_secs(1),
                ProducerFlowApplied {
                    revision: 2,
                    producer_attempt: 1,
                    state: ProducerPhysicalFlowState::Running,
                    published_at: published_at + Duration::from_secs(1),
                },
            );
            assert_eq!(actor.producer_progress_deadline.is_none(), due);
        }
    }

    #[test]
    fn provisional_deadline_observation_never_outranks_session_lifecycle() {
        let started = Instant::now();
        for terminal_at in [
            started + PRODUCER_STARTUP_BUDGET - Duration::from_millis(1),
            started + PRODUCER_STARTUP_BUDGET,
            started + PRODUCER_STARTUP_BUDGET + Duration::from_millis(1),
        ] {
            let mut actor = RollingControlActor::new(
                started,
                "session-start",
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
            if terminal_at > started + PRODUCER_STARTUP_BUDGET {
                assert!(actor
                    .settle_due_deadlines_at(started + PRODUCER_STARTUP_BUDGET)
                    .is_some());
            }
            assert!(actor.terminate(RollingTerminalCause::End).won());
            assert_eq!(actor.settle_due_deadlines_at(terminal_at), None);
            assert_eq!(actor.producer_deadline_due, None);
            assert_eq!(actor.producer_process_exit_due, None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_commands_before_at_and_after_provisional_producer_due_still_win() {
        for offset in [-1_i64, 0, 1] {
            let handle = RollingControlHandle::spawn("session-start");
            assert_eq!(handle.begin_producer_attempt().await, Ok(1));
            let advance = if offset < 0 {
                PRODUCER_STARTUP_BUDGET - Duration::from_millis(offset.unsigned_abs())
            } else {
                PRODUCER_STARTUP_BUDGET + Duration::from_millis(offset as u64)
            };
            tokio::time::advance(advance).await;
            tokio::task::yield_now().await;

            assert_eq!(
                handle.end().await.expect("lifecycle command"),
                RollingTerminalOutcome::Won(RollingTerminalCause::End),
                "the action-passive producer observation must not own lifecycle ordering at offset {offset}"
            );
            let snapshot = handle.snapshot().await.expect("terminal snapshot");
            assert_eq!(snapshot.terminal, Some(RollingTerminalCause::End));
            assert!(snapshot.retired);
        }
    }

    #[test]
    fn lease_terminal_wins_an_exact_tie_with_the_producer_deadline() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        actor.last_renewal = started - (ROLLING_LEGACY_LEASE_TIMEOUT - PRODUCER_STARTUP_BUDGET);
        let tied = started + PRODUCER_STARTUP_BUDGET;
        assert_eq!(actor.deadline(), tied);
        assert_eq!(actor.next_deadline(), tied);

        assert_eq!(actor.settle_due_deadlines_at(tied), None);
        assert_eq!(actor.terminal, Some(RollingTerminalCause::LeaseExpired));
        assert_eq!(actor.producer_deadline_due, None);
        assert_eq!(actor.producer_progress_deadline, None);
    }

    #[test]
    fn command_envelope_splits_progress_and_preserves_global_ingress_order() {
        let started = Instant::now();
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
            false,
            started,
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                200,
                950,
                850,
                started + Duration::from_secs(1),
            )),
            false,
            started + Duration::from_secs(1),
        );
        let (reply, _response) = tokio::sync::oneshot::channel();
        let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
        assert_eq!(envelope.sealed_flow_barriers, 0);
        let [RollingProducerIngressBlock::Progress(preceding)] =
            envelope.preceding_producer.as_slice()
        else {
            panic!("command seals exactly its preceding progress batch");
        };
        assert!(preceding.last_sequence() < envelope.sequence);
        assert_eq!(
            preceding
                .latest_progress
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(200)
        );

        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                300,
                1_000,
                900,
                started + Duration::from_secs(2),
            )),
            false,
            started + Duration::from_secs(2),
        );
        let successor_blocks = ingress.drain_blocks();
        let [RollingProducerIngressBlock::Progress(successor)] = successor_blocks.as_slice() else {
            panic!("post-command progress starts a successor batch");
        };
        assert!(successor.first_sequence() > envelope.sequence);
        assert_eq!(
            successor
                .first_advancing
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(300)
        );
    }

    #[tokio::test]
    async fn queued_command_cutoff_cannot_drain_its_successor_progress_early() {
        let started = rolling_now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let ingress = Arc::clone(&actor.producer_events);
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
            false,
            started + Duration::from_secs(1),
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        let envelope = ingress.seal_command(RollingControlCommand::Snapshot { reply });
        let command_sequence = envelope.sequence;
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender.try_send(envelope).expect("command queued");
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 200, 900, 800, started)),
            false,
            started + Duration::from_secs(2),
        );

        let selected = actor
            .command_before_cutoff(&mut receiver)
            .expect("queued command wins cutoff");
        assert!(
            ingress
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .progress
                .is_some(),
            "post-command progress remains in ingress"
        );
        actor.handle_command(selected).await;
        let snapshot = response.await.expect("snapshot reply");
        assert_eq!(snapshot.delivery.producer_out_time_ms, Some(100));
        assert_eq!(
            snapshot.producer_control.last_applied_sequence,
            command_sequence
        );

        actor.run_due_first_cutoff_at(started + Duration::from_secs(3));
        assert_eq!(actor.delivery.producer_out_time_ms, Some(200));
        assert!(actor.last_applied_ingress_sequence > command_sequence);
    }

    #[tokio::test(start_paused = true)]
    async fn command_publication_time_not_dispatch_delay_owns_lease_ordering() {
        let published_at = rolling_now();
        let accepted_at = published_at - ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(2);
        let mut actor = RollingControlActor::new(
            accepted_at,
            "session-start",
            Arc::new(AtomicBool::new(false)),
        );
        let (snapshot_reply, snapshot_response) = tokio::sync::oneshot::channel();
        let snapshot_envelope = RollingControlEnvelope {
            sequence: 1,
            published_at,
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::Snapshot {
                reply: snapshot_reply,
            },
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        let envelope = RollingControlEnvelope {
            sequence: 2,
            published_at: published_at + Duration::from_millis(1),
            preceding_producer: Vec::new(),
            sealed_flow_barriers: 0,
            command: RollingControlCommand::Renew {
                kind: "sequenced-before-deadline",
                source: RollingRenewalSource::Internal,
                reply,
            },
        };
        tokio::time::advance(Duration::from_millis(3)).await;
        actor.handle_command(snapshot_envelope).await;
        assert!(
            !snapshot_response
                .await
                .expect("pre-deadline snapshot reply")
                .retired,
            "dispatch delay cannot move a sequenced snapshot past a later renewal"
        );
        actor.handle_command(envelope).await;
        assert!(response.await.expect("renewal reply"));
        assert!(!actor.retired);
        assert_eq!(actor.last_renewal, published_at + Duration::from_millis(1));

        let tied = rolling_now();
        let mut tied_actor = RollingControlActor::new(
            tied - ROLLING_LEGACY_LEASE_TIMEOUT,
            "session-start",
            Arc::new(AtomicBool::new(false)),
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        tied_actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: tied,
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::Terminal {
                    cause: RollingTerminalCause::End,
                    reply,
                },
            })
            .await;
        assert_eq!(
            response.await.expect("terminal reply"),
            RollingTerminalOutcome::AlreadyTerminal(RollingTerminalCause::LeaseExpired)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn real_actor_control_after_producer_deadline_exposes_due_projection() {
        let handle = RollingControlHandle::spawn("session-start");
        let attempt = handle
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        tokio::time::advance(PRODUCER_STARTUP_BUDGET + Duration::from_millis(1)).await;

        let request = request();
        let outcome = handle
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &request.generation,
                owner_node_id: "node-a",
                owner_epoch: request.control_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(&request),
            })
            .await
            .expect("post-deadline control remains compatibility-passive");

        assert_eq!(outcome.lease.producer_control.deadline_attempt, None);
        assert_eq!(outcome.lease.producer_control.deadline_mode, None);
        assert_eq!(outcome.lease.producer_control.due_attempt, Some(attempt));
        assert_eq!(outcome.lease.producer_control.due_mode, Some("starting"));
    }

    #[tokio::test(start_paused = true)]
    async fn real_actor_timer_and_mailbox_tie_settles_due_before_control() {
        let handle = RollingControlHandle::spawn("session-start");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_producer_attempt_reply(Arc::clone(&pause));
        let begin = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.begin_producer_attempt().await })
        };
        pause.wait().await;
        let attempt = handle.current_producer_attempt();
        assert!(attempt > 0);

        tokio::time::advance(PRODUCER_STARTUP_BUDGET).await;
        let request = request();
        let (reply, response) = tokio::sync::oneshot::channel();
        assert!(
            handle.try_enqueue_command_for_test(RollingControlCommand::Control {
                request: Box::new(owned_control(&request)),
                deadline_unix_ms: i64::MAX,
                terminal_admission: None,
                reply,
            })
        );
        pause.wait().await;
        assert_eq!(begin.await.expect("begin task"), Ok(attempt));

        let outcome = response
            .await
            .expect("control response")
            .expect("control accepted");
        assert_eq!(outcome.lease.producer_control.deadline_attempt, None);
        assert_eq!(outcome.lease.producer_control.due_attempt, Some(attempt));
        assert_eq!(outcome.lease.producer_control.due_mode, Some("starting"));
    }

    #[tokio::test(start_paused = true)]
    async fn real_actor_predeadline_command_keeps_publication_time_after_delayed_dispatch() {
        let handle = RollingControlHandle::spawn("session-start");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_producer_attempt_reply(Arc::clone(&pause));
        let begin = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.begin_producer_attempt().await })
        };
        pause.wait().await;
        let attempt = handle.current_producer_attempt();
        assert!(attempt > 0);

        tokio::time::advance(PRODUCER_STARTUP_BUDGET - Duration::from_millis(1)).await;
        let request = request();
        let (reply, response) = tokio::sync::oneshot::channel();
        assert!(
            handle.try_enqueue_command_for_test(RollingControlCommand::Control {
                request: Box::new(owned_control(&request)),
                deadline_unix_ms: i64::MAX,
                terminal_admission: None,
                reply,
            })
        );
        tokio::time::advance(Duration::from_millis(2)).await;
        pause.wait().await;
        assert_eq!(begin.await.expect("begin task"), Ok(attempt));

        let outcome = response
            .await
            .expect("control response")
            .expect("control accepted");
        assert_eq!(
            outcome.lease.producer_control.deadline_attempt,
            Some(attempt)
        );
        assert_eq!(
            outcome.lease.producer_control.deadline_mode,
            Some("starting")
        );
        assert_eq!(outcome.lease.producer_control.due_attempt, None);
        assert_eq!(outcome.lease.producer_control.due_mode, None);
    }

    #[test]
    fn producer_operational_projection_is_bounded_and_tracks_actor_phase() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let starting = actor.producer_operational_snapshot_at(started);
        assert_eq!(starting.phase, "starting");
        assert_eq!(starting.deadline_mode, Some("starting"));
        assert!(starting.observation_only);

        assert!(actor.observe_producer_progress_at(
            started + Duration::from_secs(1),
            producer_progress(1, 100, 900, 800, started + Duration::from_secs(1)),
        ));
        assert_eq!(
            actor
                .producer_operational_snapshot_at(started + Duration::from_secs(1))
                .phase,
            "running"
        );
        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(2),
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(2),
            },
        );
        let held = actor.producer_operational_snapshot_at(started + Duration::from_secs(2));
        assert_eq!(held.phase, "held");
        assert_eq!(held.physical_flow, "held");

        assert!(actor.terminate(RollingTerminalCause::End).won());
        let terminal = actor.producer_operational_snapshot_at(started + Duration::from_secs(3));
        assert_eq!(terminal.phase, "terminal");
        assert_eq!(terminal.deadline_mode, None);
    }

    #[test]
    fn producer_progress_watermark_rejects_a_cross_drain_repeat_as_deadline_evidence() {
        let started = Instant::now();
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 100, 900, 800, started)),
            false,
            started,
        );
        let initial_blocks = ingress.drain_blocks();
        let [RollingProducerIngressBlock::Progress(initial)] = initial_blocks.as_slice() else {
            panic!("initial progress batch");
        };
        assert_eq!(
            initial
                .first_advancing
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(100)
        );

        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                100,
                950,
                850,
                started + Duration::from_secs(5),
            )),
            false,
            started + Duration::from_secs(5),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                150,
                1_000,
                900,
                started + Duration::from_secs(14),
            )),
            false,
            started + Duration::from_secs(14),
        );
        let repeated_blocks = ingress.drain_blocks();
        let [RollingProducerIngressBlock::Progress(repeated)] = repeated_blocks.as_slice() else {
            panic!("post-drain progress batch");
        };
        assert_eq!(
            repeated
                .first_advancing
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(150)
        );
        assert_eq!(
            repeated
                .first_advancing
                .as_ref()
                .map(|sample| sample.published_at),
            Some(started + Duration::from_secs(14)),
            "the repeated 100 at 5s cannot manufacture a deadline link"
        );
        assert_eq!(
            repeated.covered_deadline,
            Some(started + Duration::from_secs(24))
        );
    }

    #[test]
    fn producer_progress_watermark_resets_for_successor_and_rejects_late_predecessor() {
        let started = Instant::now();
        let ingress = RollingProducerIngress::new();
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(1, 50_000, 900, 800, started)),
            false,
            started,
        );
        let predecessor_blocks = ingress.drain_blocks();
        let [RollingProducerIngressBlock::Progress(predecessor)] = predecessor_blocks.as_slice()
        else {
            panic!("predecessor progress batch");
        };
        assert_eq!(
            predecessor
                .latest_progress
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(50_000)
        );

        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                2,
                100,
                1_000,
                900,
                started + Duration::from_secs(1),
            )),
            false,
            started + Duration::from_secs(1),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                1,
                60_000,
                2_000,
                1_900,
                started + Duration::from_secs(2),
            )),
            false,
            started + Duration::from_secs(2),
        );
        ingress.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                2,
                200,
                1_100,
                1_000,
                started + Duration::from_secs(3),
            )),
            false,
            started + Duration::from_secs(3),
        );
        let successor_blocks = ingress.drain_blocks();
        let [RollingProducerIngressBlock::Progress(successor)] = successor_blocks.as_slice() else {
            panic!("successor progress batch");
        };
        assert_eq!(successor.producer_attempt, 2);
        assert_eq!(
            successor
                .first_advancing
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(100),
            "a successor's restarted timeline initializes fresh coverage"
        );
        assert_eq!(
            successor
                .covered_last
                .as_ref()
                .and_then(|sample| sample.observation.out_time_ms),
            Some(200),
            "late predecessor output cannot evict or extend the successor batch"
        );
        let state = ingress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.progress_watermark_attempt, 2);
        assert_eq!(state.progress_watermark_out_time_ms, Some(200));
    }

    #[tokio::test]
    async fn producer_ingress_survives_a_full_actor_mailbox_with_command_causality() {
        let handle = RollingControlHandle::spawn("session-start");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_producer_attempt_reply(Arc::clone(&pause));
        let begin = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.begin_producer_attempt().await })
        };
        pause.wait().await;
        let attempt = handle.current_producer_attempt();
        assert!(attempt > 0, "attempt admission precedes its paused reply");

        for _ in 0..ROLLING_ACTOR_MAILBOX_CAPACITY {
            let (reply, _response) = tokio::sync::oneshot::channel();
            assert!(
                handle.try_enqueue_command_for_test(RollingControlCommand::Snapshot { reply }),
                "paused actor mailbox has the advertised bounded capacity"
            );
        }
        let (overflow_reply, _overflow_response) = tokio::sync::oneshot::channel();
        assert!(
            !handle.try_enqueue_command_for_test(RollingControlCommand::Snapshot {
                reply: overflow_reply
            })
        );

        handle.observe_producer_progress(attempt, Some(5_000), Some(1_250), Some(1_100));
        handle.observe_producer_progress(attempt, Some(6_000), Some(1_200), Some(1_050));
        pause.wait().await;
        assert_eq!(
            begin.await.expect("attempt task"),
            Ok(attempt),
            "the command establishing the attempt remains ordered before its events"
        );
        let delivery = tokio::time::timeout(Duration::from_secs(2), handle.snapshot())
            .await
            .expect("actor drains bounded commands")
            .expect("actor snapshot")
            .delivery;
        assert_eq!(delivery.producer_attempt, attempt);
        assert_eq!(delivery.producer_out_time_ms, Some(6_000));
        assert_eq!(delivery.producer_speed_milli, Some(1_200));
        assert_eq!(delivery.producer_recent_speed_milli, Some(1_050));
    }

    #[tokio::test]
    async fn stale_predecessor_exit_cannot_steal_successor_progress_before_drain() {
        let handle = RollingControlHandle::spawn("session-start");
        let predecessor = handle
            .begin_producer_attempt()
            .await
            .expect("predecessor attempt");
        handle.observe_producer_exit(predecessor, false, Some(11), None);
        let successor = handle
            .begin_producer_attempt()
            .await
            .expect("successor attempt");
        assert!(successor > predecessor);

        handle.observe_producer_progress(successor, Some(7_000), Some(1_100), Some(1_050));
        handle.observe_producer_exit(predecessor, false, Some(12), None);
        handle.observe_producer_exit(successor, false, Some(13), None);
        let snapshot = handle.snapshot().await.expect("ordered actor snapshot");
        assert_eq!(snapshot.delivery.producer_attempt, successor);
        assert_eq!(snapshot.delivery.producer_out_time_ms, Some(7_000));
        let exit = snapshot.delivery.producer_exit.expect("successor exit");
        assert!(!exit.success);
        assert_eq!(exit.code, Some(13));
        assert_eq!(exit.signal, None);
    }

    #[test]
    fn exit_seals_preceding_progress_and_later_progress_stays_terminal() {
        let started = Instant::now();
        let ingress = RollingProducerIngress::new();
        ingress.publish_progress(producer_progress(1, 100, 900, 800, started));
        ingress.publish_exit(producer_exit(
            1,
            false,
            Some(7),
            None,
            started + Duration::from_secs(1),
        ));
        ingress.publish_progress(producer_progress(
            1,
            200,
            1_000,
            900,
            started + Duration::from_secs(2),
        ));
        let events = ingress.drain();
        assert_eq!(
            events.len(),
            3,
            "the exit barrier retains one batch on each side"
        );
        assert!(matches!(&events[0], RollingProducerEvent::Progress(_)));
        assert!(matches!(&events[1], RollingProducerEvent::Exit(_)));
        assert!(matches!(&events[2], RollingProducerEvent::Progress(_)));

        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(
            actor.begin_producer_attempt_at(started),
            Ok(1),
            "fixture uses the ingress attempt"
        );
        for event in events {
            actor.handle_producer_event(event);
        }
        let delivery = actor.snapshot_at(started + Duration::from_secs(3)).delivery;
        assert_eq!(delivery.producer_out_time_ms, Some(100));
        assert_eq!(
            delivery.producer_exit.as_ref().map(|exit| exit.code),
            Some(Some(7))
        );
    }

    #[test]
    fn rolling_equal_fetch_resolves_its_pending_end() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        assert!(actor.commit_media_at(
            started + Duration::from_secs(1),
            "segment",
            attempt,
            Some(3),
            None,
        ));
        assert!(actor.commit_media_at(
            started + Duration::from_secs(2),
            "segment",
            attempt,
            Some(3),
            Some(9_000),
        ));
        assert_eq!(actor.delivery.fetched_segment, Some(3));
        assert_eq!(actor.delivery.fetched_end_ms, 9_000);
        assert_eq!(actor.delivery.pending_fetched_segment, None);
    }

    #[test]
    fn rolling_producer_install_requires_the_exact_live_attempt() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        assert_eq!(
            actor.authorize_producer_install_at(
                started + Duration::from_secs(1),
                attempt.saturating_sub(1),
            ),
            Err(ProducerAttemptRejection::StaleAttempt)
        );
        assert_eq!(
            actor.authorize_producer_install_at(started + Duration::from_secs(1), attempt,),
            Ok(())
        );
        assert_eq!(
            actor.authorize_producer_install_at(
                started + Duration::from_secs(1) + ROLLING_LEGACY_LEASE_TIMEOUT,
                attempt,
            ),
            Err(ProducerAttemptRejection::SessionEnded)
        );
        assert!(actor.retired);
    }

    #[test]
    fn rolling_delivery_resolves_fetch_after_publication_and_stays_monotonic() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        assert!(actor.commit_media_at(
            started + Duration::from_secs(1),
            "segment",
            attempt,
            Some(3),
            None,
        ));
        assert_eq!(actor.delivery.fetched_segment, Some(3));
        assert_eq!(actor.delivery.fetched_end_ms, 0);
        assert_eq!(actor.delivery.pending_fetched_segment, Some(3));

        assert!(actor.observe_publication_at(
            started + Duration::from_secs(1),
            publication(attempt, true, 2, 6_000, Some(2)),
        ));
        assert_eq!(actor.delivery.fetched_end_ms, 0);
        assert_eq!(actor.delivery.pending_fetched_segment, Some(3));

        assert!(actor.observe_publication_at(
            started + Duration::from_secs(1),
            publication(attempt, true, 3, 9_000, Some(3)),
        ));
        assert_eq!(actor.delivery.fetched_end_ms, 9_000);
        assert_eq!(actor.delivery.pending_fetched_segment, None);

        assert!(actor.commit_media_at(
            started + Duration::from_secs(2),
            "segment-range",
            attempt,
            None,
            None,
        ));
        assert!(actor.observe_publication_at(
            started + Duration::from_secs(2),
            publication(attempt, true, 2, 7_000, None),
        ));
        assert_eq!(actor.delivery.published_segment, Some(3));
        assert_eq!(actor.delivery.published_end_ms, Some(9_000));
        assert_eq!(actor.delivery.fetched_segment, Some(3));
        assert_eq!(actor.delivery.fetched_end_ms, 9_000);
    }

    #[test]
    fn rolling_delivery_retirement_rejects_late_mutations() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        assert!(actor.terminate(RollingTerminalCause::End).won());
        assert!(!actor.observe_publication_at(
            started + Duration::from_secs(1),
            publication(attempt, true, 1, 4_000, None),
        ));
        assert!(!actor.commit_media_at(
            started + Duration::from_secs(1),
            "segment",
            attempt,
            Some(1),
            Some(4_000),
        ));
        assert_eq!(actor.delivery.published_segment, None);
        assert_eq!(actor.delivery.fetched_segment, None);
    }

    #[test]
    fn rolling_delivery_expiry_rejects_late_publication() {
        let started = Instant::now();
        let retired_fence = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");

        assert!(!actor.observe_publication_at(
            started + ROLLING_LEGACY_LEASE_TIMEOUT,
            publication(attempt, true, 1, 4_000, None),
        ));
        assert!(actor.retired);
        assert!(actor.expiration_claimed);
        assert!(retired_fence.load(Ordering::Acquire));
        assert_eq!(actor.delivery.published_segment, None);
    }

    #[test]
    fn rolling_absolute_expiry_subtracts_reply_delay_from_the_deadline() {
        let started = Instant::now();
        let actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let snapshot = actor.snapshot_at(started + Duration::from_secs(10));
        assert_eq!(snapshot.remaining, Duration::from_secs(50));
        assert_eq!(
            snapshot.expires_at_unix_ms_at(started + Duration::from_secs(25), 1_000_000),
            1_035_000,
            "a delayed oneshot reply cannot move the real monotonic deadline"
        );
    }

    #[test]
    fn rolling_actor_claims_expiry_once_and_fences_every_later_renewal() {
        let started = Instant::now();
        let retired_fence = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
        let claimed = actor
            .claim_expiry_at(started + ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(1));
        assert!(matches!(
            claimed,
            RollingExpiryClaim::Claimed(ref lease)
                if lease.retired && lease.expiration_claimed && !lease.expired()
        ));
        assert!(retired_fence.load(Ordering::Acquire));
        assert!(matches!(
            actor.claim_expiry_at(started + ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_secs(1)),
            RollingExpiryClaim::Retired(_)
        ));
        assert!(!actor.renew_at(
            started + Duration::from_secs(62),
            "segment",
            RollingRenewalSource::Media,
        ));
        assert_eq!(
            actor.control_at(started + Duration::from_secs(63), owned_control(&request())),
            Err(ControlStateError::SessionEnded)
        );

        let mut renewed =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert!(renewed.renew_at(
            started + Duration::from_secs(59),
            "playlist",
            RollingRenewalSource::Media,
        ));
        assert_eq!(
            renewed.claim_expiry_at(started + Duration::from_secs(61)),
            RollingExpiryClaim::Live
        );
        assert!(matches!(
            renewed.claim_expiry_at(started + Duration::from_secs(120)),
            RollingExpiryClaim::Claimed(_)
        ));
    }

    #[test]
    fn explicit_deadline_cannot_be_revived_between_repair_ticks() {
        let started = Instant::now();
        let retired_fence = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::clone(&retired_fence));
        let request = request();
        let accepted_at = started + Duration::from_secs(20);
        let accepted = actor
            .control_at(accepted_at, owned_control(&request))
            .expect("legacy lease accepts the first explicit control");
        assert_eq!(accepted.lease.mode, RollingLeaseMode::Explicit);
        assert_eq!(accepted.lease.remaining, ROLLING_EXPLICIT_LEASE_TIMEOUT);

        let deadline = accepted_at + ROLLING_EXPLICIT_LEASE_TIMEOUT;
        assert!(actor.renew_at(
            deadline - Duration::from_millis(1),
            "segment",
            RollingRenewalSource::Media,
        ));
        let renewed_deadline = deadline - Duration::from_millis(1) + ROLLING_EXPLICIT_LEASE_TIMEOUT;
        assert!(!actor.renew_at(renewed_deadline, "playlist", RollingRenewalSource::Media,));
        assert!(retired_fence.load(Ordering::Acquire));
        assert_eq!(
            actor.control_at(
                renewed_deadline + Duration::from_millis(1),
                owned_control(&request)
            ),
            Err(ControlStateError::SessionEnded)
        );
    }

    #[test]
    fn thirty_minute_foreground_hold_remains_live_on_delivered_heartbeats() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let mut request = request();
        request.demand = PlaybackDemand::Hold;
        request.playback_rate = 0.0;
        request.render_state = RenderState::Waiting;
        let mut now = started + Duration::from_secs(1);
        for sequence in 1..=360 {
            request.sequence = sequence;
            let outcome = actor
                .control_at(now, owned_control(&request))
                .expect("delivered hold heartbeat remains admissible");
            assert_eq!(outcome.disposition, ControlDisposition::Accepted);
            assert_eq!(
                outcome.lease.demand.as_ref().map(|demand| demand.demand),
                Some(PlaybackDemand::Hold)
            );
            now += Duration::from_secs(5);
        }
        let lease = actor.snapshot_at(now);
        assert!(!lease.expired());
        assert_eq!(lease.mode, RollingLeaseMode::Explicit);
        assert_eq!(lease.remaining, Duration::from_secs(25));
    }

    #[tokio::test]
    async fn rolling_mailbox_orders_media_retirement_and_observation() {
        let handle = RollingControlHandle::spawn("session-start");
        assert!(handle.renew_media("playlist").await);
        assert_eq!(
            handle.snapshot().await.expect("snapshot").last_renewal_kind,
            "playlist"
        );
        assert!(handle.end().await.expect("end verdict").won());
        assert!(!handle.renew_media("segment").await);
        assert!(handle.snapshot().await.is_some_and(|lease| lease.retired));
    }

    #[tokio::test]
    async fn flow_completion_between_check_and_await_cannot_be_lost() {
        let sync = Arc::new(RollingFlowSync::new());
        let ticket = sync.request();
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *sync
            .wait_after_check
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let waiter = {
            let sync = Arc::clone(&sync);
            tokio::spawn(async move { sync.wait_for(ticket).await })
        };

        // The waiter has registered but has not awaited the notification.
        // `notify_waiters` in this exact interval must still release it.
        pause.wait().await;
        sync.complete(ticket);
        pause.wait().await;
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("registered completion wake was retained")
            .expect("waiter task");
    }

    #[tokio::test]
    async fn unavailable_mailbox_fence_orders_after_an_inflight_producer_signal() {
        let handle = RollingControlHandle::spawn("session-start");
        let transition = handle.lock_producer_transition();
        assert!(handle.producer_transition_is_live(&transition));
        let (fenced, observed) = std::sync::mpsc::channel();
        let worker = {
            let handle = handle.clone();
            std::thread::spawn(move || {
                handle.fence_unavailable();
                fenced.send(()).expect("fence observation");
            })
        };

        assert!(
            observed.recv_timeout(Duration::from_millis(50)).is_err(),
            "fail-closed retirement cannot publish through the signal gate"
        );
        assert!(!handle.is_retired());
        drop(transition);
        observed
            .recv_timeout(Duration::from_secs(1))
            .expect("fence follows the signal transition");
        worker.join().expect("fence thread");
        assert!(handle.is_retired());
    }

    #[tokio::test(start_paused = true)]
    async fn actor_timer_publishes_the_fence_at_each_modes_exact_deadline() {
        let legacy = RollingControlHandle::spawn("session-start");
        tokio::task::yield_now().await;
        tokio::time::advance(ROLLING_LEGACY_LEASE_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(legacy.is_retired(), "legacy deadline is actor-owned");
        assert!(legacy
            .snapshot()
            .await
            .is_some_and(|lease| { lease.retired && lease.expiration_claimed }));

        let explicit = RollingControlHandle::spawn("session-start");
        let request = request();
        explicit
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &request.generation,
                owner_node_id: "node-a",
                owner_epoch: request.control_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(&request),
            })
            .await
            .expect("explicit mode accepted");
        tokio::time::advance(ROLLING_EXPLICIT_LEASE_TIMEOUT - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(!explicit.is_retired());
        let before_deadline = explicit.snapshot().await.expect("live explicit snapshot");
        assert_eq!(before_deadline.remaining, Duration::from_millis(1));
        let wall_before = crate::media_sessions::unix_ms();
        let advertised_expiry = before_deadline.expires_at_unix_ms();
        let wall_after = crate::media_sessions::unix_ms();
        assert!(
            advertised_expiry >= wall_before && advertised_expiry <= wall_after.saturating_add(1),
            "wire expiry uses the same paused monotonic clock as the actor"
        );
        {
            let producer_transition = explicit.lock_producer_transition();
            let deadline = producer_transition.lease_deadline;
            assert!(explicit.producer_transition_is_live_at(
                &producer_transition,
                deadline - Duration::from_millis(1)
            ));
            assert!(
                !explicit.producer_transition_is_live_at(&producer_transition, deadline),
                "a producer signal is denied at the exact deadline"
            );
        }
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(explicit.is_retired(), "explicit deadline is actor-owned");
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_command_claims_expiry_at_the_exact_actor_deadline() {
        let deadline = rolling_now();
        let accepted_at = deadline - ROLLING_EXPLICIT_LEASE_TIMEOUT;
        let retired_fence = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(accepted_at, "session-start", Arc::clone(&retired_fence));
        let accepted = actor
            .control_at(accepted_at, owned_control(&request()))
            .expect("control establishes an explicit lease");
        assert_eq!(accepted.lease.deadline, deadline);

        // Drive the Snapshot command itself at the exact deadline. This is
        // independent of timer scheduling and fails if the handler ever
        // returns a zero-remaining live lease instead of claiming expiry.
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command_for_test(RollingControlCommand::Snapshot { reply })
            .await;
        let deadline_snapshot = response.await.expect("snapshot command reply");
        assert!(deadline_snapshot.retired, "snapshot linearizes expiry");
        assert!(deadline_snapshot.expiration_claimed);
        assert!(retired_fence.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn dropped_replies_preserve_the_shared_fence_and_committed_sequence() {
        let expiry = RollingControlHandle::spawn("session-start");
        expiry
            .set_renewal_for_test(
                Instant::now() - ROLLING_LEGACY_LEASE_TIMEOUT - Duration::from_secs(1),
                "before-expiry",
            )
            .await;
        let (reply, dropped) = tokio::sync::oneshot::channel();
        expiry
            .enqueue_command(RollingControlCommand::ClaimExpiry { reply })
            .await
            .expect("expiry command queued");
        drop(dropped);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !expiry.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("expiry publishes shared fence before its dropped reply");
        assert!(matches!(
            expiry.claim_expiry().await,
            Ok(RollingExpiryClaim::Retired(_))
        ));

        let retirement = RollingControlHandle::spawn("session-start");
        let (reply, dropped) = tokio::sync::oneshot::channel();
        retirement
            .enqueue_command(RollingControlCommand::Terminal {
                cause: RollingTerminalCause::End,
                reply,
            })
            .await
            .expect("retirement command queued");
        drop(dropped);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !retirement.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retirement publishes shared fence before its dropped reply");
        assert_eq!(
            retirement
                .snapshot()
                .await
                .expect("committed terminal snapshot")
                .terminal,
            Some(RollingTerminalCause::End)
        );

        let control = RollingControlHandle::spawn("session-start");
        let request = request();
        let never_polled = control.control(LocalControlRequest {
            session_id: "unused",
            generation: &request.generation,
            owner_node_id: "node-a",
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
        });
        drop(never_polled);
        assert_eq!(
            control.snapshot().await.expect("snapshot").mode,
            RollingLeaseMode::Legacy,
            "cancellation before enqueue mutates nothing"
        );

        let (reply, dropped) = tokio::sync::oneshot::channel();
        drop(dropped);
        control
            .enqueue_command(RollingControlCommand::Control {
                request: Box::new(owned_control(&request)),
                deadline_unix_ms: i64::MAX,
                terminal_admission: None,
                reply,
            })
            .await
            .expect("control command queued");
        tokio::time::sleep(MIN_CONTROL_INTERVAL).await;
        let replay = control
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &request.generation,
                owner_node_id: "node-a",
                owner_epoch: request.control_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(&request),
            })
            .await
            .expect("request remains admissible");
        assert_eq!(replay.disposition, ControlDisposition::Accepted);

        let expired = RollingControlHandle::spawn("session-start");
        let (reply, response) = tokio::sync::oneshot::channel();
        expired
            .enqueue_command(RollingControlCommand::Control {
                request: Box::new(owned_control(&request)),
                deadline_unix_ms: crate::media_sessions::unix_ms(),
                terminal_admission: None,
                reply,
            })
            .await
            .expect("expired control command queued");
        assert_eq!(
            response.await.expect("expired control reply"),
            Err(ControlStateError::Unavailable)
        );
        assert_eq!(
            expired.snapshot().await.expect("expired snapshot").mode,
            RollingLeaseMode::Legacy,
            "a queued control cannot mutate after its inherited deadline"
        );

        let terminal_control = RollingControlHandle::spawn("session-start");
        let mut end = request.clone();
        end.demand = PlaybackDemand::End;
        end.playback_rate = 0.0;
        end.render_state = RenderState::Ended;
        let (reply, dropped) = tokio::sync::oneshot::channel();
        let admission = Arc::new(DropReplyOnTerminalAdmission {
            receiver: std::sync::Mutex::new(Some(dropped)),
            accepted: AtomicUsize::new(0),
        });
        terminal_control
            .enqueue_command(RollingControlCommand::Control {
                request: Box::new(owned_control(&end)),
                deadline_unix_ms: i64::MAX,
                terminal_admission: Some(admission.clone()),
                reply,
            })
            .await
            .expect("terminal control queued");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !terminal_control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor transfers accepted End before its reply send");
        assert_eq!(admission.accepted.load(Ordering::Acquire), 1);
        let recovered = terminal_control
            .control(LocalControlRequest {
                session_id: "unused",
                generation: &end.generation,
                owner_node_id: "node-a",
                owner_epoch: end.control_epoch,
                client_instance_id: &end.client_instance_id,
                sequence: end.sequence,
                snapshot: PlaybackDemandSnapshot::from(&end),
            })
            .await
            .expect("lost terminal response is exactly replayable");
        assert_eq!(recovered.disposition, ControlDisposition::Replay);
        assert_eq!(recovered.lease.terminal, Some(RollingTerminalCause::End));
    }

    #[tokio::test]
    async fn mailbox_expiry_and_media_renewal_have_one_ordered_winner() {
        let handle = RollingControlHandle::spawn("session-start");
        handle
            .set_renewal_for_test(
                Instant::now() - ROLLING_LEGACY_LEASE_TIMEOUT - Duration::from_secs(1),
                "before-race",
            )
            .await;
        let renewal = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.renew_media("segment").await })
        };
        let expiry = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.claim_expiry().await })
        };
        let renewed = renewal.await.expect("renewal task");
        let claim = expiry.await.expect("expiry task").expect("actor available");
        assert!(
            !renewed,
            "a renewal after the deadline cannot revive the lease"
        );
        assert!(
            matches!(
                claim,
                RollingExpiryClaim::Claimed(_) | RollingExpiryClaim::Retired(_)
            ),
            "one command claims expiry and the other observes its fence"
        );
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
        assert!(request.is_valid());
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
        let mut exited = response.clone();
        exited.delivery.presentation = "live-recovery".to_owned();
        exited.delivery.producer_state = "exited".to_owned();
        assert!(
            exited.is_valid_for(&request),
            "a remote owner may truthfully report an unsuccessful producer exit"
        );
        let mut invented_state = response;
        invented_state.delivery.producer_state = "probably_running".to_owned();
        assert!(!invented_state.is_valid_for(&request));

        let mut terminal_request = request.clone();
        terminal_request.control.demand = PlaybackDemand::End;
        terminal_request.control.playback_rate = 0.0;
        terminal_request.control.render_state = RenderState::Ended;
        let mut terminal_response = exited;
        terminal_response.accepted_sequence = terminal_request.control.sequence;
        terminal_response.lease.state = "ended".to_owned();
        terminal_response.lease.expires_at_unix_ms = terminal_response.server_time_unix_ms;
        assert!(terminal_response.is_valid_for(&terminal_request));
        let mut active_terminal_response = terminal_response.clone();
        active_terminal_response.lease.state = "active".to_owned();
        assert!(!active_terminal_response.is_valid_for(&terminal_request));
        assert!(
            !terminal_response.is_valid_for(&request),
            "an ended lease is only valid for the exact terminal demand"
        );

        let mut oversized_sequence = terminal_request.clone();
        oversized_sequence.control.sequence = i64::MAX as u64 + 1;
        assert!(!oversized_sequence.is_valid());

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
        assert!(metrics.contains("plurx_playback_rolling_lease_renewals_total{source=\"control\"}"));
        assert!(metrics.contains("plurx_playback_rolling_lease_expirations_total"));
        assert!(metrics.contains(
            "plurx_playback_rolling_terminal_events_total{event=\"authority_fence\",outcome=\"already_terminal\"}"
        ));
        assert!(metrics.contains(
            "plurx_playback_rolling_producer_transitions_total{transition=\"hold\",reason=\"demand\"}"
        ));
        assert!(metrics
            .contains("plurx_playback_rolling_producer_event_ingress_total{event=\"progress\"}"));
        assert!(metrics
            .contains("plurx_playback_rolling_producer_event_coalesced_total{event=\"exit\"}"));
        assert!(metrics.contains(
            "plurx_playback_rolling_producer_event_outcomes_total{event=\"exit\",outcome=\"rejected\"}"
        ));
        assert!(metrics.contains("plurx_playback_rolling_producer_flow_deferrals_total"));
        assert!(metrics.contains(
            "plurx_playback_rolling_producer_deadline_observations_total{mode=\"advancing\"}"
        ));
        assert!(
            metrics.contains("plurx_playback_rolling_control_commands_total{kind=\"snapshot\"}")
        );
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
