//! Versioned playback-control messages and per-generation sequence fencing.
//!
//! M1 is deliberately behavior-neutral: accepted exchanges renew the legacy
//! delivery clock and return an observation with `action: none`.  Keeping the
//! wire contract and the mutation fence here prevents later policy work from
//! leaking into HTTP routing or reintroducing several independent recovery
//! owners.

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::Weak;
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
/// The protocol names seven actions. A client may name more than it will ever
/// receive, so the bound is generous; it exists to stop an unbounded list, not
/// to police the vocabulary.
const MAX_SUPPORTED_ACTIONS: usize = 16;
const MAX_ACTION_NAME_LEN: usize = 32;
const HOLD_ACTION: &str = "hold";
const TERMINAL_ACTION: &str = "terminal";
const RETRY_RESOURCE_ACTION: &str = "retry_resource";
/// The longest explanation a terminal action carries, matching the error
/// body's bound: it is operator-facing text on a client-visible path.
const MAX_TERMINAL_MESSAGE_BYTES: usize = 512;
const MIN_CONTROL_INTERVAL: Duration = Duration::from_millis(250);
const RELAY_BUCKETS_MS: [u64; 9] = [10, 25, 50, 100, 250, 500, 1_000, 2_500, 4_000];
/// Compatibility budget retained only by legacy actor fixtures. Production
/// attempts use their typed hardware, software, or copy policy budget.
#[cfg(test)]
const PRODUCER_STARTUP_BUDGET: Duration = Duration::from_secs(30);
const PREPUBLICATION_HARDWARE_STARTUP_BUDGET: Duration = Duration::from_secs(12);
const PREPUBLICATION_SOFTWARE_STARTUP_BUDGET: Duration = Duration::from_secs(30);
const PREPUBLICATION_COPY_STARTUP_BUDGET: Duration = Duration::from_secs(30);
/// Behavior-compatible advancing-output horizon used by the M4 producer
/// ingress proof. Legacy actors retain it passively; opted-in prepublication
/// actors may use their policy-specific horizon to emit one decision.
const PRODUCER_PROGRESS_BUDGET: Duration = Duration::from_secs(10);
/// Bound for exact-exit classification. It remains action-passive for legacy
/// actors and decision-bearing only in the opted-in prepublication scope.
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
    /// Action type names this client will apply. Absent or empty means the
    /// client is passive, and the server may then return only `none`.
    ///
    /// This is the fence that makes server-first deployment safe. Every M2
    /// reporter stops reporting on any action it does not recognise, so a
    /// server that returned a new action to an older client would silence it
    /// for the rest of the film — with no partial failure to notice, because
    /// playback itself never depended on the control plane. The server
    /// therefore never sends an action the client has not named.
    ///
    /// Unknown names are ignored rather than refused: a client from a later
    /// version must be able to name actions this server has never heard of.
    pub supported_actions: Option<Vec<String>>,
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
        if let Some(actions) = &self.supported_actions {
            // Bounded because it is attacker-controlled input on an
            // authenticated but client-driven path. The names themselves are
            // not checked against a vocabulary: an unrecognised action name is
            // a client this server is older than, which must not be a 400.
            if actions.len() > MAX_SUPPORTED_ACTIONS
                || actions
                    .iter()
                    .any(|name| name.is_empty() || name.len() > MAX_ACTION_NAME_LEN)
            {
                return Err("supported_actions");
            }
        }
        Ok(())
    }

    /// Whether this client said it will apply `hold`.
    pub(crate) fn accepts_hold(&self) -> bool {
        self.accepts(HOLD_ACTION)
    }

    /// Whether this client said it will apply the named action.
    pub(crate) fn accepts(&self, action: &str) -> bool {
        self.supported_actions
            .as_deref()
            .is_some_and(|actions| actions.iter().any(|name| name == action))
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
            && self
                .delivery
                .producer_decision
                .as_deref()
                .is_none_or(|value| ProducerDecisionReason::from_status(value).is_some())
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
            && self.action_is_believable(request)
    }

    /// Whether the action this peer returned is one the relaying node should
    /// forward.
    ///
    /// A relayed hold is only believable if the same response's delivery
    /// reports the same hold, and if the requesting client asked for the
    /// action at all. Anything else is a peer inventing an instruction out of
    /// a fact it did not send, and the client would obey it.
    fn action_is_believable(&self, request: &ControlRelayRequest) -> bool {
        let declared = self
            .action
            .vocabulary_name()
            .is_none_or(|name| request.control.accepts(name));
        declared
            && match &self.action {
                ControlAction::None => true,
                ControlAction::Hold { reason } => {
                    self.delivery.hold_reason.as_deref() == Some(reason.as_delivery_str())
                }
                // Both verdicts must agree with the decision the same response
                // reported, and the permanence must match the verdict: a peer
                // calling a retryable decision terminal would end a playable
                // session, and the reverse would loop a client forever.
                ControlAction::Terminal { code, message } => {
                    self.delivery.producer_decision.as_deref() == Some(code.status())
                        && code.is_permanent()
                        && !message.is_empty()
                        && message.len() <= MAX_TERMINAL_MESSAGE_BYTES
                }
                ControlAction::RetryResource { after_ms, reason } => {
                    self.delivery.producer_decision.as_deref() == Some(reason.status())
                        && !reason.is_permanent()
                        && *after_ms > 0
                        && *after_ms <= 60_000
                }
            }
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
    /// Why the producer stopped, when it stopped for a reason this server has
    /// named. `producer_state` says only `failed`; this says which of the
    /// fourteen decisions that was, and therefore whether trying again could
    /// ever work. Optional: an older peer relaying a response has no such
    /// field, and absence means "not classified here", never "healthy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_decision: Option<String>,
    /// Readiness of the native text subtitle track this client's selection
    /// names, when this server evaluated it.
    ///
    /// One of `ready`, `warming`, or `unavailable`. `ready` means cached text
    /// covering this control snapshot's demand window is servable now, either
    /// from its bounded window sidecar or the authoritative whole-track one.
    /// `warming` means materialization is in flight and segments stay empty
    /// until it lands. `unavailable` means stop asking: no such track,
    /// a bitmap track that can only be burned in, or an extraction that failed
    /// and whose memo is still live. The cause is deliberately not on the
    /// wire — the client's behaviour is identical for all three, and the
    /// distinctions are already in the logs and the 415 message.
    ///
    /// Absent means "not classified here", never "ready": an older peer
    /// relaying a response carries no such field, and this is populated only
    /// when the selection actually names a native text track. A value this
    /// client does not recognise is treated as absent for the same reason.
    ///
    /// It exists so a client stops guessing when to re-fetch an empty subtitle
    /// segment. The segment route answers `WEBVTT\n\n` with `no-store` while a
    /// sidecar warms, and nothing told the client when that stopped being the
    /// answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_readiness: Option<String>,
    pub owner_node_hash: String,
    pub owner_epoch: u64,
}

/// What the cache knows about the selected subtitle track, as the control
/// plane needs it. Mirrors `subtitles::SidecarState` without depending on it,
/// so the verdict below is decidable in a test with no filesystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubtitleTrackCache {
    Ready,
    /// Nothing cached yet, and this includes "nothing running": from the
    /// client's side both mean the same thing — keep asking — and collapsing
    /// them here is what keeps `warming` from having two meanings on the wire.
    Warming,
    /// Failed with a live memo, no such track, or a bitmap track.
    Unavailable,
}

/// The wire value for [`DeliveryView::subtitle_readiness`], or `None` when
/// there is nothing honest to say.
///
/// `None` for every mode but `Native`: burn-in subtitles are video and their
/// readiness is the producer's, already reported; `Overlay` is rendered from a
/// bitmap track by the overlay path, which has its own lifecycle; `Off` names
/// no track at all. Reporting on a track nobody selected is noise a client
/// would have to learn to ignore, and a field that is sometimes about
/// something else is worse than a field that is sometimes absent.
pub(crate) fn subtitle_readiness_value(
    selection: &SubtitleSelection,
    cache: Option<SubtitleTrackCache>,
) -> Option<String> {
    if !matches!(selection.mode, SubtitleMode::Native) {
        return None;
    }
    // `Native` without a track fails validation before it reaches here, so a
    // missing cache verdict means the caller could not resolve the track at
    // all — which is `unavailable`, not silence: the client asked for a
    // specific track and deserves to be told to stop waiting for it.
    Some(
        match cache {
            Some(SubtitleTrackCache::Ready) => "ready",
            Some(SubtitleTrackCache::Warming) => "warming",
            Some(SubtitleTrackCache::Unavailable) | None => "unavailable",
        }
        .to_owned(),
    )
}

impl DeliveryView {
    pub(crate) fn from_status(
        status: &HlsSessionInfo,
        request: &ControlRequestV1,
        owner_node_id: &str,
        owner_epoch: u64,
        media_origin_ms: i64,
        subtitle_readiness: Option<String>,
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
                producer_decision: info
                    .producer_control
                    .as_ref()
                    .and_then(|control| control.decision_reason)
                    .map(str::to_owned),
                hold_reason: info.hold_reason.map(|reason| {
                    match reason {
                        crate::transcode::AheadHoldReason::Demand => "demand",
                        crate::transcode::AheadHoldReason::Time => "time",
                        crate::transcode::AheadHoldReason::Bytes => "bytes",
                        crate::transcode::AheadHoldReason::Global => "global",
                    }
                    .to_owned()
                }),
                subtitle_readiness: subtitle_readiness.clone(),
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
                // VOD's failure is still a prose cause rather than a bounded
                // decision, so there is nothing honest to put here yet. It is
                // absent rather than guessed.
                producer_decision: None,
                hold_reason: info.producer_hold.map(str::to_owned),
                subtitle_readiness,
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
        Self::from_request(&recipe.request, delivered_height, dynamic_range)
    }

    /// The same view, from the request alone.
    ///
    /// A *candidate* recipe has no `RemoteStartRequest` — that carries the
    /// incarnation, the source stat and the playlist shape of a session that
    /// exists. Everything this reads is on the request itself, so a recipe
    /// that will never be built can have one too.
    ///
    /// `dynamic_range` is the delivered grade and belongs to a built session,
    /// so a candidate passes `None`. That is safe now and was not before:
    /// `decide_preparation` reads the grade axis off `GradeIntent` — the
    /// request's own answers — precisely because the delivered grade is the
    /// encoder's and a candidate has no encoder.
    pub(crate) fn from_request(
        request: &crate::transcode::SessionRequest,
        delivered_height: i64,
        dynamic_range: Option<String>,
    ) -> Self {
        let codec = match &request.kind {
            SessionKind::Copy { .. } => "source",
            SessionKind::Transcode { .. } => "server_selected",
        };
        Self {
            quality_auto: request.automatic,
            height: delivered_height,
            audio_track: request.audio_index,
            subtitle_burn: request.subtitle_burn,
            audio_offset_ms: request.audio_offset_ms,
            codec: codec.to_owned(),
            dynamic_range,
        }
    }
}

/// The recipe a client's selection asks for, from the one it is being served.
///
/// **An edit, not a construction.** Every field the selection does not name is
/// carried through unchanged, and that is the whole reason the candidate is a
/// `SessionRequest` rather than a `CreateSession`: a create body structurally
/// cannot express `convert_dolby_vision` — `into_request` hardcodes it false,
/// because a client cannot ask to be handed a Profile 7 → 8.1 conversion — so
/// a candidate round-tripped through one would differ from a *converting*
/// session on that field alone, and report a grade crossing on every exchange
/// for a viewer who changed nothing.
///
/// `height` is resolved by the caller through
/// [`crate::http::hls::resolve_height`], because it is the one part a
/// selection cannot answer for itself: it needs the store, the ladder ceiling
/// and the network prior. `source_height` is passed separately and is **not**
/// interchangeable with it — see the third ruling.
///
/// Four rulings are baked in here, and each is a decision rather than a
/// mapping:
///
/// * **The client's codec and dynamic-range *policies* are not the server's
///   answers.** `hdr10`, `preserve_dolby_vision` and `convert_dolby_vision`
///   come from the caps document through `review_client_plan`, which a
///   selection does not carry, so they are carried through untouched. A
///   selection change on those axes means *re-review*, which nothing does yet
///   — and nothing is lost today, because `decide_preparation` refuses both
///   axes anyway. It will matter the moment the capability is narrowed.
/// * **Only `SubtitleMode::Burn` is a burn.** Off, Native and Overlay are not
///   video replacements at all (plan §5.2). Mapping a track into
///   `subtitle_burn` for any of them would turn every native-subtitle change
///   into a whole new encode recipe.
/// * **A manual rung on a copy becomes a transcode, except at the source's own
///   height.** A copy has no rung — asking for one is asking for something a
///   copy cannot be. The source height is the exception because that *is* what
///   a copy delivers: asking for it is asking for what you already have. The
///   resulting `DeliveryMethod` crossing is then refused by the decision,
///   which is the point: the candidate says what was asked for, and the
///   decision says no.
///
///   The comparison is against `source_height`, **never** against the resolved
///   `height`. A 1080 ask on a 2160 source resolves to 1080 — it is already a
///   ladder rung, so the snap is the identity — and comparing the ask to the
///   result would find them equal and leave a copy copying a rung it cannot
///   serve.
/// * **Auto never changes the delivery method.** It is a request to let the
///   server choose, not a request to stop copying, and turning a direct play
///   into a transcode because the viewer selected Auto would be a downgrade
///   nobody asked for.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn candidate_request(
    current: &crate::transcode::SessionRequest,
    selection: &ClientSelection,
    height: i64,
    source_height: Option<i64>,
) -> crate::transcode::SessionRequest {
    let mut candidate = current.clone();
    candidate.automatic = matches!(selection.quality, QualitySelection::Auto);
    candidate.audio_index = selection.audio_track;
    candidate.audio_offset_ms = selection.audio_offset_ms;
    candidate.subtitle_burn = match selection.subtitle.mode {
        SubtitleMode::Burn => selection.subtitle.track,
        SubtitleMode::Off | SubtitleMode::Native | SubtitleMode::Overlay => None,
    };
    match (&current.kind, selection.quality) {
        // A transcode simply moves rung.
        (SessionKind::Transcode { .. }, _) => {
            candidate.kind = SessionKind::Transcode { height };
        }
        // Auto leaves a copy copying, and a source-height ask is a copy's own
        // delivery asked for by name. Both leave `kind` exactly as it was,
        // which is what carries `convert_dolby_vision` through.
        (SessionKind::Copy { .. }, QualitySelection::Auto) => {}
        (SessionKind::Copy { .. }, QualitySelection::Manual { height: asked })
            if Some(asked) == source_height => {}
        (SessionKind::Copy { .. }, QualitySelection::Manual { .. }) => {
            candidate.kind = SessionKind::Transcode { height };
        }
    }
    candidate
}

/// Which axis of the *delivered* selection a proposed transition crosses.
///
/// Named for what [`EffectiveSelection`] can actually see, which is less than
/// the recipe — see [`PreparationDecision::Unchanged`]. In particular
/// `DeliveryMethod` is **not** a codec: the field it reads takes exactly two
/// values, `source` and `server_selected`, so it distinguishes direct
/// play/remux from a transcode and nothing finer. H.264 against HEVC is
/// invisible here and always has been; today a real codec change drags
/// `dynamic_range` with it because the SDR rungs are H.264 and the HDR10 rung
/// is HEVC Main10, but that invariant lives in `plurx_core::playback` and is
/// not asserted by this module. An AV1 or HEVC-SDR rung would break it, which
/// is why `ResolutionOrBitrate` promises *same delivery method and same
/// delivered grade* rather than "same codec".
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PreparationAxis {
    /// Same delivery method, same delivered grade, different height — or a
    /// move between Auto and a pinned rung.
    ResolutionOrBitrate,
    AudioTrackOrOffset,
    /// A burned subtitle change, and only that. A native text track is not a
    /// video replacement at all (plan §5.2) and never reaches this type.
    SubtitleBurn,
    /// Direct play/remux against transcode.
    DeliveryMethod,
    DynamicRange,
}

#[cfg_attr(not(test), allow(dead_code))]
impl PreparationAxis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ResolutionOrBitrate => "resolution_or_bitrate",
            Self::AudioTrackOrOffset => "audio_track_or_offset",
            Self::SubtitleBurn => "subtitle_burn",
            Self::DeliveryMethod => "delivery_method",
            Self::DynamicRange => "dynamic_range",
        }
    }
}

/// Why a transition that crosses an axis is not being prepared.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FallbackReason {
    /// No retained `dual_player_preparation`. See [`decide_preparation`] on
    /// why this must be the *retained* capability rather than this exchange's.
    ClientCannotPrepare,
    /// The client can prepare, but not across this axis.
    AxisNotProven,
    /// More than one axis moves at once. Each is separately measured; a
    /// combination is measured by nothing.
    MultipleAxes,
    /// The client has not shown it can carry a second pipeline. M6 handoff §8:
    /// dual preparation doubles network demand and the constrained link is
    /// exactly the case M5.5 did not test.
    ThroughputUnproven,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FallbackReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ClientCannotPrepare => "client_cannot_prepare",
            Self::AxisNotProven => "axis_not_proven",
            Self::MultipleAxes => "multiple_axes",
            Self::ThroughputUnproven => "throughput_unproven",
        }
    }
}

/// What M6 does about a delivered-selection change while a stream is playing.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreparationDecision {
    /// The **delivered selection** is unchanged. Deliberately not "the recipe
    /// is unchanged": [`EffectiveSelection`] describes the output, not the
    /// request, and carries no `file_id`, `start_seconds`, `aac` or
    /// `convert_dolby_vision`. A next episode at the same height, a seek, an
    /// AAC re-encode toggle and a Profile 7 → 8.1 rewrite all compare equal
    /// here. **The caller owns those**; this answers only the question it can
    /// see.
    Unchanged,
    /// Stage a successor and commit it when the client says it is ready.
    Prepare { axis: PreparationAxis },
    /// Replace the stream the way the fleet does today, and **do not call it
    /// seamless**. Plan §5.2 draws that line and M5.5 gave it numbers: the
    /// worst observed fallback interruption is 2,246 ms, on Safari, and
    /// Apple's is unmeasured because Apple never exercised the fallback.
    ///
    /// `axis` is the **hardest** axis crossed, by [`PreparationAxis`]'s own
    /// ordering, so an operator grouping refusals by axis sees the constraint
    /// that actually bound rather than whichever field happened to move too.
    Fallback {
        axis: PreparationAxis,
        reason: FallbackReason,
    },
}

/// What the client has shown about the link it is on.
///
/// Present because M6's handoff §8 asks for it by name: *"treat a prepared
/// handoff on a contended link as unproven and gate it on the client's own
/// observed throughput rather than on the capability alone."* The whole spike
/// ran on a steady shaped 80 Mbit/s link, so the constrained case — the one
/// where doubling demand hurts — is measured by nothing.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreparationConditions {
    /// The client's own observed download rate, as it reported it.
    pub observed_download_bps: Option<u64>,
    /// What this session is currently delivering, as the server measured it.
    pub delivered_bps: Option<i64>,
}

impl PreparationConditions {
    /// Whether the link has shown room for a second pipeline.
    ///
    /// The rule is a floor, not a model: **observed throughput at least twice
    /// what this session is already delivering.** A prime runs a second
    /// pipeline beside the first, so twice the current rate is the least that
    /// could carry it; the successor's own rate is not knowable from
    /// [`EffectiveSelection`], which carries a height and no bitrate, so a
    /// tighter rule would be a guess wearing a number.
    ///
    /// **Either value missing is a refusal, not a pass.** A client that has
    /// not reported its throughput has not shown headroom, and M5.5 measured
    /// nothing about contended links — so the honest default is the fallback
    /// the fleet already takes. This is also the residual worth re-reading if
    /// prepared handoffs turn out never to fire: on a healthy link the ratio
    /// is comfortable, but a session whose `delivered_bps` is not yet measured
    /// refuses on that alone.
    fn have_headroom_for_a_second_pipeline(&self) -> bool {
        let (Some(observed), Some(delivered)) = (self.observed_download_bps, self.delivered_bps)
        else {
            return false;
        };
        let Ok(delivered) = u64::try_from(delivered) else {
            return false;
        };
        delivered > 0 && observed >= delivered.saturating_mul(2)
    }
}

/// The grade a recipe *asks for*, which is the only grade two recipes can be
/// compared on.
///
/// [`EffectiveSelection::dynamic_range`] carries the grade the encoder
/// actually built, and that is the right thing for a badge — `create`'s own
/// comment says so: *"the server refuses the HDR10 rung for a source or a rung
/// that cannot prove it, and the badge has to follow the encoder."* It is the
/// wrong thing for a *candidate*, because a recipe that will never be built
/// has no encoder and therefore no such grade.
///
/// Comparing an encoder's answer against a request would fail in one of two
/// ways, both bad: a candidate given no grade lets a real grade change
/// classify as resolution-only and be **prepared**, which is exactly what
/// `PREPARED_AXIS` exists to prevent; a candidate given the grade its body
/// asked for reads as a crossing on *every* exchange of a session whose HDR10
/// rung the encoder refused, so that viewer never gets a prepared handoff at
/// all.
///
/// So the grade axis is read off the request on both sides. Both are available
/// where the decision is made: the exchange holds the session's own
/// `RemoteStartRequest` and resolves the candidate's.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GradeIntent {
    /// The HDR10 rung was asked for. After the plan review, so this is the
    /// server's answer to the client's ask rather than the ask itself.
    pub hdr10: bool,
    /// Dolby Vision RPUs survive the bitstream filter.
    pub preserve_dolby_vision: bool,
    /// Profile 7 RPUs are rewritten to Profile 8.1 on the way through. Beside
    /// preserving rather than inside it because they answer different
    /// questions, and a viewer can see the difference between them.
    pub convert_dolby_vision: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl GradeIntent {
    pub(crate) fn from_request(request: &crate::transcode::SessionRequest) -> Self {
        let (preserve_dolby_vision, convert_dolby_vision) = match &request.kind {
            SessionKind::Copy {
                preserve_dolby_vision,
                convert_dolby_vision,
                ..
            } => (*preserve_dolby_vision, *convert_dolby_vision),
            // A transcode never carries RPUs through: the two questions only
            // arise for a copy, and answering them `false` for a transcode is
            // a statement about the output rather than a default.
            SessionKind::Transcode { .. } => (false, false),
        };
        Self {
            hdr10: request.hdr10,
            preserve_dolby_vision,
            convert_dolby_vision,
        }
    }
}

/// One side of a proposed transition: what is delivered, and what was asked
/// for.
///
/// Two halves because neither answers alone. The selection carries the height,
/// the delivery method and the audio and subtitle facts; the intent carries
/// the grade, which the selection can only report after an encoder has settled
/// it.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RecipeView<'a> {
    pub selection: &'a EffectiveSelection,
    pub grade: GradeIntent,
}

/// The only axis M6 prepares across, and the reason it is one rather than a
/// set.
///
/// M5.5 measured every platform **recipe- and device-dependent**, and in both
/// directions: Safari passes same-codec and fails codec/HDR, while the
/// tunneled Google TV passes codec/HDR 20/20 and fails same-codec 0/3 — the
/// *harder* case works and the easier one does not. So no single axis is safe
/// on every device that reports `true`, and the capability cannot say which,
/// because a bare boolean cannot express "yes for this recipe on this device".
///
/// The decision, made deliberately per M6's handoff §3 rather than
/// discovered: **honour the boolean and additionally restrict the server to
/// resolution/bitrate.**
///
/// * It is the only axis plan §5.2 expects to be transparent, so the only one
///   where "prepared" and "seamless" can be the same claim.
/// * It is the common case — a quality change on the same source.
/// * Both required Apple devices passed it 20/20.
///
/// **The residual, which is not small.** Apple passed *both* cases 20/20, so
/// on the only platform that will report `true` in the foreseeable future this
/// restriction discriminates nothing and its whole effect is to refuse
/// codec/grade transitions that were measured twice on two devices. And on the
/// device with the only hard failure it points the wrong way: the Google TV's
/// 0/3 was on *same-codec*, and M5.5's own leading explanation is two
/// identical tunneled pipelines contending for one decoder or audio track. If
/// that reading is right, restricting to same-method-same-grade selects that
/// precondition rather than avoiding it — and an
/// `ERROR_CODE_AUDIO_TRACK_WRITE_FAILED` is a fault in the shared audio path,
/// which is worse than the 2,246 ms this is trading against.
///
/// Android's platform-wide `false` is what holds that device back today. The
/// capability keyed by axis **and device class** is therefore not a nice-to-
/// have for unlocking Android's two phones; it is what has to exist before
/// this constant is safe for the television class. Recorded here rather than
/// argued around, because this constant is the entire server-side expression
/// of the restriction and whoever narrows the capability will read it.
#[cfg_attr(not(test), allow(dead_code))]
const PREPARED_AXIS: PreparationAxis = PreparationAxis::ResolutionOrBitrate;

/// Decide, from the delivered selection and a candidate one, whether M6
/// prepares.
///
/// **Reads the capability. Never re-derives it.** M5.5 exists because three
/// capability literals had been written by assumption, and a server that
/// inferred "Apple can prepare" from the platform enum would be the fourth.
/// When Apple's literal flips to `true` this starts preparing on Apple with no
/// server change, which is handoff §1's whole point.
///
/// `retained` is the capability document the session is **holding**, not this
/// exchange's. `ControlRequestV1::validate` requires `capabilities` only on
/// sequence 1 and permits every later exchange to omit them, so absent-on-this-
/// exchange means "already told you" and reading it as `false` would refuse
/// every transition a capable client ever makes. `None` here must therefore
/// mean *this session has never been told*, and the caller owns the retention.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decide_preparation(
    delivered: RecipeView<'_>,
    candidate: RecipeView<'_>,
    retained: Option<&DynamicCapabilities>,
    conditions: PreparationConditions,
) -> PreparationDecision {
    let (delivered_grade, candidate_grade) = (delivered.grade, candidate.grade);
    let (delivered, candidate) = (delivered.selection, candidate.selection);
    let mut crossed: Option<PreparationAxis> = None;
    let mut multiple = false;
    // The hardest axis wins, by the enum's own ordering rather than by the
    // order these happen to be written. Both plan §5.2 and roadmap §3.3 rank
    // resolution/bitrate most transparent, then audio and burned subtitles,
    // then the delivery method and the grade — so `max` is the ranking, and
    // regrouping these statements cannot silently change an operator metric.
    let mut cross = |axis: PreparationAxis| {
        multiple |= crossed.is_some();
        crossed = Some(crossed.map_or(axis, |held: PreparationAxis| held.max(axis)));
    };

    if delivered.codec != candidate.codec {
        cross(PreparationAxis::DeliveryMethod);
    }
    // Read off the request on both sides, never off `dynamic_range` — see
    // `GradeIntent`. The delivered selection's grade is the encoder's answer,
    // and a candidate that will never be built has no encoder to answer for
    // it, so comparing the two would be comparing unlike things.
    if delivered_grade != candidate_grade {
        cross(PreparationAxis::DynamicRange);
    }
    if delivered.audio_track != candidate.audio_track
        || delivered.audio_offset_ms != candidate.audio_offset_ms
    {
        cross(PreparationAxis::AudioTrackOrOffset);
    }
    if delivered.subtitle_burn != candidate.subtitle_burn {
        cross(PreparationAxis::SubtitleBurn);
    }
    if delivered.height != candidate.height || delivered.quality_auto != candidate.quality_auto {
        cross(PreparationAxis::ResolutionOrBitrate);
    }

    let Some(axis) = crossed else {
        return PreparationDecision::Unchanged;
    };
    if multiple {
        // Each axis is separately measured and a combination is measured by
        // nothing. M5.5 ran two cases, not their product, and the Google TV's
        // inversion is exactly the evidence that axes do not compose the way
        // reasoning would predict.
        //
        // Ranked above the capability deliberately: this is the fact that
        // would still be true after a coordinated client release flipped the
        // literal, so the metric stays stable across that release. The cost is
        // that a fleet whose clients all report `false` books its multi-axis
        // transitions here rather than under `client_cannot_prepare`, which
        // understates how much of the fallback volume is the literals'.
        return PreparationDecision::Fallback {
            axis,
            reason: FallbackReason::MultipleAxes,
        };
    }
    if !retained.is_some_and(|caps| caps.dual_player_preparation) {
        return PreparationDecision::Fallback {
            axis,
            reason: FallbackReason::ClientCannotPrepare,
        };
    }
    if axis != PREPARED_AXIS {
        return PreparationDecision::Fallback {
            axis,
            reason: FallbackReason::AxisNotProven,
        };
    }
    if !conditions.have_headroom_for_a_second_pipeline() {
        return PreparationDecision::Fallback {
            axis,
            reason: FallbackReason::ThroughputUnproven,
        };
    }
    PreparationDecision::Prepare { axis }
}

pub(crate) fn target_duration_ms(recipe: &crate::media_sessions::RemoteStartRequest) -> i64 {
    let seconds = match &recipe.request.kind {
        SessionKind::Copy { .. } => plurx_core::transcode::COPY_SEGMENT_MAX_SECS,
        SessionKind::Transcode { .. } => plurx_core::transcode::SEGMENT_SECONDS,
    };
    i64::from(seconds) * 1_000
}

/// Why production is deliberately not advancing.
///
/// One vocabulary, shared with `DeliveryView::hold_reason` rather than
/// invented alongside it: the first four are the rolling producer's
/// `AheadHoldReason`, the last three the VOD producer's hold strings. The
/// action carries the same value the delivery view reports, so an operator
/// reading either sees the same word.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HoldReason {
    Demand,
    Time,
    Bytes,
    Global,
    Ahead,
    WorkingSet,
    NoRoom,
}

impl HoldReason {
    pub(crate) fn from_delivery(reason: &str) -> Option<Self> {
        Some(match reason {
            "demand" => Self::Demand,
            "time" => Self::Time,
            "bytes" => Self::Bytes,
            "global" => Self::Global,
            "ahead" => Self::Ahead,
            "working_set" => Self::WorkingSet,
            "no_room" => Self::NoRoom,
            _ => return None,
        })
    }

    pub(crate) fn as_delivery_str(self) -> &'static str {
        match self {
            Self::Demand => "demand",
            Self::Time => "time",
            Self::Bytes => "bytes",
            Self::Global => "global",
            Self::Ahead => "ahead",
            Self::WorkingSet => "working_set",
            Self::NoRoom => "no_room",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ControlAction {
    None,
    /// Production is deliberately not advancing, and this is not a failure.
    ///
    /// Without it a client cannot tell a server that has stopped producing
    /// because the client's own runway is already long from a server that has
    /// stopped producing because it is broken. Every client currently guesses,
    /// and guesses toward recovery: it tears the player down and reopens the
    /// session, which is the single most expensive possible response to a
    /// server working exactly as designed.
    ///
    /// It is advisory and carries no `action_id`. There is no transaction to
    /// fence, nothing to acknowledge, and a replay recomputes it from current
    /// delivery rather than replaying a stale one — a hold that has since
    /// lifted must not be replayed as though it were still in force.
    Hold {
        reason: HoldReason,
    },
    /// Production stopped for a reason that trying again cannot change.
    ///
    /// This is the only thing besides buffer exhaustion that authorises a
    /// client to tear down a player it is still holding buffer for. A 404 or
    /// 410 does not: those say the route has no session to answer for, which
    /// a client survives by continuing to consume what it already has.
    ///
    /// The `code` is the producer decision itself rather than a second
    /// vocabulary invented for the wire, so an operator reading the action and
    /// the delivery view sees one word for one fact.
    Terminal {
        code: ProducerDecisionReason,
        message: String,
    },
    /// Production stopped for a reason that may not recur.
    ///
    /// Twelve of the fourteen producer decisions are timing, process or
    /// executor facts. The client should try again on the server's own
    /// cadence rather than deciding for itself how hard to retry, which is
    /// what every client does today.
    RetryResource {
        after_ms: u32,
        reason: ProducerDecisionReason,
    },
}

impl ControlAction {
    /// The one action a passive client is always safe to receive.
    pub(crate) fn is_passive(&self) -> bool {
        matches!(self, Self::None)
    }

    /// The vocabulary name a client must declare to be sent this action.
    pub(crate) fn vocabulary_name(&self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Hold { .. } => Some(HOLD_ACTION),
            Self::Terminal { .. } => Some(TERMINAL_ACTION),
            Self::RetryResource { .. } => Some(RETRY_RESOURCE_ACTION),
        }
    }
}

/// Resolve the action this response carries.
///
/// Two sources, deliberately ordered. An action the actor decided is a
/// transaction: it has identity, it is fenced by `prior_action`, and it
/// replays exactly. It always wins. Only when the actor has nothing to say
/// does the advisory hold get derived from the delivery the response is
/// already carrying, so the action and `delivery.hold_reason` can never
/// disagree — they are the same fact read once.
/// Which metric slots one resolved response occupies.
///
/// Separated from the counters so the classification can be tested. The
/// interesting field is `suppressed`: production was held, and the client
/// could not be told because it had not declared the action. That number is
/// the size of the problem the vocabulary rollout exists to close, and it is
/// the one an operator should watch fall to zero before recovery authority
/// moves off the clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActionMetrics {
    pub action: ActionKind,
    pub hold_reason: Option<HoldReason>,
    pub suppressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    None = 0,
    Hold = 1,
    Terminal = 2,
    RetryResource = 3,
}

/// What this response's action says about the exchange, for metrics only.
pub(crate) fn action_metrics(
    action: &ControlAction,
    delivery: &DeliveryView,
    request: &ControlRequestV1,
) -> ActionMetrics {
    match action {
        ControlAction::Hold { reason } => ActionMetrics {
            action: ActionKind::Hold,
            hold_reason: Some(*reason),
            suppressed: false,
        },
        ControlAction::Terminal { .. } => ActionMetrics {
            action: ActionKind::Terminal,
            hold_reason: None,
            suppressed: false,
        },
        ControlAction::RetryResource { .. } => ActionMetrics {
            action: ActionKind::RetryResource,
            hold_reason: None,
            suppressed: false,
        },
        ControlAction::None => ActionMetrics {
            action: ActionKind::None,
            hold_reason: None,
            // Only a hold this server could actually have named counts as
            // suppressed. A reason it does not recognise was never a candidate
            // instruction, so counting it here would inflate the gap with
            // responses no vocabulary rollout would change.
            // A producer decision the server could have named outranks a
            // hold, so it is the thing withheld when the client is passive.
            suppressed: match delivery
                .producer_decision
                .as_deref()
                .and_then(ProducerDecisionReason::from_status)
            {
                Some(decision) => !request.accepts(if decision.is_permanent() {
                    TERMINAL_ACTION
                } else {
                    RETRY_RESOURCE_ACTION
                }),
                None => {
                    !request.accepts_hold()
                        && delivery
                            .hold_reason
                            .as_deref()
                            .is_some_and(|reason| HoldReason::from_delivery(reason).is_some())
                }
            },
        },
    }
}

pub(crate) fn resolve_action(
    decided: &ControlAction,
    delivery: &DeliveryView,
    request: &ControlRequestV1,
) -> ControlAction {
    if !decided.is_passive() {
        return decided.clone();
    }
    // Ranked, and the order is the point. A verdict that trying again cannot
    // help outranks one that says wait, which outranks one that says this
    // pause is deliberate. A client that received `hold` when the truth was
    // `terminal` would sit through a film that was never going to play.
    if let Some(decision) = delivery
        .producer_decision
        .as_deref()
        .and_then(ProducerDecisionReason::from_status)
    {
        let proposed = if decision.is_permanent() {
            ControlAction::Terminal {
                code: decision,
                message: terminal_message(decision),
            }
        } else {
            ControlAction::RetryResource {
                // The server's own exchange cadence. A client should not be
                // inventing a retry interval when the server already publishes
                // the rate at which it wants to be asked.
                after_ms: NEXT_EXCHANGE_MS,
                reason: decision,
            }
        };
        if proposed
            .vocabulary_name()
            .is_some_and(|name| request.accepts(name))
        {
            return proposed;
        }
        // The client cannot apply this one. Fall through rather than
        // substituting a hold: a producer that has stopped is not holding, and
        // saying so would be a comfortable lie.
        return ControlAction::None;
    }
    if !request.accepts_hold() {
        return ControlAction::None;
    }
    delivery
        .hold_reason
        .as_deref()
        .and_then(HoldReason::from_delivery)
        .map_or(ControlAction::None, |reason| ControlAction::Hold { reason })
}

/// A bounded explanation for a terminal verdict.
///
/// Deliberately not the producer's own prose: that text carries file paths and
/// ffmpeg diagnostics, and this travels to three clients and their logs.
fn terminal_message(decision: ProducerDecisionReason) -> String {
    let text = match decision {
        ProducerDecisionReason::Unsupported => {
            "this source cannot be carried by this delivery pipeline"
        }
        ProducerDecisionReason::InvalidConfiguration => {
            "the requested delivery configuration is not a legal one"
        }
        // Only permanent decisions reach here today; a future permanent
        // variant without its own sentence gets the reason and no invention.
        other => other.status(),
    };
    debug_assert!(text.len() <= MAX_TERMINAL_MESSAGE_BYTES);
    text.to_owned()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RollingTerminalRequestError {
    AdmissionDeadline,
    ControlUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RollingCommandAdmissionError {
    Deadline,
    ControlUnavailable,
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
    if route.publication_ready_at_ms != 0 {
        return Err(ControlStateError::OwnerTransition);
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
    /// This accepted exchange's selection differs from the last one's.
    ///
    /// M6's gate, answered by whichever engine served the exchange —
    /// `ControlState::observe_selection` is the one implementation, and it is
    /// on `ControlState` precisely so both can answer it. `false` on a replay
    /// and on the first accepted exchange.
    pub selection_changed: bool,
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
            producer_decision: None,
            hold_reason: None,
            subtitle_readiness: None,
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
/// fetches or from whichever compatibility heuristic happened to fire.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettledTarget {
    pub sequence: u64,
    pub anchor_ms: i64,
}

const SETTLED_TARGET_SLACK_MS: i64 = 1_000;

impl SettledTarget {
    pub(crate) fn supersedes(&self, sequence: u64, anchor_ms: i64) -> bool {
        self.sequence > sequence && (self.anchor_ms - anchor_ms).abs() > SETTLED_TARGET_SLACK_MS
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
    /// The selection the last accepted exchange carried.
    ///
    /// Here rather than on the rolling actor because **both delivery engines
    /// hold a `ControlState` and only one of them has an actor**. A session is
    /// served by the VOD engine whenever `try_vod_session` admits it, which it
    /// does for every `Presentation::Vod` request — and `into_request` sets
    /// that for every create, since the growing live presentation was removed.
    /// So a gate that lived on the rolling actor alone would see the minority
    /// of exchanges and report a confidently undercounted picture, which is
    /// worse than reporting none.
    ///
    /// Cleared with the client identity on an owner-epoch advance: the
    /// sequence space restarts, and a selection from before the advance is not
    /// something this client has since departed from.
    last_selection: Option<ClientSelection>,
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
            last_selection: None,
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
            self.last_selection = None;
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

    /// Take this exchange's selection and say whether it moved.
    ///
    /// M6's gate. Building a candidate recipe costs two store reads — the
    /// source file and the network prior — the exchange runs about once a
    /// second per client under an absolute deadline, and the answer is almost
    /// always *nothing changed*.
    ///
    /// **Against the previous selection, never against what is delivered.** A
    /// client's ask and the height it gets are not the same number: an
    /// explicit rung snaps onto the ladder, so a client asking 1079 is served
    /// 1080 and goes on asking 1079. A gate comparing the two would read
    /// *changed* on every exchange for the rest of that session, spending the
    /// reads it exists to save on a candidate identical to what is playing.
    ///
    /// Called by both delivery engines on acceptance, and only on acceptance:
    /// a replay is the same exchange arriving twice and changed the selection
    /// the first time or not at all. `false` on the first accepted exchange —
    /// a session just created from an intent has not since departed from it.
    pub(crate) fn observe_selection(&mut self, selection: &ClientSelection) -> bool {
        let moved = self
            .last_selection
            .as_ref()
            .is_some_and(|previous| previous != selection);
        self.last_selection = Some(selection.clone());
        moved
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
/// How long an actor-authorized SIGSTOP/SIGCONT may go unacknowledged.
///
/// A lifecycle bound on one physical transaction, not a progress verdict: it
/// fires only when a signal the actor itself issued never came back. Ten
/// seconds is the same order as the progress budget, so a wedged signal is
/// noticed on roughly the timescale a wedged producer is.
const PRODUCER_FLOW_ACTION_BUDGET: Duration = Duration::from_secs(10);

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
    /// The destination the client actually wants, and the exchange that
    /// settled it. Rides the existing snapshot rather than a query of its own:
    /// every caller that needs to ask "is this work still wanted" already has
    /// a reason to hold the lease.
    pub settled_target: Option<SettledTarget>,
    pub delivery: RollingDeliverySnapshot,
    pub retired: bool,
    /// Immutable cause of the actor's first terminal transition. `None` is
    /// live; later end/fence events cannot relabel the winning cause.
    pub terminal: Option<RollingTerminalCause>,
    /// True only when the playback deadline, rather than an explicit
    /// lifecycle fence, performed the actor's terminal transition.
    pub expiration_claimed: bool,
    /// Bounded, actor-owned rolling producer truth. Compatibility producers
    /// remain observation-only; opted-in transcode and copy producers expose
    /// their exact decision, proposal, and completion evidence here.
    pub producer_control: RollingProducerOperationalSnapshot,
}

/// The only producer-failure reasons that may be retained by the M4 actor.
/// Legacy rolling sessions remain action-passive; only an explicit
/// actor-managed producer constructor permits production decisions before or
/// after first-media publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum ProducerDecisionReason {
    StartupDeadline,
    ProgressDeadline,
    ExitClassificationDeadline,
    ProcessExit,
    PartialSuccessExit,
    Unsupported,
    InvalidConfiguration,
    ReaderFailed,
    FlowStopFailed,
    FlowResumeFailed,
    FlowStopDeadline,
    FlowResumeDeadline,
    InstallDeadline,
    ExecutorLost,
}

impl ProducerDecisionReason {
    fn status(self) -> &'static str {
        match self {
            Self::StartupDeadline => "startup_deadline",
            Self::ProgressDeadline => "progress_deadline",
            Self::ExitClassificationDeadline => "exit_classification_deadline",
            Self::ProcessExit => "process_exit",
            Self::PartialSuccessExit => "partial_success_exit",
            Self::Unsupported => "unsupported",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::ReaderFailed => "reader_failed",
            Self::FlowStopFailed => "flow_stop_failed",
            Self::FlowResumeFailed => "flow_resume_failed",
            Self::FlowStopDeadline => "flow_stop_deadline",
            Self::FlowResumeDeadline => "flow_resume_deadline",
            Self::InstallDeadline => "install_deadline",
            Self::ExecutorLost => "executor_lost",
        }
    }

    /// Whether retrying this source, unchanged, can ever succeed.
    ///
    /// Twelve of these fourteen reasons are timing, process, or executor
    /// facts: the same file on the same pipeline may well work on the next
    /// attempt. Two are verdicts about the source itself — this container
    /// cannot be carried by this pipeline, or the recipe that was asked for is
    /// not a legal one — and no amount of retrying changes either.
    ///
    /// The distinction is the whole reason a client cannot decide for itself.
    /// `producer_state` flattens all fourteen to the word `failed`, so a
    /// client seeing a failure has no way to tell "try again" from "this will
    /// never work", and every client currently guesses toward retry: it
    /// reopens the session, gets the same verdict, and reopens again.
    pub(crate) fn is_permanent(self) -> bool {
        matches!(self, Self::Unsupported | Self::InvalidConfiguration)
    }

    /// The bounded vocabulary, in the order the wire and metrics use.
    pub(crate) const ALL: [Self; 14] = [
        Self::StartupDeadline,
        Self::ProgressDeadline,
        Self::ExitClassificationDeadline,
        Self::ProcessExit,
        Self::PartialSuccessExit,
        Self::Unsupported,
        Self::InvalidConfiguration,
        Self::ReaderFailed,
        Self::FlowStopFailed,
        Self::FlowResumeFailed,
        Self::FlowStopDeadline,
        Self::FlowResumeDeadline,
        Self::InstallDeadline,
        Self::ExecutorLost,
    ];

    pub(crate) fn from_status(status: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.status() == status)
    }
}

/// Cleanup is immutable decision evidence. It does not authorize deletion or
/// termination in this behavior-neutral transport slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum CleanupPolicy {
    RetainPublished,
    DiscardPrepublication,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ProducerFailureCleanupKind {
    ProducerFailureCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ProducerFailureCleanup {
    pub kind: ProducerFailureCleanupKind,
    pub cleanup_policy: CleanupPolicy,
}

/// The executor receives an opaque identity plus a fingerprint. Concrete
/// command inputs remain outside this actor-owned transport so this type can
/// be cloned for redelivery without permitting mutation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ValidatedRetryRecipe {
    pub identity: String,
    pub fingerprint: String,
    pub presentation_contract_fingerprint: String,
    pub startup_kind: ProducerStartupKind,
    /// The actor-owned exit contract for the successor attempt. This is
    /// frozen into the validated recipe so retry installation cannot silently
    /// inherit the failed attempt's classifier.
    pub exit_classifier: ProducerExitClassifier,
}

impl ValidatedRetryRecipe {
    pub(crate) fn new(
        identity: String,
        fingerprint: String,
        presentation_contract_fingerprint: String,
        startup_kind: ProducerStartupKind,
    ) -> Self {
        Self {
            identity,
            fingerprint,
            presentation_contract_fingerprint,
            startup_kind,
            exit_classifier: ProducerExitClassifier::Immediate,
        }
    }

    pub(crate) fn with_exit_classifier(mut self, exit_classifier: ProducerExitClassifier) -> Self {
        self.exit_classifier = exit_classifier;
        self
    }

    #[cfg(test)]
    fn for_test(identity: &str, fingerprint: &str) -> Self {
        Self {
            identity: identity.to_owned(),
            fingerprint: fingerprint.to_owned(),
            presentation_contract_fingerprint: "test-presentation-contract".to_owned(),
            startup_kind: ProducerStartupKind::Software,
            exit_classifier: ProducerExitClassifier::Immediate,
        }
    }
}

/// Exact-attempt exit facts are interpreted according to the producer that
/// was actually installed, rather than according to the session's original
/// recipe. Ordinary transcodes classify directly from the process exit. A
/// copy producer must rendezvous that exit with the reader's typed result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProducerExitClassifier {
    Immediate,
    CopyReader,
}

/// Typed completion emitted by the copy reader. These facts are deliberately
/// narrower than process exit: only `Completed` needs the process half and
/// completion probe, while the other variants are decisive failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyProducerExitClassification {
    Unsupported,
    InvalidConfiguration,
    ReaderFailed,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProducerStartupKind {
    Hardware,
    Software,
    Copy,
}

impl ProducerStartupKind {
    pub(crate) fn startup_budget(self) -> Duration {
        match self {
            Self::Hardware => PREPUBLICATION_HARDWARE_STARTUP_BUDGET,
            Self::Software => PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
            Self::Copy => PREPUBLICATION_COPY_STARTUP_BUDGET,
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Copy => "copy",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerRetryEligibility {
    AnyPrepublicationFailure,
    UnsupportedOnly,
    Never,
}

impl ProducerRetryEligibility {
    fn allows(self, reason: ProducerDecisionReason) -> bool {
        match self {
            Self::AnyPrepublicationFailure => true,
            Self::UnsupportedOnly => reason == ProducerDecisionReason::Unsupported,
            Self::Never => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InitialProducerPolicy {
    pub startup_kind: ProducerStartupKind,
    pub progress_budget: Duration,
    pub retry_recipe: Option<ValidatedRetryRecipe>,
    pub presentation_contract_fingerprint: String,
    exit_classifier: ProducerExitClassifier,
    retry_eligibility: ProducerRetryEligibility,
    expected_remaining_ms: Option<i64>,
    completion_tolerance_ms: i64,
}

impl InitialProducerPolicy {
    pub(crate) fn hardware(
        presentation_contract_fingerprint: String,
        progress_budget: Duration,
        retry_recipe: ValidatedRetryRecipe,
    ) -> Self {
        Self {
            startup_kind: ProducerStartupKind::Hardware,
            progress_budget,
            retry_recipe: Some(retry_recipe),
            presentation_contract_fingerprint,
            exit_classifier: ProducerExitClassifier::Immediate,
            retry_eligibility: ProducerRetryEligibility::AnyPrepublicationFailure,
            expected_remaining_ms: None,
            completion_tolerance_ms: 10_000,
        }
    }

    pub(crate) fn software(
        presentation_contract_fingerprint: String,
        progress_budget: Duration,
    ) -> Self {
        Self {
            startup_kind: ProducerStartupKind::Software,
            progress_budget,
            retry_recipe: None,
            presentation_contract_fingerprint,
            exit_classifier: ProducerExitClassifier::Immediate,
            retry_eligibility: ProducerRetryEligibility::Never,
            expected_remaining_ms: None,
            completion_tolerance_ms: 10_000,
        }
    }

    /// Copy startup is actor-managed. When the caller freezes a fallback
    /// recipe, it is eligible only when the reader proves the input
    /// unsupported; no recipe means every failure is terminal. Runtime or
    /// configuration failures never become a generic second-attempt surface.
    pub(crate) fn copy(
        presentation_contract_fingerprint: String,
        progress_budget: Duration,
        retry_recipe: Option<ValidatedRetryRecipe>,
    ) -> Self {
        let retry_eligibility = if retry_recipe.is_some() {
            ProducerRetryEligibility::UnsupportedOnly
        } else {
            ProducerRetryEligibility::Never
        };
        Self {
            startup_kind: ProducerStartupKind::Copy,
            progress_budget,
            retry_recipe,
            presentation_contract_fingerprint,
            exit_classifier: ProducerExitClassifier::CopyReader,
            retry_eligibility,
            expected_remaining_ms: None,
            completion_tolerance_ms: 10_000,
        }
    }

    /// Direct and takeover copy attempts have no independent reader verdict;
    /// they retain the copy startup budget but classify their process exit
    /// immediately and cannot retry.
    pub(crate) fn copy_immediate(
        presentation_contract_fingerprint: String,
        progress_budget: Duration,
    ) -> Self {
        Self {
            startup_kind: ProducerStartupKind::Copy,
            progress_budget,
            retry_recipe: None,
            presentation_contract_fingerprint,
            exit_classifier: ProducerExitClassifier::Immediate,
            retry_eligibility: ProducerRetryEligibility::Never,
            expected_remaining_ms: None,
            completion_tolerance_ms: 10_000,
        }
    }

    /// Freeze the exact natural-exit completion policy beside attempt
    /// admission. The classifier may report playlist facts, but it cannot
    /// choose a more permissive expected duration or tolerance after exit.
    pub(crate) fn with_completion_expectation(
        mut self,
        expected_remaining_ms: Option<i64>,
        completion_tolerance_ms: i64,
    ) -> Self {
        self.expected_remaining_ms = expected_remaining_ms;
        self.completion_tolerance_ms = completion_tolerance_ms;
        self
    }

    fn validate(&self) -> Result<(), ProducerAttemptRejection> {
        const MAX_FINGERPRINT_BYTES: usize = 256;
        const MAX_RECIPE_IDENTITY_BYTES: usize = 256;
        const MAX_PROGRESS_BUDGET: Duration = Duration::from_secs(5 * 60);

        if self.presentation_contract_fingerprint.is_empty()
            || self.presentation_contract_fingerprint.len() > MAX_FINGERPRINT_BYTES
            || self.progress_budget.is_zero()
            || self.progress_budget > MAX_PROGRESS_BUDGET
            || self
                .expected_remaining_ms
                .is_some_and(|remaining| !(0..=MAX_MEDIA_MILLIS).contains(&remaining))
            || !(1..=120_000).contains(&self.completion_tolerance_ms)
            || !matches!(
                (
                    self.startup_kind,
                    self.exit_classifier,
                    self.retry_eligibility,
                    self.retry_recipe.is_some(),
                ),
                (
                    ProducerStartupKind::Hardware,
                    ProducerExitClassifier::Immediate,
                    ProducerRetryEligibility::AnyPrepublicationFailure,
                    true,
                ) | (
                    ProducerStartupKind::Software,
                    ProducerExitClassifier::Immediate,
                    ProducerRetryEligibility::Never,
                    false,
                ) | (
                    ProducerStartupKind::Copy,
                    ProducerExitClassifier::CopyReader,
                    ProducerRetryEligibility::UnsupportedOnly,
                    true,
                ) | (
                    ProducerStartupKind::Copy,
                    ProducerExitClassifier::CopyReader,
                    ProducerRetryEligibility::Never,
                    false,
                ) | (
                    ProducerStartupKind::Copy,
                    ProducerExitClassifier::Immediate,
                    ProducerRetryEligibility::Never,
                    false,
                )
            )
        {
            return Err(ProducerAttemptRejection::InvalidPolicy);
        }
        if let Some(recipe) = &self.retry_recipe {
            if recipe.identity.is_empty()
                || recipe.identity.len() > MAX_RECIPE_IDENTITY_BYTES
                || recipe.fingerprint.is_empty()
                || recipe.fingerprint.len() > MAX_FINGERPRINT_BYTES
                || recipe.presentation_contract_fingerprint
                    != self.presentation_contract_fingerprint
            {
                return Err(ProducerAttemptRejection::InvalidPolicy);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ActionProposal {
    pub proposal_id: String,
    pub kind: &'static str,
    pub reason: ProducerDecisionReason,
    pub source: &'static str,
    pub severity: &'static str,
}

/// Performs a preparation's durable half, gated by the actor that owns the
/// slot.
///
/// The actor decides and this executes; the store's own CAS decides the
/// outcome. Three separate authorities, in that order, and none of them may
/// be skipped:
///
/// * the actor answers whether this successor is still wanted,
/// * the store answers whether the pointer still names the recorded
///   predecessor,
/// * the actor is told which happened and frees the slot.
///
/// The gate and the store call cannot be one transaction, so the actor is
/// asked as late as possible and told as soon as an answer exists. The window
/// between them is exactly why `settle` re-checks identity rather than
/// trusting the gate's earlier `true`.
// Exercised by tests against a real `SqliteStore` — the three phases M6's
// acceptance names are testable without hardware, and they are the ones worth
// pinning before a caller exists. Still no production caller until the HTTP
// layer stages a successor, hence the non-test allow rather than an invented
// one: a caller written to satisfy a lint is how a mechanism ends up with a
// shape nobody chose.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct PreparationExecutor {
    store: std::sync::Arc<dyn plurx_core::store::Store>,
    control: RollingControlHandle,
    user_id: i64,
    playback_id: String,
}

#[cfg_attr(not(test), allow(dead_code))]
impl PreparationExecutor {
    pub(crate) fn new(
        store: std::sync::Arc<dyn plurx_core::store::Store>,
        control: RollingControlHandle,
        user_id: i64,
        playback_id: String,
    ) -> Self {
        Self {
            store,
            control,
            user_id,
            playback_id,
        }
    }

    /// Stage a successor: durable row first, then the slot.
    ///
    /// That order is deliberate. If the row is created and the slot refuses,
    /// this aborts the row it just made — a staged row nobody owns is reaped
    /// only by the maintenance backstop, and until then it counts against the
    /// user's admission cap for a successor that will never commit. The
    /// reverse order would instead leave the actor believing in a successor
    /// the store rejected, which is worse: a commit could then name it.
    pub(crate) async fn stage(
        &self,
        preparation: &plurx_core::domain::MediaSessionPreparation,
    ) -> Result<bool, plurx_core::error::StoreError> {
        if self
            .store
            .prepare_media_session(preparation)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        if self
            .control
            .stage_preparation(
                preparation.incarnation_id.clone(),
                preparation.expected_predecessor_incarnation_id.clone(),
            )
            .await
        {
            return Ok(true);
        }
        let _ = self
            .store
            .abort_media_session_preparation(
                self.user_id,
                &self.playback_id,
                &preparation.incarnation_id,
                preparation.now_ms,
            )
            .await;
        Ok(false)
    }

    /// Commit a staged successor, or abort it if the pointer moved.
    ///
    /// `Ok(false)` covers both refusals and means the same thing to a caller:
    /// this successor is not becoming current. The distinction that matters is
    /// invisible from here and deliberate — a lost CAS aborts the staged
    /// generation and **never reaps the newer player generation** that won,
    /// which is why the predecessor is recorded at preparation time rather
    /// than read fresh at commit.
    pub(crate) async fn commit(
        &self,
        staged_incarnation_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<bool, plurx_core::error::StoreError> {
        if !self
            .control
            .may_commit_preparation(staged_incarnation_id)
            .await
        {
            return Ok(false);
        }
        let committed = self
            .store
            .commit_media_session_preparation(
                self.user_id,
                &self.playback_id,
                staged_incarnation_id,
                now_ms,
                lease_expires_at_ms,
            )
            .await?
            .is_some();
        if !committed {
            // The pointer moved. On both backends the store's own lost-CAS
            // branch has already run `abort_staged_generation` before
            // returning `Ok(None)`, so this call is normally a guaranteed
            // no-op — every write inside that helper is gated on the ledger
            // row it has just deleted. It is kept, and kept idempotent, for
            // the branch the store documents as unreachable (a committed
            // projection that cannot be read back rolls its transaction
            // *back*, restoring the ledger row) and so a backend that ever
            // stopped tearing down on its own does not silently leave a row
            // the actor has already forgotten. Do not read it as the
            // teardown: the store's is.
            let _ = self
                .store
                .abort_media_session_preparation(
                    self.user_id,
                    &self.playback_id,
                    staged_incarnation_id,
                    now_ms,
                )
                .await;
        }
        self.control
            .settle_preparation(staged_incarnation_id, committed)
            .await;
        Ok(committed)
    }

    /// Discard a staged successor and leave the current stream authoritative.
    ///
    /// The slot is settled whatever the store says. A row that is already
    /// ended, or that was never staged, leaves nothing for the actor to hold —
    /// and an owner retrying an abort after a crash must read back the same
    /// outcome rather than a spurious loss.
    pub(crate) async fn abort(
        &self,
        staged_incarnation_id: &str,
        now_ms: i64,
    ) -> Result<(), plurx_core::error::StoreError> {
        self.store
            .abort_media_session_preparation(
                self.user_id,
                &self.playback_id,
                staged_incarnation_id,
                now_ms,
            )
            .await?;
        self.control
            .settle_preparation(staged_incarnation_id, false)
            .await;
        Ok(())
    }
}

/// The state of this playback's single preparation slot.
///
/// M6 stages a successor while the current stream still plays and commits only
/// once the successor says it is ready. The store half of that already exists
/// and is contract-tested on three voters — `prepare_media_session`,
/// `rejoin_media_session_preparation`, `commit_media_session_preparation`,
/// `abort_media_session_preparation` — and had no caller. This is the actor's
/// half: the slot's lifecycle, and who is allowed to move it.
///
/// It lives in the actor rather than in the HTTP handler because the actor is
/// already the ordering authority — it accepts control sequences, fences
/// producer attempts and owns the terminal transition — and a preparation that
/// could be committed from outside that ordering would be a second authority
/// over the same playback. The actor decides; an executor performs the durable
/// call; the outcome is reported back. That is the shape producer decisions
/// already use, deliberately: M7's plan warns against building a third
/// mechanism, and a preparation lifecycle invented beside the decision
/// lifecycle would be exactly that.
/// `abort_preparation` has a production caller in `terminate`; the rest are
/// the actor's half of a lifecycle whose executor half is M6's next slice.
/// Marked the way `ProducerDecision` is, and for the same reason: the
/// alternative is inventing a caller to satisfy the lint, which is how a
/// mechanism ends up with a shape nobody chose. Remove this the moment the
/// executor lands.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreparationSlot {
    /// Nothing staged. The current generation is the only one.
    Empty,
    /// A successor is staged and the durable row exists. `predecessor` is the
    /// incarnation the commit must find the pointer still naming — recorded at
    /// preparation time rather than read at commit time, because a pointer that
    /// moved means a newer player generation exists and the correct outcome is
    /// to abort this successor rather than reap that one.
    Staged {
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
    },
    /// The successor is being torn down. Terminal for this slot: an abort that
    /// is under way cannot become a commit, or a disconnect could publish a
    /// successor nobody asked for.
    Aborting { staged_incarnation_id: String },
}

impl PreparationSlot {
    pub(crate) fn staged_incarnation_id(&self) -> Option<&str> {
        match self {
            Self::Empty => None,
            Self::Staged {
                staged_incarnation_id,
                ..
            }
            | Self::Aborting {
                staged_incarnation_id,
            } => Some(staged_incarnation_id),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Whether a commit may be attempted for this exact successor.
    ///
    /// Identity is checked rather than assumed: a commit naming a successor
    /// this slot is not holding is a stale executor speaking for a preparation
    /// that has already been replaced, and honouring it would advance the
    /// pointer to an incarnation the actor has forgotten.
    pub(crate) fn may_commit(&self, staged_incarnation_id: &str) -> bool {
        matches!(
            self,
            Self::Staged {
                staged_incarnation_id: staged,
                ..
            } if staged == staged_incarnation_id
        )
    }

    /// Whether an abort may be attempted for this exact successor. An abort
    /// already under way is idempotent — an owner retrying after a crash must
    /// read back the same outcome rather than a spurious loss.
    pub(crate) fn may_abort(&self, staged_incarnation_id: &str) -> bool {
        self.staged_incarnation_id() == Some(staged_incarnation_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ProducerDecision {
    Retry {
        decision_sequence: u64,
        failed_attempt: u64,
        recipe: ValidatedRetryRecipe,
        reason: ProducerDecisionReason,
    },
    Fail {
        decision_sequence: u64,
        failed_attempt: u64,
        reason: ProducerDecisionReason,
        proposal: Option<ActionProposal>,
        cleanup: ProducerFailureCleanup,
    },
}

impl ProducerDecision {
    pub(crate) fn decision_sequence(&self) -> u64 {
        match self {
            Self::Retry {
                decision_sequence, ..
            }
            | Self::Fail {
                decision_sequence, ..
            } => *decision_sequence,
        }
    }

    pub(crate) fn reason(&self) -> ProducerDecisionReason {
        match self {
            Self::Retry { reason, .. } | Self::Fail { reason, .. } => *reason,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProducerDecisionPoll {
    Idle,
    Decision(Arc<ProducerDecision>),
    ClassifyExit(RollingProducerExitProbe),
    Terminal(RollingTerminalCause),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RollingProducerExecutorPoll {
    Decision(Arc<ProducerDecision>),
    ClassifyExit(RollingProducerExitProbe),
    Terminal(RollingTerminalCause),
    Unavailable,
}

/// One immutable, exact-attempt request for the executor's bounded playlist
/// completion read. The actor's `ClassifyingExit` progress deadline is the
/// sole verdict clock; the probe does not create a second watchdog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RollingProducerExitProbe {
    pub probe_sequence: u64,
    pub producer_attempt: u64,
    pub deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RollingProducerCompletionEvidence {
    pub probe_sequence: u64,
    pub producer_attempt: u64,
    pub end_list: bool,
    pub final_segment: Option<i64>,
    pub final_end_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RollingProducerCompletionDisposition {
    CompleteVerifiedDuration,
    CompleteUnverifiedDuration,
    FailedPartialSuccess,
    Stale,
}

impl RollingProducerCompletionDisposition {
    fn metric_index(self) -> usize {
        match self {
            Self::CompleteVerifiedDuration => 0,
            Self::CompleteUnverifiedDuration => 1,
            Self::FailedPartialSuccess => 2,
            Self::Stale => 3,
        }
    }

    fn record(self) -> Self {
        ROLLING_PRODUCER_EXIT_CLASSIFICATIONS[self.metric_index()].fetch_add(1, Ordering::Relaxed);
        self
    }
}

enum DecisionTransportPoll {
    Available(ProducerDecisionPoll),
    Unavailable,
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
    pub startup_kind: Option<&'static str>,
    pub presentation_contract_fingerprint: Option<String>,
    pub metadata_response_authorized: bool,
    pub producer_media_published: bool,
    pub retry_state: &'static str,
    /// Immutable producer decision identity, when one has been retained.
    pub decision_sequence: Option<u64>,
    pub decision_reason: Option<&'static str>,
    /// Bounded executor/lifecycle state, including exact DecisionApplied
    /// acknowledgement for the opted-in producer lifetime.
    pub executor_state: &'static str,
    pub executor_pending_decision_age_ms: Option<i64>,
    pub executor_last_observed_sequence: u64,
    pub executor_last_action_failure: Option<&'static str>,
    pub executor_registered: bool,
    pub decision_applied_sequence: Option<u64>,
    pub decision_installed_attempt: Option<u64>,
    pub pending_probe_sequence: Option<u64>,
    pub pending_probe_attempt: Option<u64>,
    pub pending_probe_deadline_remaining_ms: Option<i64>,
    pub last_probe_outcome: &'static str,
    pub producer_ended_with_proposal: bool,
    pub proposal: Option<RollingProducerProposalSnapshot>,
    pub completion: &'static str,
    pub completion_attempt: Option<u64>,
    pub completion_final_segment: Option<i64>,
    pub completion_final_end_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RollingProducerProposalSnapshot {
    pub proposal_id: String,
    pub kind: &'static str,
    pub reason: &'static str,
    pub source: &'static str,
    pub severity: &'static str,
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

    fn projection(self) -> u8 {
        match self {
            Self::End => 1,
            Self::AuthorityFence => 2,
            Self::LeaseExpired => 3,
        }
    }

    fn from_projection(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::End),
            2 => Some(Self::AuthorityFence),
            3 => Some(Self::LeaseExpired),
            _ => None,
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
    /// Exact-attempt terminal process observation. Actor-managed producers
    /// classify this ordered fact under their frozen exit contract; passive
    /// compatibility actors retain it for diagnostics only.
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

/// The actor's sole rolling-producer clock. When an actor-managed producer is
/// admitted, expiry is sequenced into its one immutable retry-or-fail
/// decision; passive compatibility actors retain the observation only.
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

/// Provisional deadline observation retained when the exact armed clock
/// becomes due. It is deliberately not itself a `ProducerDecision`: legacy
/// actors leave it passive, while opted-in prepublication actors settle it
/// only after the exact publication/progress boundary is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerDeadlineDue {
    producer_attempt: u64,
    mode: ProducerProgressDeadlineMode,
    deadline: Instant,
}

/// Immediate typed observation for a non-success producer exit. Actor-managed
/// producers turn it into one immutable decision; compatibility actors retain
/// the ordered fact without gaining an action owner.
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

/// What the actor has been asked to make the producer's physical flow, as
/// distinct from what a successful signal has already made it.
///
/// The merged slices sequence only the *acknowledgement*: a successful
/// SIGSTOP/SIGCONT enters producer ingress as an ordered `FlowApplied`
/// barrier. The desire itself lived in a caller's stack frame and in the
/// `Session::suspended` atomic, so two evaluations could each believe they
/// owned the next signal. This carries the desire onto the same sequence
/// without touching the applied state: per the M4 contract, requesting a hold
/// does not change the running deadline, and a resume arms nothing until its
/// acknowledgement lands.
/// One actor-issued physical transaction awaiting acknowledgement.
///
/// This is a lifecycle bound, not a second progress verdict. It is armed only
/// while an actor-authorized signal is outstanding, and its expiry records a
/// bounded action failure rather than competing with the progress decision.
/// Only the flow kinds exist here: retry, install and cleanup actions are
/// their own later slices, and pre-approving names for them would put symbols
/// in the ownership ledger that nothing yet issues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerActionKind {
    SignalStop,
    SignalResume,
}

impl ProducerActionKind {
    /// The typed failure a lapsed action becomes.
    fn deadline_reason(self) -> ProducerDecisionReason {
        match self {
            Self::SignalStop => ProducerDecisionReason::FlowStopDeadline,
            Self::SignalResume => ProducerDecisionReason::FlowResumeDeadline,
        }
    }

    fn metric(self) -> (usize, &'static str) {
        match self {
            Self::SignalStop => (0, "signal_stop"),
            Self::SignalResume => (1, "signal_resume"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingProducerAction {
    action_sequence: u64,
    kind: ProducerActionKind,
    producer_attempt: u64,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerFlowIntention {
    /// Monotonic on the desired side only. Distinct from the applied ingress
    /// coordinate in `producer_flow_revision`; conflating them would let an
    /// acknowledgement silence a newer desire.
    revision: u64,
    desired: ProducerPhysicalFlowState,
}

/// The actor's answer to one intention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProducerFlowIntentionOutcome {
    /// The desire was accepted and no signal is outstanding, so the caller
    /// owns issuing exactly this one.
    Issue { revision: u64, hold: bool },
    /// Accepted and recorded, but a signal for an earlier revision is still
    /// outstanding. The contract coalesces here: its acknowledgement is
    /// applied first, and the actor issues again only if the desire still
    /// differs. The caller must not signal.
    Coalesced,
    /// The desire already matches the applied state and nothing is
    /// outstanding, so there is nothing to do.
    Settled,
    /// An older revision, a different attempt, a retired actor, or revoked
    /// signal authorization. Never a reason to signal.
    Rejected,
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
    install_authorization: Option<ProducerInstallCoordinate>,
    producer_signal_authorized: bool,
}

impl RollingProducerTransitionFence {
    fn new(lease_deadline: Instant) -> Self {
        Self {
            lease_deadline,
            install_authorization: None,
            producer_signal_authorized: true,
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RollingResponseObject {
    MasterPlaylist,
    VideoMediaPlaylist,
    SubtitleMediaPlaylist,
    SubtitleSegment,
    InitializationSegment,
    MediaSegment,
    ByteRange,
    NotModified,
    RangeNotSatisfiable,
    ProtocolResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RollingResponsePublicationBinding {
    GenerationMetadata {
        presentation_contract_fingerprint: String,
    },
    AttemptMedia {
        producer_attempt: u64,
        /// Exact numeric HLS segment coordinate for media-segment responses.
        /// `None` is valid only for non-segment media and init-backed range or
        /// not-modified responses; the actor validates that relationship.
        media_segment_index: Option<i64>,
    },
    /// Exact-attempt authorization for a typed status response. Unlike media
    /// publication, this neither closes retry nor claims first-media
    /// ownership; unlike the legacy protocol response, it remains valid after
    /// media publication.
    AttemptStatus {
        producer_attempt: u64,
    },
    ProtocolOnly {
        producer_attempt: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RollingResponsePublication {
    pub object: RollingResponseObject,
    pub binding: RollingResponsePublicationBinding,
}

impl RollingResponsePublication {
    pub(crate) fn generation_metadata(
        object: RollingResponseObject,
        presentation_contract_fingerprint: String,
    ) -> Self {
        Self {
            object,
            binding: RollingResponsePublicationBinding::GenerationMetadata {
                presentation_contract_fingerprint,
            },
        }
    }

    pub(crate) fn attempt_media(
        object: RollingResponseObject,
        producer_attempt: u64,
        media_segment_index: Option<i64>,
    ) -> Self {
        Self {
            object,
            binding: RollingResponsePublicationBinding::AttemptMedia {
                producer_attempt,
                media_segment_index,
            },
        }
    }

    pub(crate) fn attempt_status(object: RollingResponseObject, producer_attempt: u64) -> Self {
        Self {
            object,
            binding: RollingResponsePublicationBinding::AttemptStatus { producer_attempt },
        }
    }

    pub(crate) fn protocol_only(object: RollingResponseObject, producer_attempt: u64) -> Self {
        Self {
            object,
            binding: RollingResponsePublicationBinding::ProtocolOnly { producer_attempt },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RollingResponseAuthorization {
    pub first_producer_media_publication: bool,
}

/// Concrete, panic-free first-media ownership latch. The actor may flip this
/// while holding its transition fence because the operation is bounded to one
/// atomic store plus a retained notification permit; no caller code executes
/// and no actor re-entry is possible.
pub(crate) struct RollingFirstMediaPublicationHandoff {
    state: Arc<RollingFirstMediaPublicationHandoffState>,
}

struct RollingFirstMediaPublicationHandoffState {
    outcome: AtomicU8,
    notify: tokio::sync::Notify,
    prepublication_projection: Option<Arc<AtomicBool>>,
    /// Capacity follows the move-only actor command, not the request future.
    /// A timed-out request therefore cannot release admission while its
    /// unsettled handoff is still retained in the actor mailbox.
    _settlement_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

pub(crate) struct RollingFirstMediaPublicationWaiter {
    state: Arc<RollingFirstMediaPublicationHandoffState>,
}

impl RollingFirstMediaPublicationHandoff {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(RollingFirstMediaPublicationHandoffState {
                outcome: AtomicU8::new(0),
                notify: tokio::sync::Notify::new(),
                prepublication_projection: None,
                _settlement_permit: None,
            }),
        }
    }

    pub(crate) fn with_prepublication_projection(
        projection: Arc<AtomicBool>,
        settlement_permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        Self {
            state: Arc::new(RollingFirstMediaPublicationHandoffState {
                outcome: AtomicU8::new(0),
                notify: tokio::sync::Notify::new(),
                prepublication_projection: Some(projection),
                _settlement_permit: Some(settlement_permit),
            }),
        }
    }

    pub(crate) fn waiter(&self) -> RollingFirstMediaPublicationWaiter {
        RollingFirstMediaPublicationWaiter {
            state: Arc::clone(&self.state),
        }
    }

    fn settle(&self, accepted: bool) {
        self.state.settle(accepted);
    }

    fn is_pending(&self) -> bool {
        self.state.outcome.load(Ordering::Acquire) == 0
    }

    #[cfg(test)]
    pub(crate) fn settle_for_test(&self, accepted: bool) {
        self.settle(accepted);
    }
}

impl RollingFirstMediaPublicationHandoffState {
    fn settle(&self, accepted: bool) {
        if self
            .outcome
            .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if accepted {
                if let Some(projection) = &self.prepublication_projection {
                    projection.store(false, Ordering::Release);
                }
            }
            self.outcome
                .store(if accepted { 1 } else { 2 }, Ordering::Release);
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

impl Drop for RollingFirstMediaPublicationHandoff {
    fn drop(&mut self) {
        // The command is the only settlement owner. If it is dropped before
        // actor application (enqueue cancellation, actor exit, receiver
        // teardown), release the separate waiter with a rejection verdict.
        self.settle(false);
    }
}

impl RollingFirstMediaPublicationWaiter {
    /// Wait until the actor has settled this exact authorization. `true`
    /// transfers first-media ownership; `false` means the response was either
    /// rejected, was not first media, or its command owner disappeared.
    pub(crate) async fn wait(&self) -> bool {
        loop {
            match self.state.outcome.load(Ordering::Acquire) {
                1 => return true,
                2 => return false,
                _ => {}
            }
            self.state.notify.notified().await;
        }
    }

    pub(crate) fn settled_outcome(&self) -> Option<bool> {
        match self.state.outcome.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    fn settle_rejected(&self) {
        self.state.settle(false);
    }

    #[cfg(test)]
    fn is_accepted(&self) -> bool {
        self.state.outcome.load(Ordering::Acquire) == 1
    }
}

/// Move-only completion for one exact rolling segment EOF. The actor consumes
/// it while holding the producer transition fence, so the authoritative
/// commit, compatibility projection, and flow wake are one synchronous
/// operation even if the HTTP future or actor reply disappears immediately
/// after acceptance.
pub(crate) struct RollingMediaCommitHandoff {
    producer_attempt: u64,
    segment_index: i64,
    segment_end_ms: Option<i64>,
    compatibility_attempt: Arc<std::sync::Mutex<u64>>,
    high_segment: Arc<AtomicI64>,
    fetched_end_ms: Arc<AtomicI64>,
    flow_sync: Arc<RollingFlowSync>,
}

impl RollingMediaCommitHandoff {
    fn matches(
        &self,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
    ) -> bool {
        self.producer_attempt == producer_attempt
            && segment_index == Some(self.segment_index)
            && self.segment_end_ms == segment_end_ms
    }

    fn settle(self, accepted: bool) {
        if !accepted {
            return;
        }
        let projected_attempt = self
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *projected_attempt == self.producer_attempt {
            let previous = self
                .high_segment
                .fetch_max(self.segment_index, Ordering::Relaxed);
            if self.segment_index >= previous {
                if let Some(end) = self.segment_end_ms {
                    self.fetched_end_ms.fetch_max(end, Ordering::Relaxed);
                }
            }
        }
        drop(projected_attempt);
        self.flow_sync.request();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponsePublicationRejection {
    SessionEnded,
    ProducerNotAdmitted,
    StaleAttempt,
    PresentationContractMismatch,
    DecisionCommitted,
    InvalidBinding,
    ControlUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProducerAttemptRejection {
    SessionEnded,
    PlaylistPublished,
    StaleAttempt,
    AttemptExhausted,
    ExecutorNotRegistered,
    ExecutorLost,
    InitialAttemptAlreadyAdmitted,
    InvalidPolicy,
    RetryUnavailable,
    DecisionMismatch,
    RecipeMismatch,
    PresentationContractMismatch,
    ClassifierMismatch,
    DuplicateClassification,
    ControlUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerInstallCoordinate {
    revision: u64,
    producer_attempt: u64,
    producer_deadline: Option<Instant>,
}

/// Move-only proof that the actor authorized one exact producer installation.
/// Its private coordinate is consumed by the synchronous final install fence;
/// callers cannot reconstruct authority from an attempt number.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "producer installation remains unauthorized until this token crosses the final fence"]
pub(crate) struct ProducerInstallAuthorization {
    coordinate: ProducerInstallCoordinate,
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
    /// This exchange's selection differs from the last accepted one.
    ///
    /// M6's gate, and the reason it lives here: building a candidate recipe
    /// costs two store reads (the source file and the network prior), the
    /// exchange runs about once a second per client under an absolute
    /// deadline, and the answer is almost always *nothing changed*. The actor
    /// holds both selections at the moment of acceptance, so the comparison is
    /// free here and nowhere else.
    ///
    /// **Against the previous selection, never against what is delivered.**
    /// A client's ask and the height it gets are not the same number — an ask
    /// of 1079 snaps onto the ladder and is delivered as 1080 — so a gate that
    /// compared the two would read *changed* on every exchange of a snapped
    /// session and spend the reads it exists to save. `false` on a replay and
    /// on the first accepted exchange: a session that has just been created
    /// from an intent has not since departed from it.
    pub selection_changed: bool,
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
/// shared ordering is action-passive for legacy actors and decision-bearing
/// only for an explicit actor-managed producer constructor.
struct RollingProducerIngress {
    state: std::sync::Mutex<RollingProducerIngressState>,
    notify: tokio::sync::Notify,
    flow_capacity_available: tokio::sync::Notify,
    #[cfg(test)]
    flow_capacity_wait_started: tokio::sync::Notify,
}

struct RollingProducerIngressState {
    next_sequence: u64,
    progress_budget: Duration,
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

impl Default for RollingProducerIngressState {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            progress_budget: PRODUCER_PROGRESS_BUDGET,
            progress_watermark_attempt: 0,
            progress_watermark_out_time_ms: None,
            progress: None,
            exit: None,
            flow: std::collections::VecDeque::new(),
            flow_reservations: 0,
            sealed_flow_barriers: 0,
            #[cfg(test)]
            last_flow_applied: None,
        }
    }
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
    fn new(sample: PublishedProgress, progress_budget: Duration) -> Self {
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
                .checked_add(progress_budget)
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

    fn push(&mut self, mut sample: PublishedProgress, progress_budget: Duration) {
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
                        .checked_add(progress_budget)
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
                        .checked_add(progress_budget)
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

    fn set_progress_budget(&self, progress_budget: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.progress_budget = progress_budget;
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
                let progress_budget = state.progress_budget;
                state
                    .progress
                    .as_mut()
                    .expect("merged progress batch")
                    .push(
                        PublishedProgress {
                            sequence,
                            published_at,
                            observation: incoming,
                        },
                        progress_budget,
                    );
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
            state.progress = Some(ProgressCoverageBatch::new(
                PublishedProgress {
                    sequence,
                    published_at,
                    observation,
                },
                state.progress_budget,
            ));
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

    #[cfg(test)]
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
    /// Strong lifetime owner for the weak executor transport. Dropping the
    /// final handle must close the actor rather than leave a self-owned task.
    #[allow(dead_code)]
    decision_transport: Arc<RollingDecisionTransport>,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_abort: Option<tokio::task::AbortHandle>,
    #[cfg(test)]
    executor_abort: Option<tokio::task::AbortHandle>,
    #[cfg(test)]
    actor_run_started: Arc<tokio::sync::Notify>,
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
    #[cfg(test)]
    BeginProducerAttempt {
        reply: tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
    },
    RegisterProducerExecutor {
        reply: tokio::sync::oneshot::Sender<Result<(), ProducerAttemptRejection>>,
    },
    BeginInitialProducerAttempt {
        policy: InitialProducerPolicy,
        reply: tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
    },
    PollProducerDecision {
        after_sequence: u64,
        reply: tokio::sync::oneshot::Sender<ProducerDecisionPoll>,
    },
    /// Take the preparation slot for a successor. `false` when it is already
    /// occupied or the playback is terminal.
    StagePreparation {
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Ask whether this exact successor may still be committed, immediately
    /// before the durable CAS. The gate and the store call cannot be one
    /// transaction, so the actor is asked as late as possible and its answer
    /// is re-checked on the way back in `SettlePreparation`.
    MayCommitPreparation {
        staged_incarnation_id: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    /// Report a preparation's durable outcome. `committed` false covers both a
    /// completed abort and a lost commit CAS — the store returning `Ok(None)`
    /// because the pointer no longer names the recorded predecessor. Both free
    /// the slot; neither reaps the newer generation.
    SettlePreparation {
        staged_incarnation_id: String,
        committed: bool,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    #[cfg(test)]
    InstallProducerDecision {
        decision: ProducerDecision,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    AuthorizeProducerInstall {
        producer_attempt: u64,
        reply: tokio::sync::oneshot::Sender<
            Result<ProducerInstallAuthorization, ProducerAttemptRejection>,
        >,
    },
    BindResponsePublicationContract {
        presentation_contract_fingerprint: String,
        failure_fence: Arc<AtomicBool>,
        reply: tokio::sync::oneshot::Sender<Result<(), ResponsePublicationRejection>>,
    },
    AuthorizeResponsePublication {
        publication: RollingResponsePublication,
        handoff: Option<RollingFirstMediaPublicationHandoff>,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<
            Result<RollingResponseAuthorization, ResponsePublicationRejection>,
        >,
    },
    AdmitProducerRetry {
        decision_sequence: u64,
        recipe_fingerprint: String,
        reply: tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
    },
    DecisionApplied {
        decision_sequence: u64,
        installed_attempt: Option<u64>,
        reply: tokio::sync::oneshot::Sender<Result<(), ProducerAttemptRejection>>,
    },
    ExecutorSettled {
        reply: tokio::sync::oneshot::Sender<Result<(), ProducerAttemptRejection>>,
    },
    ClassifyProducerExit {
        evidence: RollingProducerCompletionEvidence,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<RollingProducerCompletionDisposition>,
    },
    ClassifyCopyProducerExit {
        producer_attempt: u64,
        classification: CopyProducerExitClassification,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<Result<(), ProducerAttemptRejection>>,
    },
    ObservePublication {
        observation: RollingPublicationObservation,
        deadline: Option<Instant>,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    CommitMedia {
        kind: &'static str,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
        handoff: Option<RollingMediaCommitHandoff>,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    CommitGenerationMetadata {
        presentation_contract_fingerprint: String,
        kind: &'static str,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    ApplyProducerFlow {
        producer_attempt: u64,
        desired_hold: bool,
        reply: tokio::sync::oneshot::Sender<ProducerFlowIntentionOutcome>,
    },
    SettleProducerFlowSignal {
        producer_attempt: u64,
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
            #[cfg(test)]
            Self::BeginProducerAttempt { .. } => Some(1),
            Self::RegisterProducerExecutor { .. } => Some(9),
            Self::BeginInitialProducerAttempt { .. } => Some(10),
            Self::PollProducerDecision { .. } => Some(2),
            #[cfg(test)]
            Self::InstallProducerDecision { .. } => None,
            Self::AuthorizeProducerInstall { .. } => Some(3),
            Self::BindResponsePublicationContract { .. } => Some(15),
            Self::AuthorizeResponsePublication { .. } => Some(11),
            Self::AdmitProducerRetry { .. } => Some(12),
            Self::DecisionApplied { .. } => Some(13),
            Self::ClassifyProducerExit { .. } => Some(16),
            Self::ExecutorSettled { .. } => Some(17),
            Self::ClassifyCopyProducerExit { .. } => Some(18),
            Self::ApplyProducerFlow { .. } => Some(19),
            Self::SettleProducerFlowSignal { .. } => Some(20),
            Self::StagePreparation { .. } => Some(21),
            Self::MayCommitPreparation { .. } => Some(22),
            Self::SettlePreparation { .. } => Some(23),
            Self::ObservePublication { .. } => Some(4),
            Self::CommitMedia { .. } => Some(5),
            Self::CommitGenerationMetadata { .. } => Some(14),
            Self::Snapshot { .. } => Some(6),
            Self::ClaimExpiry { .. } => Some(7),
            Self::Terminal { .. } => Some(8),
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

/// A move-only inbox is created once for a rolling generation. Its receiver
/// belongs to the one passive executor task; callers can retain only the
/// bounded sender held by the actor-side wake path.
struct RollingSessionExecutorInbox {
    receiver: tokio::sync::mpsc::Receiver<()>,
}

impl RollingSessionExecutorInbox {
    fn new() -> (tokio::sync::mpsc::Sender<()>, Self) {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        (sender, Self { receiver })
    }
}

#[derive(Default)]
struct RollingExecutorObservation {
    /// 0=unregistered, 1=idle, 2=executing, 3=terminal, 4=lost,
    /// 5=queued, 6=acknowledged, 7=expected-settled.
    state: AtomicU8,
    last_observed_sequence: AtomicU64,
}

impl RollingExecutorObservation {
    fn state(&self) -> &'static str {
        match self.state.load(Ordering::Acquire) {
            1 => "idle",
            2 => "executing",
            3 => "terminal",
            4 => "lost",
            5 => "queued",
            6 => "acknowledged",
            7 => "settled",
            _ => "unregistered",
        }
    }

    fn register(&self) -> bool {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            || self.state.load(Ordering::Acquire) == 1
    }

    fn queue_decision(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (!matches!(state, 3 | 4 | 7)).then_some(5)
            });
    }

    fn acknowledge(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (!matches!(state, 3 | 4 | 7)).then_some(6)
            });
    }

    fn begin_observing(&self) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (!matches!(state, 3 | 4 | 7)).then_some(2)
            })
            .is_ok()
    }

    fn finish_observing(&self) -> bool {
        self.state
            .compare_exchange(2, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn settle_terminal(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state != 4).then_some(3)
            });
    }

    fn settle_lost(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (!matches!(state, 3 | 4 | 7)).then_some(4)
            });
    }

    fn settle_expected(&self) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (!matches!(state, 3 | 4 | 7)).then_some(7)
            })
            .is_ok()
            || self.state.load(Ordering::Acquire) == 7
    }

    fn is_expected_settled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 7
    }
}

/// Actor-owned half of the decision path. It deliberately contains no actor
/// command sender, so keeping the wake path alive cannot keep the actor's own
/// mailbox open after every external handle is dropped.
struct RollingDecisionWake {
    decision_notify: Arc<tokio::sync::Notify>,
    executor_wake: tokio::sync::mpsc::Sender<()>,
    executor_observation: Arc<RollingExecutorObservation>,
    terminal_projection: Arc<AtomicU8>,
}

impl RollingDecisionWake {
    fn wake(&self) {
        // Notify is the retained decision permit required by the M4 contract.
        // The capacity-one inbox additionally wakes on actor-side closure. A
        // full inbox already guarantees another poll, so coalescing is safe.
        self.decision_notify.notify_one();
        let _ = self.executor_wake.try_send(());
    }
}

/// Handle-owned command half of the decision path. The passive executor holds
/// only a `Weak` reference, so it cannot keep a retired actor/session alive.
/// Polling is deliberately observational and has no DecisionApplied command
/// in this slice.
struct RollingDecisionTransport {
    sender: tokio::sync::mpsc::Sender<RollingControlEnvelope>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    producer_events: Arc<RollingProducerIngress>,
    executor_observation: Arc<RollingExecutorObservation>,
    terminal_projection: Arc<AtomicU8>,
    #[cfg(test)]
    executor_poll_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    executor_observation_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
}

impl RollingDecisionTransport {
    fn committed_terminal(&self) -> Option<RollingTerminalCause> {
        RollingTerminalCause::from_projection(self.terminal_projection.load(Ordering::Acquire))
    }

    async fn poll_after(&self, after_sequence: u64) -> DecisionTransportPoll {
        let (reply, response) = tokio::sync::oneshot::channel();
        let permit = match self.sender.reserve().await {
            Ok(permit) => permit,
            Err(_) => {
                return self
                    .committed_terminal()
                    .map_or(DecisionTransportPoll::Unavailable, |cause| {
                        DecisionTransportPoll::Available(ProducerDecisionPoll::Terminal(cause))
                    });
            }
        };
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope =
            self.producer_events
                .seal_command(RollingControlCommand::PollProducerDecision {
                    after_sequence,
                    reply,
                });
        permit.send(envelope);
        drop(transition);
        response
            .await
            .map(DecisionTransportPoll::Available)
            .unwrap_or_else(|_| {
                self.committed_terminal()
                    .map_or(DecisionTransportPoll::Unavailable, |cause| {
                        DecisionTransportPoll::Available(ProducerDecisionPoll::Terminal(cause))
                    })
            })
    }

    async fn register_executor(&self) -> Result<(), ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let permit = self
            .sender
            .reserve()
            .await
            .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope = self
            .producer_events
            .seal_command(RollingControlCommand::RegisterProducerExecutor { reply });
        permit.send(envelope);
        drop(transition);
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    async fn settle_executor_expected(&self) -> Result<(), ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let permit = self
            .sender
            .reserve()
            .await
            .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        let transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let envelope = self
            .producer_events
            .seal_command(RollingControlCommand::ExecutorSettled { reply });
        permit.send(envelope);
        drop(transition);
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    fn spawn_executor(
        self: &Arc<Self>,
        weak: Weak<Self>,
        decision_notify: Arc<tokio::sync::Notify>,
        mut inbox: RollingSessionExecutorInbox,
    ) -> tokio::task::JoinHandle<()> {
        let observation = Arc::clone(&self.executor_observation);
        #[cfg(test)]
        let executor_poll_pause = Arc::clone(&self.executor_poll_pause);
        #[cfg(test)]
        let executor_observation_pause = Arc::clone(&self.executor_observation_pause);
        tokio::spawn(async move {
            let mut observed_sequence = 0_u64;
            loop {
                let notified = decision_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let wake = tokio::select! {
                    _ = notified.as_mut() => true,
                    wake = inbox.receiver.recv() => wake.is_some(),
                };
                let Some(transport) = weak.upgrade() else {
                    break;
                };
                if !wake {
                    if transport.committed_terminal().is_some() {
                        observation.settle_terminal();
                    } else {
                        observation.settle_lost();
                    }
                    break;
                }
                if !observation.begin_observing() {
                    break;
                }
                #[cfg(test)]
                let poll_pause = {
                    executor_poll_pause
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                };
                #[cfg(test)]
                if let Some(pause) = poll_pause {
                    pause.wait().await;
                    pause.wait().await;
                }
                let poll = transport.poll_after(observed_sequence).await;
                #[cfg(test)]
                let observation_pause = {
                    executor_observation_pause
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                };
                #[cfg(test)]
                if let Some(pause) = observation_pause {
                    pause.wait().await;
                    pause.wait().await;
                }
                match poll {
                    DecisionTransportPoll::Available(ProducerDecisionPoll::Idle) => {
                        if !observation.finish_observing() {
                            break;
                        }
                    }
                    DecisionTransportPoll::Available(ProducerDecisionPoll::Decision(decision)) => {
                        // Polling never consumes the actor slot. The local
                        // observation cursor only prevents a passive task
                        // from repeatedly observing one retained value.
                        observed_sequence = decision.decision_sequence();
                        observation
                            .last_observed_sequence
                            .store(observed_sequence, Ordering::Release);
                        if !observation.finish_observing() {
                            break;
                        }
                    }
                    DecisionTransportPoll::Available(ProducerDecisionPoll::ClassifyExit(_)) => {
                        // Only the explicit executor registration is eligible
                        // for completion work. Compatibility actors never
                        // create this value; keep this observer passive if a
                        // future constructor is miswired.
                        if !observation.finish_observing() {
                            break;
                        }
                    }
                    DecisionTransportPoll::Available(ProducerDecisionPoll::Terminal(_)) => {
                        observation.settle_terminal();
                        break;
                    }
                    DecisionTransportPoll::Unavailable => {
                        observation.settle_lost();
                        break;
                    }
                }
            }
        })
    }
}

/// Move-only receiver for the one real pre-publication action executor. It
/// holds only a weak command transport, so an abandoned session handle still
/// closes the actor. Registration is an explicit actor command and must win
/// before the initial producer policy can arm a deadline.
pub(crate) struct RollingProducerExecutorRegistration {
    transport: Weak<RollingDecisionTransport>,
    terminal_projection: Arc<AtomicU8>,
    inbox: RollingSessionExecutorInbox,
    registered: bool,
}

impl RollingProducerExecutorRegistration {
    pub(crate) async fn register(&mut self) -> Result<(), ProducerAttemptRejection> {
        if self.registered {
            return Ok(());
        }
        let transport = self
            .transport
            .upgrade()
            .ok_or(ProducerAttemptRejection::ControlUnavailable)?;
        transport.register_executor().await?;
        self.registered = true;
        Ok(())
    }

    pub(crate) async fn next_decision(&mut self) -> RollingProducerExecutorPoll {
        if !self.registered {
            return RollingProducerExecutorPoll::Unavailable;
        }
        loop {
            let Some(transport) = self.transport.upgrade() else {
                return RollingTerminalCause::from_projection(
                    self.terminal_projection.load(Ordering::Acquire),
                )
                .map_or(
                    RollingProducerExecutorPoll::Unavailable,
                    RollingProducerExecutorPoll::Terminal,
                );
            };
            if !transport.executor_observation.begin_observing() {
                return transport.committed_terminal().map_or(
                    RollingProducerExecutorPoll::Unavailable,
                    RollingProducerExecutorPoll::Terminal,
                );
            }
            match transport.poll_after(0).await {
                DecisionTransportPoll::Available(ProducerDecisionPoll::Decision(decision)) => {
                    transport
                        .executor_observation
                        .last_observed_sequence
                        .store(decision.decision_sequence(), Ordering::Release);
                    return RollingProducerExecutorPoll::Decision(decision);
                }
                DecisionTransportPoll::Available(ProducerDecisionPoll::ClassifyExit(probe)) => {
                    return RollingProducerExecutorPoll::ClassifyExit(probe);
                }
                DecisionTransportPoll::Available(ProducerDecisionPoll::Terminal(cause)) => {
                    transport.executor_observation.settle_terminal();
                    return RollingProducerExecutorPoll::Terminal(cause);
                }
                DecisionTransportPoll::Available(ProducerDecisionPoll::Idle) => {
                    if !transport.executor_observation.finish_observing() {
                        return transport.committed_terminal().map_or(
                            RollingProducerExecutorPoll::Unavailable,
                            RollingProducerExecutorPoll::Terminal,
                        );
                    }
                }
                DecisionTransportPoll::Unavailable => {
                    transport.executor_observation.settle_lost();
                    return RollingProducerExecutorPoll::Unavailable;
                }
            }
            drop(transport);
            if self.inbox.receiver.recv().await.is_none() {
                return RollingTerminalCause::from_projection(
                    self.terminal_projection.load(Ordering::Acquire),
                )
                .map_or(
                    RollingProducerExecutorPoll::Unavailable,
                    RollingProducerExecutorPoll::Terminal,
                );
            }
        }
    }

    /// Close the sole decision consumer after the actor has durably observed
    /// either successful completion or applied RetainPublished cleanup. The
    /// actor validates that terminal producer state before changing the shared
    /// receiver-close verdict from unexpected loss to expected settlement.
    pub(crate) async fn settle_expected(&mut self) -> Result<(), ProducerAttemptRejection> {
        if !self.registered {
            return Err(ProducerAttemptRejection::ExecutorNotRegistered);
        }
        let transport = self
            .transport
            .upgrade()
            .ok_or(ProducerAttemptRejection::ControlUnavailable)?;
        transport.settle_executor_expected().await?;
        self.registered = false;
        Ok(())
    }
}

#[cfg(test)]
struct DeferredProducerAttemptReply {
    reply: tokio::sync::oneshot::Sender<Result<u64, ProducerAttemptRejection>>,
    outcome: Result<u64, ProducerAttemptRejection>,
    reply_pause: Option<Arc<tokio::sync::Barrier>>,
}

struct RollingActorRuntime {
    retired_fence: Arc<AtomicBool>,
    producer_attempt: Arc<AtomicU64>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    decision_wake: Arc<RollingDecisionWake>,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_run_started: Arc<tokio::sync::Notify>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum PrepublicationRetryState {
    Unavailable,
    Available(ValidatedRetryRecipe),
    Reserved {
        decision_sequence: u64,
        recipe: ValidatedRetryRecipe,
    },
    Consumed,
    ClosedByPublication,
}

impl PrepublicationRetryState {
    fn status(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Available(_) => "available",
            Self::Reserved { .. } => "reserved",
            Self::Consumed => "consumed",
            Self::ClosedByPublication => "closed_by_publication",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProducerRetryAdmission {
    decision_sequence: u64,
    recipe_fingerprint: String,
    installed_attempt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerDecisionApplied {
    decision_sequence: u64,
    installed_attempt: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerCompletionState {
    Incomplete,
    CompleteVerifiedDuration {
        producer_attempt: u64,
        final_segment: i64,
        final_end_ms: i64,
    },
    CompleteUnverifiedDuration {
        producer_attempt: u64,
        final_segment: i64,
        final_end_ms: i64,
    },
}

impl ProducerCompletionState {
    fn status(self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::CompleteVerifiedDuration { .. } => "complete_verified_duration",
            Self::CompleteUnverifiedDuration { .. } => "complete_unverified_duration",
        }
    }

    fn evidence(self) -> (Option<u64>, Option<i64>, Option<i64>) {
        match self {
            Self::Incomplete => (None, None, None),
            Self::CompleteVerifiedDuration {
                producer_attempt,
                final_segment,
                final_end_ms,
            }
            | Self::CompleteUnverifiedDuration {
                producer_attempt,
                final_segment,
                final_end_ms,
            } => (
                Some(producer_attempt),
                Some(final_segment),
                Some(final_end_ms),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProducerProbeOutcome {
    None,
    Published,
    Cancelled,
    Stale,
}

impl ProducerProbeOutcome {
    fn status(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Published => "published",
            Self::Cancelled => "cancelled",
            Self::Stale => "stale",
        }
    }
}

struct PrepublicationProducerControl {
    executor_registered: bool,
    initial_policy: Option<InitialProducerPolicy>,
    retry_state: PrepublicationRetryState,
    retry_admission: Option<ProducerRetryAdmission>,
    metadata_response_authorized: bool,
    producer_media_published: bool,
    next_decision_sequence: u64,
    decision_applied: Option<ProducerDecisionApplied>,
    failure_applied: bool,
    last_action_failure: Option<&'static str>,
    next_probe_sequence: u64,
    pending_probe: Option<RollingProducerExitProbe>,
    last_probe_outcome: ProducerProbeOutcome,
    completion: ProducerCompletionState,
}

/// Immutable response-admission contract shared with a process-local session.
///
/// For a compatibility-owned producer this supplies both the presentation
/// identity and the failure fence. For an actor-owned prepublication producer,
/// `InitialProducerPolicy` remains the sole presentation/retry authority and
/// this contract supplies only the Acquire half of `Session::fail` ordering.
struct RollingResponsePublicationContract {
    presentation_contract_fingerprint: String,
    /// Monotone producer-failure publication shared with the compatibility
    /// process owner. A Release failure store ordered before the actor's
    /// Acquire load rejects publication; if the actor load wins first, that
    /// exact response authorization is the earlier linearized event.
    failure_fence: Arc<AtomicBool>,
    metadata_response_authorized: bool,
}

impl PrepublicationProducerControl {
    fn new() -> Self {
        Self {
            executor_registered: false,
            initial_policy: None,
            retry_state: PrepublicationRetryState::Unavailable,
            retry_admission: None,
            metadata_response_authorized: false,
            producer_media_published: false,
            next_decision_sequence: 1,
            decision_applied: None,
            failure_applied: false,
            last_action_failure: None,
            next_probe_sequence: 1,
            pending_probe: None,
            last_probe_outcome: ProducerProbeOutcome::None,
            completion: ProducerCompletionState::Incomplete,
        }
    }
}

struct RollingControlActor {
    control: ControlState,
    last_renewal: Instant,
    last_renewal_kind: &'static str,
    mode: RollingLeaseMode,
    demand: Option<PlaybackDemandSnapshot>,
    /// The capability document this session is holding, not this exchange's.
    ///
    /// `ControlRequestV1::validate` requires `capabilities` only on sequence 1
    /// and permits every later exchange to omit them, so a consumer reading
    /// the live snapshot sees `None` for the whole session after the first
    /// message. For a field meaning *this device can hold two live pipelines*
    /// that would refuse every transition a capable client ever makes — which
    /// is every transition that will actually happen.
    ///
    /// Last write wins over `Some`. A client that changes its answer
    /// mid-session is telling the truth about a device that changed — a
    /// television that woke a second decoder, a phone that lost one — so the
    /// newer document is the right one and no reconciliation is owed.
    ///
    /// **Nothing clears this on an owner-epoch advance, and it cannot go
    /// stale anyway.** An advance resets `client_instance_id`, after which the
    /// next accepted exchange must be sequence 1, and a sequence-1 exchange
    /// that carries no capabilities fails twice over: `validate` rejects the
    /// body, and the fence's `platform.ok_or(StaleClient)` rejects the accept.
    /// So the exchange that could read a stale document is the exchange that
    /// has just overwritten it.
    retained_capabilities: Option<DynamicCapabilities>,
    settled_target: Option<SettledTarget>,
    /// This playback's single preparation slot. One per playback is already
    /// the store's invariant; holding it here makes the actor the only thing
    /// that can move it.
    preparation: PreparationSlot,
    delivery: RollingDeliverySnapshot,
    producer_progress_at: Option<Instant>,
    producer_exit_at: Option<Instant>,
    producer_progress_deadline: Option<ProducerProgressDeadline>,
    producer_deadline_due: Option<ProducerDeadlineDue>,
    producer_process_exit_due: Option<ProducerProcessExitDue>,
    producer_exit_classifier: ProducerExitClassifier,
    copy_producer_exit_classification: Option<CopyProducerExitClassification>,
    producer_flow_revision: u64,
    producer_physical_flow: ProducerPhysicalFlowState,
    producer_flow_intention: ProducerFlowIntention,
    producer_flow_signal_outstanding: bool,
    pending_producer_action: Option<PendingProducerAction>,
    producer_action_sequence: u64,
    last_applied_ingress_sequence: u64,
    retired: bool,
    terminal: Option<RollingTerminalCause>,
    expiration_claimed: bool,
    retired_fence: Arc<AtomicBool>,
    producer_attempt: Arc<AtomicU64>,
    producer_transition: Arc<std::sync::Mutex<RollingProducerTransitionFence>>,
    flow_sync: Arc<RollingFlowSync>,
    producer_events: Arc<RollingProducerIngress>,
    decision_wake: Arc<RollingDecisionWake>,
    pending_decision: Option<Arc<ProducerDecision>>,
    last_decision: Option<Arc<ProducerDecision>>,
    decision_committed_at: Option<Instant>,
    executor_lost: bool,
    prepublication: Option<PrepublicationProducerControl>,
    response_publication_contract: Option<RollingResponsePublicationContract>,
    next_install_revision: u64,
    authorized_install: Option<ProducerInstallCoordinate>,
    producer_signal_authorized: bool,
    executor_loss_cutoff_pending: bool,
    last_flow_ticket: u64,
    #[cfg(test)]
    producer_attempt_reply_pause: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    #[cfg(test)]
    actor_run_started: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    actor_exit_fence_started: Arc<tokio::sync::Notify>,
}

impl RollingControlActor {
    #[cfg(test)]
    fn new(now: Instant, initial_kind: &'static str, retired_fence: Arc<AtomicBool>) -> Self {
        let producer_transition = Arc::new(std::sync::Mutex::new(
            RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
        ));
        let producer_events = Arc::new(RollingProducerIngress::new());
        let (executor_wake, _executor_inbox) = RollingSessionExecutorInbox::new();
        Self::with_runtime(
            now,
            initial_kind,
            false,
            RollingActorRuntime {
                retired_fence,
                producer_attempt: Arc::new(AtomicU64::new(0)),
                producer_transition: Arc::clone(&producer_transition),
                flow_sync: Arc::new(RollingFlowSync::new()),
                producer_events: Arc::clone(&producer_events),
                decision_wake: Arc::new(RollingDecisionWake {
                    decision_notify: Arc::new(tokio::sync::Notify::new()),
                    executor_wake,
                    executor_observation: Arc::new(RollingExecutorObservation::default()),
                    terminal_projection: Arc::new(AtomicU8::new(0)),
                }),
                producer_attempt_reply_pause: Arc::new(std::sync::Mutex::new(None)),
                actor_run_started: Arc::new(tokio::sync::Notify::new()),
                actor_exit_fence_started: Arc::new(tokio::sync::Notify::new()),
            },
        )
    }

    fn with_runtime(
        now: Instant,
        initial_kind: &'static str,
        prepublication_transcode: bool,
        runtime: RollingActorRuntime,
    ) -> Self {
        let RollingActorRuntime {
            retired_fence,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            decision_wake,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_run_started,
            #[cfg(test)]
            actor_exit_fence_started,
        } = runtime;
        Self {
            control: ControlState::default(),
            last_renewal: now,
            last_renewal_kind: initial_kind,
            mode: RollingLeaseMode::Legacy,
            demand: None,
            retained_capabilities: None,
            settled_target: None,
            preparation: PreparationSlot::Empty,
            delivery: RollingDeliverySnapshot::default(),
            producer_progress_at: None,
            producer_exit_at: None,
            producer_progress_deadline: None,
            producer_deadline_due: None,
            producer_process_exit_due: None,
            producer_exit_classifier: ProducerExitClassifier::Immediate,
            copy_producer_exit_classification: None,
            producer_flow_revision: 0,
            producer_physical_flow: ProducerPhysicalFlowState::Running,
            producer_flow_intention: ProducerFlowIntention {
                revision: 0,
                desired: ProducerPhysicalFlowState::Running,
            },
            producer_flow_signal_outstanding: false,
            pending_producer_action: None,
            producer_action_sequence: 0,
            last_applied_ingress_sequence: 0,
            retired: false,
            terminal: None,
            expiration_claimed: false,
            retired_fence,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            decision_wake,
            pending_decision: None,
            last_decision: None,
            decision_committed_at: None,
            executor_lost: false,
            prepublication: prepublication_transcode.then(PrepublicationProducerControl::new),
            response_publication_contract: None,
            next_install_revision: 1,
            authorized_install: None,
            producer_signal_authorized: !prepublication_transcode,
            executor_loss_cutoff_pending: false,
            last_flow_ticket: 0,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_run_started,
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
            settled_target: self.settled_target,
            delivery,
            retired: self.retired,
            terminal: self.terminal,
            expiration_claimed: self.expiration_claimed,
            producer_control: self.producer_operational_snapshot_at(now),
        }
    }

    fn has_terminal_prepublication_failure(&self) -> bool {
        self.prepublication.as_ref().is_some_and(|control| {
            !control.producer_media_published
                && (control.failure_applied
                    || self.executor_lost
                    || self.pending_decision.as_ref().is_some_and(|decision| {
                        matches!(decision.as_ref(), ProducerDecision::Fail { .. })
                    }))
        })
    }

    fn has_published_producer_failure_proposal(&self) -> bool {
        self.prepublication
            .as_ref()
            .is_some_and(|control| control.producer_media_published)
            && self
                .pending_decision
                .as_ref()
                .or(self.last_decision.as_ref())
                .is_some_and(|decision| {
                    matches!(
                        decision.as_ref(),
                        ProducerDecision::Fail {
                            proposal: Some(_),
                            ..
                        }
                    )
                })
    }

    fn producer_operational_snapshot_at(&self, now: Instant) -> RollingProducerOperationalSnapshot {
        let prepublication = self.prepublication.as_ref();
        let decision_evidence = self
            .pending_decision
            .as_ref()
            .or(self.last_decision.as_ref());
        let retained_proposal = decision_evidence.and_then(|decision| match decision.as_ref() {
            ProducerDecision::Fail {
                proposal: Some(proposal),
                ..
            } => Some(proposal),
            _ => None,
        });
        let producer_ended_with_proposal = prepublication
            .is_some_and(|control| control.producer_media_published)
            && retained_proposal.is_some();
        let completion = prepublication.map_or(ProducerCompletionState::Incomplete, |control| {
            control.completion
        });
        let (completion_attempt, completion_final_segment, completion_final_end_ms) =
            completion.evidence();
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
        } else if !matches!(completion, ProducerCompletionState::Incomplete) {
            "complete"
        } else if producer_ended_with_proposal {
            "producer_ended_with_proposal"
        } else if self.has_terminal_prepublication_failure() {
            "failed"
        } else if self.pending_decision.is_some() {
            "decided"
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
                None if prepublication.is_some_and(|control| control.producer_media_published) => {
                    "published"
                }
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
            observation_only: prepublication.is_none()
                || self.has_terminal_prepublication_failure()
                || producer_ended_with_proposal
                || !matches!(completion, ProducerCompletionState::Incomplete),
            action_owner: match prepublication {
                Some(_) if producer_ended_with_proposal => "producer_ended_with_proposal",
                Some(control)
                    if !matches!(control.completion, ProducerCompletionState::Incomplete) =>
                {
                    "complete"
                }
                Some(_) if self.has_terminal_prepublication_failure() => "terminal_failure",
                Some(_) => "playback_control_actor",
                None => "legacy_compatibility",
            },
            startup_kind: prepublication
                .and_then(|control| control.initial_policy.as_ref())
                .map(|policy| policy.startup_kind.status()),
            presentation_contract_fingerprint: prepublication
                .and_then(|control| control.initial_policy.as_ref())
                .map(|policy| policy.presentation_contract_fingerprint.clone())
                .or_else(|| {
                    self.response_publication_contract
                        .as_ref()
                        .map(|contract| contract.presentation_contract_fingerprint.clone())
                }),
            metadata_response_authorized: prepublication
                .is_some_and(|control| control.metadata_response_authorized)
                || self
                    .response_publication_contract
                    .as_ref()
                    .is_some_and(|contract| contract.metadata_response_authorized),
            producer_media_published: prepublication
                .is_some_and(|control| control.producer_media_published),
            retry_state: prepublication.map_or("legacy_compatibility", |control| {
                control.retry_state.status()
            }),
            decision_sequence: decision_evidence.map(|decision| decision.decision_sequence()),
            decision_reason: decision_evidence.map(|decision| decision.reason().status()),
            executor_state: self.decision_wake.executor_observation.state(),
            executor_pending_decision_age_ms: self.decision_committed_at.map(|committed_at| {
                i64::try_from(now.saturating_duration_since(committed_at).as_millis())
                    .unwrap_or(i64::MAX)
            }),
            executor_last_observed_sequence: self
                .decision_wake
                .executor_observation
                .last_observed_sequence
                .load(Ordering::Acquire),
            executor_last_action_failure: prepublication
                .and_then(|control| control.last_action_failure),
            executor_registered: prepublication.is_none_or(|control| control.executor_registered),
            decision_applied_sequence: prepublication
                .and_then(|control| control.decision_applied)
                .map(|applied| applied.decision_sequence),
            decision_installed_attempt: prepublication
                .and_then(|control| control.decision_applied)
                .and_then(|applied| applied.installed_attempt),
            pending_probe_sequence: prepublication
                .and_then(|control| control.pending_probe)
                .map(|probe| probe.probe_sequence),
            pending_probe_attempt: prepublication
                .and_then(|control| control.pending_probe)
                .map(|probe| probe.producer_attempt),
            pending_probe_deadline_remaining_ms: prepublication
                .and_then(|control| control.pending_probe)
                .map(|probe| {
                    i64::try_from(probe.deadline.saturating_duration_since(now).as_millis())
                        .unwrap_or(i64::MAX)
                }),
            last_probe_outcome: prepublication
                .map_or(ProducerProbeOutcome::None, |control| {
                    control.last_probe_outcome
                })
                .status(),
            producer_ended_with_proposal,
            proposal: retained_proposal.map(|proposal| RollingProducerProposalSnapshot {
                proposal_id: proposal.proposal_id.clone(),
                kind: proposal.kind,
                reason: proposal.reason.status(),
                source: proposal.source,
                severity: proposal.severity,
            }),
            completion: completion.status(),
            completion_attempt,
            completion_final_segment,
            completion_final_end_ms,
        }
    }

    fn deadline(&self) -> Instant {
        self.last_renewal
            .checked_add(self.mode.timeout())
            .unwrap_or(self.last_renewal)
    }

    fn sync_install_authorization(&self, transition: &mut RollingProducerTransitionFence) {
        transition.install_authorization = (!self.executor_loss_cutoff_pending)
            .then_some(self.authorized_install)
            .flatten();
        transition.producer_signal_authorized = self.producer_signal_authorized;
    }

    fn producer_progress_budget(&self) -> Duration {
        self.prepublication
            .as_ref()
            .and_then(|control| control.initial_policy.as_ref())
            .map_or(PRODUCER_PROGRESS_BUDGET, |policy| policy.progress_budget)
    }

    fn next_deadline(&self) -> Instant {
        let deadline = self.producer_progress_deadline.map_or_else(
            || self.deadline(),
            |producer| self.deadline().min(producer.instant),
        );
        // Without this the action deadline would only be noticed the next time
        // something else happened to wake the actor — which, for a signal that
        // never comes back, is exactly never.
        self.pending_producer_action
            .map_or(deadline, |action| deadline.min(action.deadline))
    }

    /// A retry is installed while its predecessor's retained Retry decision
    /// still awaits DecisionApplied. That exact N -> N+1 overlap is not a
    /// second decision owner: the successor must retain its own startup,
    /// progress, and exit evidence until the executor acknowledges the
    /// predecessor decision.
    fn pending_retry_precedes_successor(&self, producer_attempt: u64) -> bool {
        self.delivery.producer_attempt == producer_attempt
            && self.pending_decision.as_deref().is_some_and(|decision| {
                matches!(
                    decision,
                    ProducerDecision::Retry { failed_attempt, .. }
                        if failed_attempt.checked_add(1) == Some(producer_attempt)
                )
            })
    }

    fn arm_producer_deadline(
        &mut self,
        producer_attempt: u64,
        mode: ProducerProgressDeadlineMode,
        instant: Instant,
    ) {
        let pending_retry_successor = self.pending_retry_precedes_successor(producer_attempt);
        if self.retired
            || self.has_terminal_prepublication_failure()
            || (self.pending_decision.is_some() && !pending_retry_successor)
            || self.prepublication.as_ref().is_some_and(|control| {
                !matches!(control.completion, ProducerCompletionState::Incomplete)
            })
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
    /// Settle one lapsed actor-issued physical transaction.
    ///
    /// Priority is exact: this is evaluated before the progress deadline, so
    /// it wins a same-instant tie with running progress. It is not a second
    /// progress verdict — it fires only when a signal the actor itself
    /// authorized never came back, and it records the bounded action failure
    /// without changing a decision that already exists.
    fn settle_producer_action_deadline_at(&mut self, now: Instant) -> bool {
        let Some(action) = self
            .pending_producer_action
            .filter(|action| now >= action.deadline)
        else {
            return false;
        };
        // A preempted action's deadline is stale and observation-only: the
        // decision or terminal transition that took the slot already advanced
        // the sequence.
        if action.producer_attempt != self.delivery.producer_attempt {
            self.pending_producer_action = None;
            return false;
        }
        self.pending_producer_action = None;
        self.producer_action_sequence = self.producer_action_sequence.saturating_add(1);
        self.producer_flow_signal_outstanding = false;
        // Fence the signal token before requesting anything else, so a late
        // physical acknowledgement cannot revive or relabel the producer.
        self.producer_signal_authorized = false;
        let (kind_index, _) = action.kind.metric();
        ROLLING_PRODUCER_ACTION_DEADLINES[kind_index].fetch_add(1, Ordering::Relaxed);
        if self.pending_decision.is_none() {
            let _ = self.commit_producer_decision_at(
                now,
                action.producer_attempt,
                action.kind.deadline_reason(),
            );
        }
        true
    }

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
    /// session may record a producer deadline; legacy actors retain it
    /// passively while opted-in actor-managed producers may settle a decision.
    #[cfg(test)]
    fn settle_due_deadlines_at(&mut self, now: Instant) -> Option<ProducerDeadlineDue> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return None;
        }
        if self.settle_producer_action_deadline_at(now) {
            return None;
        }
        let due = self.settle_producer_deadline_at(now);
        let _ = self.maybe_commit_producer_decision_at(now);
        due
    }

    /// Record one desired physical-flow state on the shared actor sequence.
    ///
    /// This is ordering only. It never touches `producer_physical_flow` or the
    /// progress deadline: per the M4 contract, requesting a hold does not
    /// change the running deadline, and a resume arms nothing when requested
    /// or queued — only the exact-attempt supervisor's successful
    /// acknowledgement does either. What it does own is *who may signal*: a
    /// desire arriving while a signal is outstanding coalesces into the newest
    /// revision instead of racing a second syscall against it.
    fn apply_producer_flow_intention_at(
        &mut self,
        now: Instant,
        producer_attempt: u64,
        desired: ProducerPhysicalFlowState,
    ) -> ProducerFlowIntentionOutcome {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live)
            || self.retired
            || !self.producer_signal_authorized
            || producer_attempt != self.delivery.producer_attempt
        {
            return ProducerFlowIntentionOutcome::Rejected;
        }
        if desired == self.producer_flow_intention.desired
            && !self.producer_flow_signal_outstanding
            && desired == self.producer_physical_flow
        {
            return ProducerFlowIntentionOutcome::Settled;
        }
        let revision = self.producer_flow_intention.revision.saturating_add(1);
        self.producer_flow_intention = ProducerFlowIntention { revision, desired };
        if self.producer_flow_signal_outstanding {
            return ProducerFlowIntentionOutcome::Coalesced;
        }
        if desired == self.producer_physical_flow {
            return ProducerFlowIntentionOutcome::Settled;
        }
        self.producer_flow_signal_outstanding = true;
        let hold = matches!(desired, ProducerPhysicalFlowState::Held);
        // One nonterminal action at a time, bounded from the moment the actor
        // authorizes it. Nothing else arms this: an intention that is
        // coalesced, settled or rejected issues no signal and owes no
        // acknowledgement.
        self.producer_action_sequence = self.producer_action_sequence.saturating_add(1);
        self.pending_producer_action = Some(PendingProducerAction {
            action_sequence: self.producer_action_sequence,
            kind: if hold {
                ProducerActionKind::SignalStop
            } else {
                ProducerActionKind::SignalResume
            },
            producer_attempt,
            deadline: now.checked_add(PRODUCER_FLOW_ACTION_BUDGET).unwrap_or(now),
        });
        ProducerFlowIntentionOutcome::Issue { revision, hold }
    }

    /// Release the outstanding-signal claim after one signal attempt finishes,
    /// and report whether the desire still diverges from the applied state.
    ///
    /// Called for a failed syscall as well as a successful one: a failure
    /// publishes no barrier, so without this the actor would believe a signal
    /// were forever in flight and never issue again.
    fn settle_producer_flow_signal(&mut self, producer_attempt: u64) -> bool {
        if producer_attempt != self.delivery.producer_attempt {
            return false;
        }
        self.producer_flow_signal_outstanding = false;
        self.pending_producer_action = None;
        self.producer_signal_authorized
            && !self.retired
            && self.producer_flow_intention.desired != self.producer_physical_flow
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
        // The acknowledgement ends this signal's outstanding claim. A desire
        // recorded while it was in flight is applied next, by the caller that
        // observes the divergence — not from inside this fold.
        self.producer_flow_signal_outstanding = false;
        self.pending_producer_action = None;
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
                            .checked_add(self.producer_progress_budget())
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

    /// The capability document this session is holding.
    ///
    /// Read this, never the live snapshot's — see the field's own doc. M6's
    /// preparation decision takes exactly this value, because a capability
    /// that reads `None` from sequence 2 onward is a capability that refuses
    /// every transition a capable client ever makes.
    #[cfg_attr(not(test), allow(dead_code))]
    fn retained_capabilities(&self) -> Option<&DynamicCapabilities> {
        self.retained_capabilities.as_ref()
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
                // A terminal replay is not a transition by construction.
                selection_changed: false,
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
        // Delegated to `ControlState` rather than compared against
        // `self.demand`, because both delivery engines hold a `ControlState`
        // and only this one has an actor — see `ControlState::last_selection`.
        // Only for an accepted exchange: a replay is the same exchange
        // arriving twice.
        let selection_changed = disposition == ControlDisposition::Accepted
            && self.control.observe_selection(&request.snapshot.selection);
        if disposition == ControlDisposition::Accepted {
            self.mode = RollingLeaseMode::Explicit;
            self.settled_target = Some(SettledTarget {
                sequence: accepted_sequence,
                anchor_ms: request.snapshot.buffer_anchor_ms(),
            });
            // Retained before the snapshot is moved, and only over `Some`:
            // an exchange that omits capabilities is one that has already
            // told us, not one that has changed its mind.
            if let Some(capabilities) = &request.snapshot.capabilities {
                self.retained_capabilities = Some(capabilities.clone());
            }
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
            selection_changed,
        })
    }

    fn begin_producer_attempt_with_budget_at(
        &mut self,
        now: Instant,
        startup_budget: Duration,
        exit_classifier: ProducerExitClassifier,
    ) -> Result<u64, ProducerAttemptRejection> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        let attempt = self
            .delivery
            .producer_attempt
            .checked_add(1)
            .ok_or(ProducerAttemptRejection::AttemptExhausted)?;
        self.authorized_install = None;
        self.delivery = RollingDeliverySnapshot {
            producer_attempt: attempt,
            ..RollingDeliverySnapshot::default()
        };
        self.producer_progress_at = Some(now);
        self.producer_exit_at = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.producer_exit_classifier = exit_classifier;
        self.copy_producer_exit_classification = None;
        self.producer_physical_flow = ProducerPhysicalFlowState::Running;
        // Desired flow is attempt-scoped. A predecessor's outstanding desire
        // must not survive into its successor and signal a different child.
        self.producer_flow_intention = ProducerFlowIntention {
            revision: 0,
            desired: ProducerPhysicalFlowState::Running,
        };
        self.producer_flow_signal_outstanding = false;
        self.pending_producer_action = None;
        self.producer_signal_authorized = true;
        self.arm_producer_deadline(
            attempt,
            ProducerProgressDeadlineMode::Starting,
            now.checked_add(startup_budget).unwrap_or(now),
        );
        self.producer_attempt.store(attempt, Ordering::Release);
        Ok(attempt)
    }

    #[cfg(test)]
    fn begin_producer_attempt_at(&mut self, now: Instant) -> Result<u64, ProducerAttemptRejection> {
        if self.prepublication.is_some() {
            return Err(ProducerAttemptRejection::InvalidPolicy);
        }
        if self.delivery.playlist_ready {
            return Err(ProducerAttemptRejection::PlaylistPublished);
        }
        self.begin_producer_attempt_with_budget_at(
            now,
            PRODUCER_STARTUP_BUDGET,
            ProducerExitClassifier::Immediate,
        )
    }

    fn register_producer_executor_at(&mut self) -> Result<(), ProducerAttemptRejection> {
        if self.retired {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        let control = self
            .prepublication
            .as_mut()
            .ok_or(ProducerAttemptRejection::InvalidPolicy)?;
        if self.executor_lost {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        if control.executor_registered {
            return Ok(());
        }
        if !self.decision_wake.executor_observation.register() {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        control.executor_registered = true;
        Ok(())
    }

    fn begin_initial_producer_attempt_at(
        &mut self,
        now: Instant,
        policy: InitialProducerPolicy,
    ) -> Result<u64, ProducerAttemptRejection> {
        policy.validate()?;
        let control = self
            .prepublication
            .as_ref()
            .ok_or(ProducerAttemptRejection::InvalidPolicy)?;
        if self
            .response_publication_contract
            .as_ref()
            .is_some_and(|contract| {
                contract.presentation_contract_fingerprint
                    != policy.presentation_contract_fingerprint
            })
        {
            return Err(ProducerAttemptRejection::PresentationContractMismatch);
        }
        if self.response_failure_fenced() {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if !control.executor_registered {
            return Err(ProducerAttemptRejection::ExecutorNotRegistered);
        }
        if self.executor_lost {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        if control.initial_policy.is_some() {
            return Err(ProducerAttemptRejection::InitialAttemptAlreadyAdmitted);
        }
        self.producer_events
            .set_progress_budget(policy.progress_budget);
        let attempt = self.begin_producer_attempt_with_budget_at(
            now,
            policy.startup_kind.startup_budget(),
            policy.exit_classifier,
        )?;
        let retry_state = policy.retry_recipe.clone().map_or(
            PrepublicationRetryState::Unavailable,
            PrepublicationRetryState::Available,
        );
        let control = self
            .prepublication
            .as_mut()
            .expect("pre-publication scope checked before attempt admission");
        control.retry_state = retry_state;
        control.initial_policy = Some(policy);
        Ok(attempt)
    }

    fn poll_producer_decision_at(&self, after_sequence: u64) -> ProducerDecisionPoll {
        // Lifecycle terminal state is a higher-priority observation than a
        // retained decision. Polling never consumes either value.
        if let Some(cause) = self.terminal {
            return ProducerDecisionPoll::Terminal(cause);
        }
        if let Some(decision) = self
            .pending_decision
            .as_ref()
            .filter(|decision| decision.decision_sequence() > after_sequence)
            .cloned()
        {
            return ProducerDecisionPoll::Decision(decision);
        }
        self.prepublication
            .as_ref()
            .and_then(|control| control.pending_probe)
            .map_or(
                ProducerDecisionPoll::Idle,
                ProducerDecisionPoll::ClassifyExit,
            )
    }

    fn producer_failure_reason(&self) -> Option<(u64, ProducerDecisionReason)> {
        if let Some(exit) = self.producer_process_exit_due {
            return Some((exit.producer_attempt, ProducerDecisionReason::ProcessExit));
        }
        self.producer_deadline_due.map(|due| {
            let reason = match due.mode {
                ProducerProgressDeadlineMode::Starting => ProducerDecisionReason::StartupDeadline,
                ProducerProgressDeadlineMode::Advancing => ProducerDecisionReason::ProgressDeadline,
                ProducerProgressDeadlineMode::ClassifyingExit => {
                    ProducerDecisionReason::ExitClassificationDeadline
                }
            };
            (due.producer_attempt, reason)
        })
    }

    fn commit_producer_decision_at(
        &mut self,
        committed_at: Instant,
        failed_attempt: u64,
        mut reason: ProducerDecisionReason,
    ) -> bool {
        if self.retired
            || self.pending_decision.is_some()
            || failed_attempt == 0
            || failed_attempt != self.delivery.producer_attempt
        {
            return false;
        }
        let Some(control) = self.prepublication.as_mut() else {
            return false;
        };
        if control.failure_applied
            || !matches!(control.completion, ProducerCompletionState::Incomplete)
        {
            return false;
        }
        let Some(policy) = control.initial_policy.as_ref() else {
            return false;
        };
        let decision_sequence = control.next_decision_sequence;
        control.next_decision_sequence = control.next_decision_sequence.saturating_add(1);

        let producer_media_published = control.producer_media_published;
        let retry_recipe = match &control.retry_state {
            PrepublicationRetryState::Available(recipe)
                if !producer_media_published
                    && !self.executor_lost
                    && policy.retry_eligibility.allows(reason)
                    && recipe.presentation_contract_fingerprint
                        == policy.presentation_contract_fingerprint =>
            {
                Some(recipe.clone())
            }
            PrepublicationRetryState::Available(recipe)
                if recipe.presentation_contract_fingerprint
                    != policy.presentation_contract_fingerprint =>
            {
                reason = ProducerDecisionReason::InvalidConfiguration;
                None
            }
            _ => None,
        };
        let decision = if let Some(recipe) = retry_recipe {
            control.retry_state = PrepublicationRetryState::Reserved {
                decision_sequence,
                recipe: recipe.clone(),
            };
            ProducerDecision::Retry {
                decision_sequence,
                failed_attempt,
                recipe,
                reason,
            }
        } else {
            if matches!(&control.retry_state, PrepublicationRetryState::Available(_)) {
                control.retry_state = PrepublicationRetryState::Consumed;
            }
            ProducerDecision::Fail {
                decision_sequence,
                failed_attempt,
                reason,
                proposal: producer_media_published.then(|| ActionProposal {
                    proposal_id: uuid::Uuid::new_v4().to_string(),
                    kind: "replace_failed_producer",
                    reason,
                    source: "server",
                    severity: "required",
                }),
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: if producer_media_published {
                        CleanupPolicy::RetainPublished
                    } else {
                        CleanupPolicy::DiscardPrepublication
                    },
                },
            }
        };
        self.producer_progress_deadline = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.authorized_install = None;
        self.producer_signal_authorized = false;
        if control.pending_probe.take().is_some() {
            control.last_probe_outcome = ProducerProbeOutcome::Cancelled;
        }
        let decision = Arc::new(decision);
        self.last_decision = Some(Arc::clone(&decision));
        self.pending_decision = Some(decision);
        self.decision_committed_at = Some(committed_at);
        self.decision_wake.executor_observation.queue_decision();
        true
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Record that a successor has been staged.
    ///
    /// Refused when the slot is occupied. One preparation per playback is the
    /// store's invariant and the ledger's primary key enforces it; refusing
    /// here as well means the actor never believes in a second successor the
    /// store would reject, which is what would let a commit name the wrong one.
    fn stage_preparation(
        &mut self,
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
    ) -> bool {
        if self.retired || self.terminal.is_some() {
            return false;
        }
        if !matches!(self.preparation, PreparationSlot::Empty) {
            return false;
        }
        self.preparation = PreparationSlot::Staged {
            staged_incarnation_id,
            predecessor_incarnation_id,
        };
        true
    }

    /// Move the slot to aborting, and say whether this call is the one that
    /// moved it.
    ///
    /// `false` covers an empty slot, a different successor, and an abort
    /// already under way. None of those is an error: an owner retrying after a
    /// crash must read back the same outcome, and a stale executor must not be
    /// able to tear down a successor that replaced the one it knew about.
    fn abort_preparation(&mut self, staged_incarnation_id: &str) -> bool {
        if !self.preparation.may_abort(staged_incarnation_id) {
            return false;
        }
        let already = matches!(self.preparation, PreparationSlot::Aborting { .. });
        self.preparation = PreparationSlot::Aborting {
            staged_incarnation_id: staged_incarnation_id.to_owned(),
        };
        !already
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Clear the slot once its durable outcome is known.
    ///
    /// Called for a committed successor and for a completed abort alike: after
    /// either, this playback has no staged generation. Commit's own CAS lives
    /// in the store, and its `Ok(None)` — the pointer no longer names the
    /// recorded predecessor — reaches the actor as an abort rather than a
    /// commit, which is the rule that keeps a lost race from reaping a newer
    /// player generation.
    fn settle_preparation(&mut self, staged_incarnation_id: &str) -> bool {
        if self.preparation.staged_incarnation_id() != Some(staged_incarnation_id) {
            return false;
        }
        self.preparation = PreparationSlot::Empty;
        true
    }

    fn maybe_commit_producer_decision_at(&mut self, committed_at: Instant) -> bool {
        let Some((failed_attempt, reason)) = self.producer_failure_reason() else {
            return false;
        };
        self.commit_producer_decision_at(committed_at, failed_attempt, reason)
    }

    fn mark_executor_lost(&mut self, now: Instant) {
        if self.retired
            || self.executor_lost
            || self
                .decision_wake
                .executor_observation
                .is_expected_settled()
        {
            return;
        }
        self.executor_lost = true;
        self.decision_wake.executor_observation.settle_lost();
        let failed_managed_producer = self.prepublication.as_ref().is_some_and(|control| {
            control.initial_policy.is_some()
                && matches!(control.completion, ProducerCompletionState::Incomplete)
        });
        if failed_managed_producer && self.pending_decision.is_none() {
            let attempt = self.delivery.producer_attempt;
            let _ = self.commit_producer_decision_at(
                now,
                attempt,
                ProducerDecisionReason::ExecutorLost,
            );
        }
        if failed_managed_producer {
            if let Some(control) = self.prepublication.as_mut() {
                if matches!(
                    &control.retry_state,
                    PrepublicationRetryState::Available(_)
                        | PrepublicationRetryState::Reserved { .. }
                ) {
                    control.retry_state = PrepublicationRetryState::Consumed;
                }
                control.failure_applied = true;
                control.last_action_failure = Some("executor_lost");
            }
            self.authorized_install = None;
            self.producer_signal_authorized = false;
            self.producer_progress_deadline = None;
            self.producer_deadline_due = None;
            self.producer_process_exit_due = None;
        }
    }

    fn wake_executor_after_transition(
        &self,
        previous_decision_sequence: Option<u64>,
        previous_probe_sequence: Option<u64>,
        was_retired: bool,
    ) {
        let became_terminal = !was_retired && self.retired;
        if became_terminal {
            // Executor loss is useful pre-terminal evidence and wins this
            // status projection. Otherwise terminal settlement is actor truth
            // even if the task disappears between commit and wake delivery.
            self.decision_wake.executor_observation.settle_terminal();
        }
        let current_decision_sequence = self
            .pending_decision
            .as_ref()
            .map(|decision| decision.decision_sequence());
        let current_probe_sequence = self
            .prepublication
            .as_ref()
            .and_then(|control| control.pending_probe)
            .map(|probe| probe.probe_sequence);
        if (current_decision_sequence.is_some()
            && current_decision_sequence != previous_decision_sequence)
            || (current_probe_sequence.is_some()
                && current_probe_sequence != previous_probe_sequence)
            || became_terminal
        {
            self.decision_wake.wake();
        }
    }

    #[cfg(test)]
    fn install_producer_decision_for_test(
        &mut self,
        committed_at: Instant,
        decision: ProducerDecision,
    ) -> bool {
        if self.retired || self.pending_decision.is_some() || decision.decision_sequence() == 0 {
            return false;
        }
        let decision = Arc::new(decision);
        self.last_decision = Some(Arc::clone(&decision));
        self.pending_decision = Some(decision);
        self.decision_committed_at = Some(committed_at);
        true
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
            deadline.producer_attempt == producer_attempt
                && !matches!(deadline.mode, ProducerProgressDeadlineMode::ClassifyingExit)
                && now <= deadline.instant
        });
        if !deadline_accepts_progress {
            let _ = self.settle_producer_deadline_at(now);
        }
        let (accepted, advanced) = self.update_producer_progress_at(now, observation);
        if advanced && deadline_accepts_progress {
            self.arm_producer_deadline(
                producer_attempt,
                ProducerProgressDeadlineMode::Advancing,
                now.checked_add(self.producer_progress_budget())
                    .unwrap_or(now),
            );
        }
        accepted
    }

    /// Start (or retain) the one classification rendezvous clock. The second
    /// half never receives a fresh five seconds: both facts must fit under the
    /// deadline started by whichever exact-attempt fact arrived first.
    fn ensure_exit_classification_deadline_at(
        &mut self,
        published_at: Instant,
        producer_attempt: u64,
    ) -> Option<Instant> {
        if let Some(deadline) = self.producer_progress_deadline.filter(|deadline| {
            deadline.producer_attempt == producer_attempt
                && deadline.mode == ProducerProgressDeadlineMode::ClassifyingExit
        }) {
            return Some(deadline.instant);
        }
        self.producer_progress_deadline = None;
        let deadline = published_at
            .checked_add(PRODUCER_EXIT_CLASSIFICATION_BUDGET)
            .unwrap_or(published_at);
        self.arm_producer_deadline(
            producer_attempt,
            ProducerProgressDeadlineMode::ClassifyingExit,
            deadline,
        );
        self.producer_progress_deadline
            .filter(|armed| {
                armed.producer_attempt == producer_attempt
                    && armed.mode == ProducerProgressDeadlineMode::ClassifyingExit
            })
            .map(|armed| armed.instant)
    }

    fn request_completion_probe(&mut self, producer_attempt: u64, deadline: Instant) {
        let Some(control) = self.prepublication.as_mut() else {
            return;
        };
        if control.pending_probe.is_some() {
            return;
        }
        let probe_sequence = control.next_probe_sequence;
        control.next_probe_sequence = control.next_probe_sequence.saturating_add(1);
        control.pending_probe = Some(RollingProducerExitProbe {
            probe_sequence,
            producer_attempt,
            deadline,
        });
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
        if !observation.success
            || self.producer_exit_classifier == ProducerExitClassifier::Immediate
        {
            self.producer_progress_deadline = None;
        }
        self.producer_signal_authorized = false;
        if self.has_terminal_prepublication_failure()
            || (self.pending_decision.is_some()
                && !self.pending_retry_precedes_successor(observation.producer_attempt))
            || self.prepublication.as_ref().is_some_and(|control| {
                !matches!(control.completion, ProducerCompletionState::Incomplete)
            })
        {
            self.producer_deadline_due = None;
            self.producer_process_exit_due = None;
            return ProducerExitAcceptance::Accepted;
        }
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
            let should_probe = self.producer_exit_classifier == ProducerExitClassifier::Immediate
                || self.copy_producer_exit_classification
                    == Some(CopyProducerExitClassification::Completed);
            let deadline = self
                .ensure_exit_classification_deadline_at(published_at, observation.producer_attempt);
            if let (true, Some(deadline)) = (should_probe, deadline) {
                self.request_completion_probe(observation.producer_attempt, deadline);
            }
        }
        ProducerExitAcceptance::Accepted
    }

    fn classify_copy_producer_exit_at(
        &mut self,
        published_at: Instant,
        producer_attempt: u64,
        classification: CopyProducerExitClassification,
    ) -> Result<(), ProducerAttemptRejection> {
        if self.retired {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if producer_attempt == 0 || producer_attempt != self.delivery.producer_attempt {
            return Err(ProducerAttemptRejection::StaleAttempt);
        }
        if self.producer_exit_classifier != ProducerExitClassifier::CopyReader {
            return Err(ProducerAttemptRejection::ClassifierMismatch);
        }
        if self.copy_producer_exit_classification.is_some() {
            return Err(ProducerAttemptRejection::DuplicateClassification);
        }
        if self.pending_decision.is_some()
            || self
                .producer_deadline_due
                .is_some_and(|due| due.producer_attempt == producer_attempt)
            || self.prepublication.as_ref().is_none_or(|control| {
                control.failure_applied
                    || !matches!(control.completion, ProducerCompletionState::Incomplete)
            })
        {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        if self
            .producer_process_exit_due
            .is_some_and(|due| due.producer_attempt == producer_attempt)
        {
            let _ = self.maybe_commit_producer_decision_at(published_at);
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        if self.producer_progress_deadline.is_some_and(|deadline| {
            deadline.producer_attempt == producer_attempt && published_at > deadline.instant
        }) {
            let _ = self.settle_producer_deadline_at(published_at);
            let _ = self.maybe_commit_producer_decision_at(published_at);
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }

        self.copy_producer_exit_classification = Some(classification);
        let decisive_reason = match classification {
            CopyProducerExitClassification::Unsupported => {
                Some(ProducerDecisionReason::Unsupported)
            }
            CopyProducerExitClassification::InvalidConfiguration => {
                Some(ProducerDecisionReason::InvalidConfiguration)
            }
            CopyProducerExitClassification::ReaderFailed => {
                Some(ProducerDecisionReason::ReaderFailed)
            }
            CopyProducerExitClassification::Completed => None,
        };
        if let Some(reason) = decisive_reason {
            return self
                .commit_producer_decision_at(published_at, producer_attempt, reason)
                .then_some(())
                .ok_or(ProducerAttemptRejection::DecisionMismatch);
        }

        let deadline = self
            .ensure_exit_classification_deadline_at(published_at, producer_attempt)
            .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
        if self
            .delivery
            .producer_exit
            .as_ref()
            .is_some_and(|exit| exit.success)
        {
            self.request_completion_probe(producer_attempt, deadline);
        }
        Ok(())
    }

    fn classify_producer_exit_at(
        &mut self,
        published_at: Instant,
        evidence: RollingProducerCompletionEvidence,
    ) -> RollingProducerCompletionDisposition {
        let matching_probe = self.prepublication.as_ref().and_then(|control| {
            control.pending_probe.filter(|probe| {
                probe.probe_sequence == evidence.probe_sequence
                    && probe.producer_attempt == evidence.producer_attempt
            })
        });
        let exact_success_exit = self.delivery.producer_attempt == evidence.producer_attempt
            && self
                .delivery
                .producer_exit
                .as_ref()
                .is_some_and(|exit| exit.success)
            && self.producer_progress_deadline.is_some_and(|deadline| {
                deadline.producer_attempt == evidence.producer_attempt
                    && deadline.mode == ProducerProgressDeadlineMode::ClassifyingExit
                    && published_at <= deadline.instant
            });
        let Some(probe) = matching_probe
            .filter(|probe| exact_success_exit && published_at <= probe.deadline && !self.retired)
        else {
            if let Some(control) = self.prepublication.as_mut() {
                if control.pending_probe.is_some()
                    && matches!(control.completion, ProducerCompletionState::Incomplete)
                    && !control.failure_applied
                {
                    control.last_probe_outcome = ProducerProbeOutcome::Stale;
                }
            }
            return RollingProducerCompletionDisposition::Stale.record();
        };

        let (expected_remaining_ms, completion_tolerance_ms) = self
            .prepublication
            .as_ref()
            .and_then(|control| control.initial_policy.as_ref())
            .map(|policy| (policy.expected_remaining_ms, policy.completion_tolerance_ms))
            .expect("a pending completion probe requires an admitted producer policy");
        let frontier = evidence.final_segment.zip(evidence.final_end_ms).filter(
            |(final_segment, final_end_ms)| {
                *final_segment >= 0
                    && *final_end_ms > 0
                    && self
                        .delivery
                        .published_segment
                        .is_none_or(|published| *final_segment >= published)
                    && self
                        .delivery
                        .published_end_ms
                        .is_none_or(|published| *final_end_ms >= published)
            },
        );
        if let Some((final_segment, final_end_ms)) = frontier {
            self.delivery.playlist_ready |= evidence.end_list;
            self.delivery.published_segment = Some(
                self.delivery
                    .published_segment
                    .map_or(final_segment, |current| current.max(final_segment)),
            );
            self.delivery.published_end_ms = Some(
                self.delivery
                    .published_end_ms
                    .map_or(final_end_ms, |current| current.max(final_end_ms)),
            );
        }
        let completion =
            frontier
                .filter(|_| evidence.end_list)
                .and_then(
                    |(final_segment, final_end_ms)| match expected_remaining_ms {
                        Some(expected) => (final_end_ms.abs_diff(expected)
                            <= u64::try_from(completion_tolerance_ms).unwrap_or(u64::MAX))
                        .then_some((
                            RollingProducerCompletionDisposition::CompleteVerifiedDuration,
                            ProducerCompletionState::CompleteVerifiedDuration {
                                producer_attempt: evidence.producer_attempt,
                                final_segment,
                                final_end_ms,
                            },
                        )),
                        None => Some((
                            RollingProducerCompletionDisposition::CompleteUnverifiedDuration,
                            ProducerCompletionState::CompleteUnverifiedDuration {
                                producer_attempt: evidence.producer_attempt,
                                final_segment,
                                final_end_ms,
                            },
                        )),
                    },
                );

        let control = self
            .prepublication
            .as_mut()
            .expect("matching completion probe requires producer control");
        debug_assert_eq!(control.pending_probe, Some(probe));
        control.pending_probe = None;
        control.last_probe_outcome = ProducerProbeOutcome::Published;
        self.producer_progress_deadline = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.producer_signal_authorized = false;
        self.authorized_install = None;
        self.decision_wake.executor_observation.acknowledge();

        if let Some((disposition, completion)) = completion {
            control.completion = completion;
            return disposition.record();
        }
        let _ = self.commit_producer_decision_at(
            published_at,
            evidence.producer_attempt,
            ProducerDecisionReason::PartialSuccessExit,
        );
        RollingProducerCompletionDisposition::FailedPartialSuccess.record()
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

    fn fold_producer_blocks_at(&mut self, now: Instant, blocks: Vec<RollingProducerIngressBlock>) {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            for block in blocks {
                self.handle_producer_block_at(now, block);
            }
            return;
        }
        for block in blocks {
            self.handle_producer_block_at(now, block);
        }
    }

    fn handle_producer_blocks_at(
        &mut self,
        now: Instant,
        blocks: Vec<RollingProducerIngressBlock>,
    ) {
        self.fold_producer_blocks_at(now, blocks);
        // Priority is exact: a lapsed action wins a same-instant tie with
        // running progress, so it is evaluated first. This is the live path —
        // it runs on every actor tick, including the timer wake that
        // `next_deadline` now schedules for a signal that never came back.
        if self.settle_producer_action_deadline_at(now) {
            return;
        }
        let _ = self.settle_producer_deadline_at(now);
        let _ = self.maybe_commit_producer_decision_at(now);
    }

    #[cfg(test)]
    fn run_due_first_cutoff_at(&mut self, now: Instant) {
        let transition = Arc::clone(&self.producer_transition);
        let mut transition = transition
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
        self.sync_install_authorization(&mut transition);
        drop(ingress);
        drop(transition);
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
    ) -> Result<ProducerInstallAuthorization, ProducerAttemptRejection> {
        if producer_attempt != self.delivery.producer_attempt {
            return Err(ProducerAttemptRejection::StaleAttempt);
        }
        if self.executor_lost {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        if let Some(control) = self.prepublication.as_ref() {
            if control.failure_applied || control.producer_media_published {
                return Err(ProducerAttemptRejection::DecisionMismatch);
            }
            match self.pending_decision.as_deref() {
                Some(ProducerDecision::Retry {
                    decision_sequence,
                    failed_attempt,
                    ..
                }) => {
                    let admitted_successor =
                        control.retry_admission.as_ref().is_some_and(|admission| {
                            admission.decision_sequence == *decision_sequence
                                && admission.installed_attempt == producer_attempt
                        });
                    let failed_attempt_pending_manager_publication = *failed_attempt
                        == producer_attempt
                        && control.retry_admission.is_none()
                        && matches!(
                            &control.retry_state,
                            PrepublicationRetryState::Reserved {
                                decision_sequence: reserved_sequence,
                                ..
                            } if reserved_sequence == decision_sequence
                        );
                    if !admitted_successor && !failed_attempt_pending_manager_publication {
                        return Err(ProducerAttemptRejection::DecisionMismatch);
                    }
                }
                Some(ProducerDecision::Fail { .. }) => {
                    return Err(ProducerAttemptRejection::DecisionMismatch);
                }
                None => {}
            }
        }
        let revision = self.next_install_revision;
        let next_install_revision = self
            .next_install_revision
            .checked_add(1)
            .ok_or(ProducerAttemptRejection::AttemptExhausted)?;
        if !self.renew_at(now, "producer-install", RollingRenewalSource::Internal) {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        self.next_install_revision = next_install_revision;
        let coordinate = ProducerInstallCoordinate {
            revision,
            producer_attempt,
            producer_deadline: self
                .producer_progress_deadline
                .filter(|deadline| deadline.producer_attempt == producer_attempt)
                .map(|deadline| deadline.instant),
        };
        self.authorized_install = Some(coordinate);
        Ok(ProducerInstallAuthorization { coordinate })
    }

    fn bind_response_publication_contract_at(
        &mut self,
        presentation_contract_fingerprint: String,
        failure_fence: Arc<AtomicBool>,
    ) -> Result<(), ResponsePublicationRejection> {
        if self.retired {
            return Err(ResponsePublicationRejection::SessionEnded);
        }
        if self
            .prepublication
            .as_ref()
            .is_some_and(|control| control.initial_policy.is_some())
        {
            return Err(ResponsePublicationRejection::InvalidBinding);
        }
        match self.response_publication_contract.as_ref() {
            Some(contract)
                if contract.presentation_contract_fingerprint
                    == presentation_contract_fingerprint
                    && Arc::ptr_eq(&contract.failure_fence, &failure_fence) =>
            {
                Ok(())
            }
            Some(contract)
                if contract.presentation_contract_fingerprint
                    != presentation_contract_fingerprint =>
            {
                Err(ResponsePublicationRejection::PresentationContractMismatch)
            }
            Some(_) => Err(ResponsePublicationRejection::InvalidBinding),
            None => {
                self.response_publication_contract = Some(RollingResponsePublicationContract {
                    presentation_contract_fingerprint,
                    failure_fence,
                    metadata_response_authorized: false,
                });
                Ok(())
            }
        }
    }

    fn response_presentation_contract_fingerprint(&self) -> Option<&str> {
        match self.prepublication.as_ref() {
            Some(control) => control
                .initial_policy
                .as_ref()
                .map(|policy| policy.presentation_contract_fingerprint.as_str()),
            None => self
                .response_publication_contract
                .as_ref()
                .map(|contract| contract.presentation_contract_fingerprint.as_str()),
        }
    }

    fn response_failure_fenced(&self) -> bool {
        self.response_publication_contract
            .as_ref()
            .is_some_and(|contract| contract.failure_fence.load(Ordering::Acquire))
    }

    fn authorize_response_publication_at(
        &mut self,
        now: Instant,
        publication: RollingResponsePublication,
    ) -> Result<RollingResponseAuthorization, ResponsePublicationRejection> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return Err(ResponsePublicationRejection::SessionEnded);
        }
        if self.response_failure_fenced() {
            return Err(ResponsePublicationRejection::SessionEnded);
        }
        let Some(expected_presentation_contract_fingerprint) = self
            .response_presentation_contract_fingerprint()
            .map(str::to_owned)
        else {
            return Err(ResponsePublicationRejection::ProducerNotAdmitted);
        };
        if self.has_terminal_prepublication_failure() {
            return Err(ResponsePublicationRejection::DecisionCommitted);
        }
        match publication.binding {
            RollingResponsePublicationBinding::GenerationMetadata {
                presentation_contract_fingerprint,
            } => {
                if publication.object != RollingResponseObject::MasterPlaylist {
                    return Err(ResponsePublicationRejection::InvalidBinding);
                }
                if presentation_contract_fingerprint != expected_presentation_contract_fingerprint {
                    return Err(ResponsePublicationRejection::PresentationContractMismatch);
                }
                if let Some(control) = self.prepublication.as_mut() {
                    control.metadata_response_authorized = true;
                } else if let Some(contract) = self.response_publication_contract.as_mut() {
                    contract.metadata_response_authorized = true;
                }
                Ok(RollingResponseAuthorization {
                    first_producer_media_publication: false,
                })
            }
            RollingResponsePublicationBinding::AttemptStatus { producer_attempt } => {
                if publication.object == RollingResponseObject::ProtocolResponse {
                    return Err(ResponsePublicationRejection::InvalidBinding);
                }
                if producer_attempt != self.delivery.producer_attempt {
                    return Err(ResponsePublicationRejection::StaleAttempt);
                }
                Ok(RollingResponseAuthorization {
                    first_producer_media_publication: false,
                })
            }
            RollingResponsePublicationBinding::ProtocolOnly { producer_attempt } => {
                if publication.object != RollingResponseObject::ProtocolResponse {
                    return Err(ResponsePublicationRejection::InvalidBinding);
                }
                if producer_attempt != self.delivery.producer_attempt {
                    return Err(ResponsePublicationRejection::StaleAttempt);
                }
                if self
                    .prepublication
                    .as_ref()
                    .is_some_and(|control| control.producer_media_published)
                {
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                Ok(RollingResponseAuthorization {
                    first_producer_media_publication: false,
                })
            }
            RollingResponsePublicationBinding::AttemptMedia {
                producer_attempt,
                media_segment_index,
            } => {
                if publication.object == RollingResponseObject::ProtocolResponse {
                    return Err(ResponsePublicationRejection::InvalidBinding);
                }
                let valid_segment_binding = match publication.object {
                    RollingResponseObject::MediaSegment => {
                        media_segment_index.is_some_and(|index| index >= 0)
                    }
                    RollingResponseObject::ByteRange | RollingResponseObject::NotModified => {
                        media_segment_index.is_none_or(|index| index >= 0)
                    }
                    _ => media_segment_index.is_none(),
                };
                if !valid_segment_binding {
                    return Err(ResponsePublicationRejection::InvalidBinding);
                }
                if producer_attempt != self.delivery.producer_attempt {
                    return Err(ResponsePublicationRejection::StaleAttempt);
                }
                if self.prepublication.is_none() {
                    return Ok(RollingResponseAuthorization {
                        first_producer_media_publication: false,
                    });
                }
                if self.pending_decision.is_some()
                    && self
                        .prepublication
                        .as_ref()
                        .is_none_or(|control| !control.producer_media_published)
                {
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                // RetainPublished keeps exact bytes at or behind the frozen
                // frontier readable, but a playlist reload is fresh demand for
                // a mutable manifest. If the failure verdict wins this actor
                // transaction, the caller must publish the typed ProducerEnded
                // status instead of replaying a partial EVENT playlist.
                if publication.object == RollingResponseObject::VideoMediaPlaylist
                    && self.has_published_producer_failure_proposal()
                {
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                if self.has_published_producer_failure_proposal()
                    && media_segment_index.is_some_and(|index| {
                        self.delivery
                            .published_segment
                            .is_none_or(|published| index > published)
                    })
                {
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                if self
                    .producer_process_exit_due
                    .is_some_and(|due| due.producer_attempt == producer_attempt)
                {
                    let _ = self.maybe_commit_producer_decision_at(now);
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                if self.producer_progress_deadline.is_some_and(|deadline| {
                    deadline.producer_attempt == producer_attempt && now > deadline.instant
                }) {
                    let _ = self.settle_producer_deadline_at(now);
                }
                if self.producer_deadline_due.is_some_and(|due| {
                    due.producer_attempt == producer_attempt && now > due.deadline
                }) {
                    let _ = self.maybe_commit_producer_decision_at(now);
                    return Err(ResponsePublicationRejection::DecisionCommitted);
                }
                let control = self
                    .prepublication
                    .as_mut()
                    .expect("pre-publication policy was present");
                let first_producer_media_publication = !control.producer_media_published;
                control.producer_media_published = true;
                if matches!(&control.retry_state, PrepublicationRetryState::Available(_)) {
                    control.retry_state = PrepublicationRetryState::ClosedByPublication;
                }
                if first_producer_media_publication {
                    self.authorized_install = None;
                }
                Ok(RollingResponseAuthorization {
                    first_producer_media_publication,
                })
            }
        }
    }

    fn admit_producer_retry_at(
        &mut self,
        now: Instant,
        decision_sequence: u64,
        recipe_fingerprint: &str,
    ) -> Result<u64, ProducerAttemptRejection> {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live) {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        let control = self
            .prepublication
            .as_ref()
            .ok_or(ProducerAttemptRejection::RetryUnavailable)?;
        if !control.executor_registered {
            return Err(ProducerAttemptRejection::ExecutorNotRegistered);
        }
        if self.executor_lost {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        if let Some(admission) = &control.retry_admission {
            if admission.decision_sequence == decision_sequence {
                return if admission.recipe_fingerprint == recipe_fingerprint {
                    Ok(admission.installed_attempt)
                } else {
                    Err(ProducerAttemptRejection::DecisionMismatch)
                };
            }
        }
        let pending = self
            .pending_decision
            .as_ref()
            .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
        let ProducerDecision::Retry {
            decision_sequence: pending_sequence,
            failed_attempt,
            recipe,
            ..
        } = pending.as_ref()
        else {
            return Err(ProducerAttemptRejection::RetryUnavailable);
        };
        if *pending_sequence != decision_sequence
            || *failed_attempt != self.delivery.producer_attempt
        {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        if recipe.fingerprint != recipe_fingerprint {
            return Err(ProducerAttemptRejection::RecipeMismatch);
        }
        let policy = control
            .initial_policy
            .as_ref()
            .ok_or(ProducerAttemptRejection::RetryUnavailable)?;
        if recipe.presentation_contract_fingerprint != policy.presentation_contract_fingerprint {
            return Err(ProducerAttemptRejection::PresentationContractMismatch);
        }
        let PrepublicationRetryState::Reserved {
            decision_sequence: reserved_sequence,
            recipe: available_recipe,
        } = &control.retry_state
        else {
            return Err(ProducerAttemptRejection::RetryUnavailable);
        };
        if *reserved_sequence != decision_sequence
            || available_recipe.fingerprint != recipe_fingerprint
            || available_recipe.presentation_contract_fingerprint
                != policy.presentation_contract_fingerprint
        {
            return Err(ProducerAttemptRejection::RecipeMismatch);
        }
        if control.producer_media_published {
            return Err(ProducerAttemptRejection::PlaylistPublished);
        }
        let recipe = recipe.clone();
        let installed_attempt = self.begin_producer_attempt_with_budget_at(
            now,
            recipe.startup_kind.startup_budget(),
            recipe.exit_classifier,
        )?;
        let control = self
            .prepublication
            .as_mut()
            .expect("retry admission remains in pre-publication scope");
        control.retry_state = PrepublicationRetryState::Consumed;
        control.retry_admission = Some(ProducerRetryAdmission {
            decision_sequence,
            recipe_fingerprint: recipe_fingerprint.to_owned(),
            installed_attempt,
        });
        Ok(installed_attempt)
    }

    fn decision_applied_at(
        &mut self,
        decision_sequence: u64,
        installed_attempt: Option<u64>,
    ) -> Result<(), ProducerAttemptRejection> {
        let control = self
            .prepublication
            .as_ref()
            .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
        if let Some(applied) = control.decision_applied {
            if applied.decision_sequence == decision_sequence {
                return (applied.installed_attempt == installed_attempt)
                    .then_some(())
                    .ok_or(ProducerAttemptRejection::DecisionMismatch);
            }
            if decision_sequence < applied.decision_sequence {
                return Err(ProducerAttemptRejection::DecisionMismatch);
            }
        }
        let decision = self
            .pending_decision
            .as_ref()
            .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
        if decision.decision_sequence() != decision_sequence {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        let retry_action_failure = matches!(decision.as_ref(), ProducerDecision::Retry { .. })
            && installed_attempt.is_none();
        let applied_failure =
            matches!(decision.as_ref(), ProducerDecision::Fail { .. }) || retry_action_failure;
        match decision.as_ref() {
            ProducerDecision::Retry { .. } if installed_attempt.is_some() => {
                let admission = control
                    .retry_admission
                    .as_ref()
                    .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
                if installed_attempt != Some(admission.installed_attempt)
                    || admission.decision_sequence != decision_sequence
                {
                    return Err(ProducerAttemptRejection::DecisionMismatch);
                }
            }
            ProducerDecision::Retry { .. } => {}
            ProducerDecision::Fail { .. } if installed_attempt.is_some() => {
                return Err(ProducerAttemptRejection::DecisionMismatch);
            }
            ProducerDecision::Fail { .. } => {}
        }
        self.pending_decision = None;
        self.decision_committed_at = None;
        let control = self
            .prepublication
            .as_mut()
            .expect("decision was retained in pre-publication scope");
        control.decision_applied = Some(ProducerDecisionApplied {
            decision_sequence,
            installed_attempt,
        });
        self.authorized_install = None;
        if applied_failure {
            control.failure_applied = true;
            if matches!(
                &control.retry_state,
                PrepublicationRetryState::Available(_) | PrepublicationRetryState::Reserved { .. }
            ) {
                control.retry_state = PrepublicationRetryState::Consumed;
            }
            if retry_action_failure {
                control.last_action_failure = Some("retry_failed");
            }
            self.producer_progress_deadline = None;
            self.producer_deadline_due = None;
            self.producer_process_exit_due = None;
        }
        self.decision_wake.executor_observation.acknowledge();
        Ok(())
    }

    fn settle_executor_expected_at(&mut self) -> Result<(), ProducerAttemptRejection> {
        if self.retired {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if self.executor_lost {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        let control = self
            .prepublication
            .as_ref()
            .ok_or(ProducerAttemptRejection::DecisionMismatch)?;
        if !control.executor_registered
            || self.pending_decision.is_some()
            || control.pending_probe.is_some()
        {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        let applied_published_failure = control.producer_media_published
            && control.failure_applied
            && control.decision_applied.is_some_and(|applied| {
                self.last_decision.as_ref().is_some_and(|decision| {
                    decision.decision_sequence() == applied.decision_sequence
                        && matches!(
                            decision.as_ref(),
                            ProducerDecision::Fail {
                                proposal: Some(_),
                                cleanup: ProducerFailureCleanup {
                                    cleanup_policy: CleanupPolicy::RetainPublished,
                                    ..
                                },
                                ..
                            }
                        )
                })
            });
        let completed = !matches!(control.completion, ProducerCompletionState::Incomplete);
        if !applied_published_failure && !completed {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        if !self.decision_wake.executor_observation.settle_expected() {
            return Err(ProducerAttemptRejection::ExecutorLost);
        }
        Ok(())
    }

    fn observe_publication_at(
        &mut self,
        now: Instant,
        observation: RollingPublicationObservation,
    ) -> bool {
        if !matches!(self.claim_expiry_at(now), RollingExpiryClaim::Live)
            || observation.producer_attempt != self.delivery.producer_attempt
            // A retained post-publication failure freezes the exact delivery
            // frontier that won before its proposal. An observation already
            // sequenced ahead of that verdict is folded before the decision
            // and still wins; one sealed afterward must not make a rejected
            // playlist expand the media/range authorization surface.
            || self.has_published_producer_failure_proposal()
        {
            return false;
        }
        if let Some((final_segment, final_end_ms)) =
            self.prepublication
                .as_ref()
                .and_then(|control| match control.completion {
                    ProducerCompletionState::Incomplete => None,
                    ProducerCompletionState::CompleteVerifiedDuration {
                        final_segment,
                        final_end_ms,
                        ..
                    }
                    | ProducerCompletionState::CompleteUnverifiedDuration {
                        final_segment,
                        final_end_ms,
                        ..
                    } => Some((final_segment, final_end_ms)),
                })
        {
            if observation
                .published_segment
                .is_some_and(|segment| segment > final_segment)
                || observation
                    .published_end_ms
                    .is_some_and(|end_ms| end_ms > final_end_ms)
                || observation.next_media_sequence > final_segment.saturating_add(1)
                || observation
                    .resolved_fetched_segment
                    .is_some_and(|segment| segment > final_segment)
                || observation
                    .resolved_fetched_end_ms
                    .is_some_and(|end_ms| end_ms > final_end_ms)
            {
                return false;
            }
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
        if self.response_failure_fenced()
            || producer_attempt != self.delivery.producer_attempt
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

    fn commit_generation_metadata_at(
        &mut self,
        now: Instant,
        presentation_contract_fingerprint: &str,
        kind: &'static str,
    ) -> bool {
        if self.has_terminal_prepublication_failure()
            || self.response_failure_fenced()
            || self
                .response_presentation_contract_fingerprint()
                .is_none_or(|expected| expected != presentation_contract_fingerprint)
        {
            return false;
        }
        self.renew_at(now, kind, RollingRenewalSource::Media)
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
        // A disconnect does not imply a commit. Whatever ended this playback --
        // an explicit end, a fence, or the lease simply expiring -- a successor
        // that was staged but never committed must be torn down, and the
        // current generation left authoritative if it is healthy. Moving the
        // slot to `Aborting` here is what makes that true by construction: a
        // commit arriving afterwards finds `may_commit` false and cannot
        // publish a successor the viewer never waited for.
        if let Some(staged) = self.preparation.staged_incarnation_id() {
            let staged = staged.to_owned();
            self.abort_preparation(&staged);
        }
        self.decision_wake
            .terminal_projection
            .store(cause.projection(), Ordering::Release);
        self.expiration_claimed = cause == RollingTerminalCause::LeaseExpired;
        self.producer_progress_deadline = None;
        self.producer_deadline_due = None;
        self.producer_process_exit_due = None;
        self.authorized_install = None;
        self.producer_signal_authorized = false;
        if let Some(control) = self.prepublication.as_mut() {
            if control.pending_probe.take().is_some() {
                control.last_probe_outcome = ProducerProbeOutcome::Cancelled;
            }
        }
        self.pending_decision = None;
        self.decision_committed_at = None;
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
            let previous_decision_sequence = self
                .pending_decision
                .as_ref()
                .map(|decision| decision.decision_sequence());
            let previous_probe_sequence = self
                .prepublication
                .as_ref()
                .and_then(|control| control.pending_probe)
                .map(|probe| probe.probe_sequence);
            let was_retired = self.retired;
            // Some synchronous fail-closed paths (notably a detached
            // first-media settlement that reaches its absolute deadline)
            // publish retirement through the handle-side fence while the
            // actor may still have queued commands. Import that monotone
            // fence before folding or applying any of them so a follower can
            // never revive publication or renew the actor after the timeout.
            // `was_retired` is sampled first so the normal transition wake
            // still reaches the passive executor.
            if self.retired_fence.load(Ordering::Acquire) && !self.retired {
                let _ = self.terminate(RollingTerminalCause::AuthorityFence);
            }
            let publication_may_win_exact_deadline = matches!(
                &command,
                RollingControlCommand::AuthorizeResponsePublication {
                    publication: RollingResponsePublication {
                        binding: RollingResponsePublicationBinding::AttemptMedia { .. },
                        ..
                    },
                    handoff: Some(handoff),
                    deadline,
                    reply,
                    ..
                } if handoff.is_pending() && !reply.is_closed() && rolling_now() < *deadline
            );
            let classification_may_win_exact_deadline = matches!(
                &command,
                RollingControlCommand::ClassifyProducerExit {
                    deadline,
                    reply,
                    ..
                } if !reply.is_closed() && published_at <= *deadline && rolling_now() < *deadline
            ) || matches!(
                &command,
                RollingControlCommand::ClassifyCopyProducerExit {
                    deadline,
                    reply,
                    ..
                } if !reply.is_closed() && published_at <= *deadline && rolling_now() < *deadline
            );
            self.fold_producer_blocks_at(published_at, preceding_producer);
            if !publication_may_win_exact_deadline && !classification_may_win_exact_deadline {
                let _ = self.settle_producer_deadline_at(published_at);
                let _ = self.maybe_commit_producer_decision_at(published_at);
            }
            self.last_applied_ingress_sequence = self.last_applied_ingress_sequence.max(sequence);
            if let Some(metric_index) = command.metric_index() {
                ROLLING_CONTROL_COMMANDS[metric_index].fetch_add(1, Ordering::Relaxed);
            }
            #[cfg(test)]
            let mut deferred_begin_reply: Option<DeferredProducerAttemptReply> = None;
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
                #[cfg(test)]
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
                        deferred_begin_reply = Some(DeferredProducerAttemptReply {
                            reply,
                            outcome,
                            reply_pause,
                        });
                    }
                    #[cfg(not(test))]
                    let _ = reply.send(outcome);
                }
                RollingControlCommand::RegisterProducerExecutor { reply } => {
                    let _ = reply.send(self.register_producer_executor_at());
                }
                RollingControlCommand::BeginInitialProducerAttempt { policy, reply } => {
                    let outcome = self.begin_initial_producer_attempt_at(published_at, policy);
                    let _ = reply.send(outcome);
                }
                RollingControlCommand::PollProducerDecision {
                    after_sequence,
                    reply,
                } => {
                    let _ = reply.send(self.poll_producer_decision_at(after_sequence));
                }
                RollingControlCommand::StagePreparation {
                    staged_incarnation_id,
                    predecessor_incarnation_id,
                    reply,
                } => {
                    let staged =
                        self.stage_preparation(staged_incarnation_id, predecessor_incarnation_id);
                    let _ = reply.send(staged);
                }
                RollingControlCommand::MayCommitPreparation {
                    staged_incarnation_id,
                    reply,
                } => {
                    let _ = reply.send(self.preparation.may_commit(&staged_incarnation_id));
                }
                RollingControlCommand::SettlePreparation {
                    staged_incarnation_id,
                    committed,
                    reply,
                } => {
                    // A commit that lost its CAS is reported here as
                    // `committed: false`, and it must land as an abort rather
                    // than as nothing: the successor still exists durably and
                    // something has to own tearing it down.
                    //
                    // The `Aborting` state this writes is not observable from
                    // outside — the actor is single-threaded, so `settle`
                    // empties the slot in the same turn and no other command
                    // can interleave. It is written anyway because the two
                    // calls are the abort lifecycle's own steps, and a future
                    // settle that must await anything durable would otherwise
                    // open exactly the window this ordering closes. A retry
                    // of the same commit is refused either way: `may_commit`
                    // is false against `Empty` as much as against `Aborting`.
                    if !committed {
                        self.abort_preparation(&staged_incarnation_id);
                    }
                    let settled = self.settle_preparation(&staged_incarnation_id);
                    let _ = reply.send(settled);
                }
                #[cfg(test)]
                RollingControlCommand::InstallProducerDecision { decision, reply } => {
                    let installed = self.install_producer_decision_for_test(published_at, decision);
                    let _ = reply.send(installed);
                }
                RollingControlCommand::AuthorizeProducerInstall {
                    producer_attempt,
                    reply,
                } => {
                    if !reply.is_closed() {
                        let authorized =
                            self.authorize_producer_install_at(published_at, producer_attempt);
                        if authorized.is_ok() {
                            transition.lease_deadline = self.deadline();
                        }
                        let _ = reply.send(authorized);
                    }
                }
                RollingControlCommand::BindResponsePublicationContract {
                    presentation_contract_fingerprint,
                    failure_fence,
                    reply,
                } => {
                    if !reply.is_closed() {
                        let bound = self.bind_response_publication_contract_at(
                            presentation_contract_fingerprint,
                            failure_fence,
                        );
                        let _ = reply.send(bound);
                    }
                }
                RollingControlCommand::AuthorizeResponsePublication {
                    publication,
                    handoff,
                    deadline,
                    reply,
                } => {
                    if reply.is_closed() || rolling_now() >= deadline {
                        if let Some(handoff) = handoff {
                            handoff.settle(false);
                        }
                    } else {
                        let first_media_handoff_required = matches!(
                            &publication.binding,
                            RollingResponsePublicationBinding::AttemptMedia { .. }
                        ) && self
                            .prepublication
                            .as_ref()
                            .is_some_and(|control| !control.producer_media_published);
                        let authorized = if first_media_handoff_required
                            && handoff.as_ref().is_none_or(|handoff| !handoff.is_pending())
                        {
                            Err(ResponsePublicationRejection::InvalidBinding)
                        } else {
                            self.authorize_response_publication_at(published_at, publication)
                        };
                        if let Some(handoff) = handoff {
                            handoff.settle(authorized.as_ref().is_ok_and(|authorization| {
                                authorization.first_producer_media_publication
                            }));
                        }
                        let _ = reply.send(authorized);
                    }
                }
                RollingControlCommand::AdmitProducerRetry {
                    decision_sequence,
                    recipe_fingerprint,
                    reply,
                } => {
                    if !reply.is_closed() {
                        let admitted = self.admit_producer_retry_at(
                            published_at,
                            decision_sequence,
                            &recipe_fingerprint,
                        );
                        let _ = reply.send(admitted);
                    }
                }
                RollingControlCommand::DecisionApplied {
                    decision_sequence,
                    installed_attempt,
                    reply,
                } => {
                    let _ =
                        reply.send(self.decision_applied_at(decision_sequence, installed_attempt));
                }
                RollingControlCommand::ExecutorSettled { reply } => {
                    let _ = reply.send(self.settle_executor_expected_at());
                }
                RollingControlCommand::ClassifyProducerExit {
                    evidence,
                    deadline,
                    reply,
                } => {
                    let disposition = if reply.is_closed() || rolling_now() >= deadline {
                        RollingProducerCompletionDisposition::Stale
                    } else {
                        self.classify_producer_exit_at(published_at, evidence)
                    };
                    let _ = reply.send(disposition);
                }
                RollingControlCommand::ClassifyCopyProducerExit {
                    producer_attempt,
                    classification,
                    deadline,
                    reply,
                } => {
                    let outcome = if reply.is_closed() || rolling_now() >= deadline {
                        Err(ProducerAttemptRejection::ControlUnavailable)
                    } else {
                        self.classify_copy_producer_exit_at(
                            published_at,
                            producer_attempt,
                            classification,
                        )
                    };
                    let _ = reply.send(outcome);
                }
                RollingControlCommand::ObservePublication {
                    observation,
                    deadline,
                    reply,
                } => {
                    let accepted = !reply.is_closed()
                        && deadline.is_none_or(|deadline| rolling_now() < deadline)
                        && self.observe_publication_at(published_at, observation);
                    let _ = reply.send(accepted);
                }
                RollingControlCommand::CommitMedia {
                    kind,
                    producer_attempt,
                    segment_index,
                    segment_end_ms,
                    handoff,
                    deadline,
                    reply,
                } => {
                    let handoff_matches = match (&handoff, segment_index) {
                        (Some(handoff), Some(_)) => {
                            handoff.matches(producer_attempt, segment_index, segment_end_ms)
                        }
                        (None, None) => true,
                        _ => false,
                    };
                    let committed = handoff_matches
                        && !reply.is_closed()
                        && rolling_now() < deadline
                        && self.commit_media_at(
                            published_at,
                            kind,
                            producer_attempt,
                            segment_index,
                            segment_end_ms,
                        );
                    if committed {
                        transition.lease_deadline = self.deadline();
                    }
                    if let Some(handoff) = handoff {
                        handoff.settle(committed);
                    }
                    let _ = reply.send(committed);
                }
                RollingControlCommand::CommitGenerationMetadata {
                    presentation_contract_fingerprint,
                    kind,
                    deadline,
                    reply,
                } => {
                    let committed = !reply.is_closed()
                        && rolling_now() < deadline
                        && self.commit_generation_metadata_at(
                            published_at,
                            &presentation_contract_fingerprint,
                            kind,
                        );
                    if committed {
                        transition.lease_deadline = self.deadline();
                    }
                    let _ = reply.send(committed);
                }
                RollingControlCommand::SettleProducerFlowSignal {
                    producer_attempt,
                    reply,
                } => {
                    let diverged = self.settle_producer_flow_signal(producer_attempt);
                    let _ = reply.send(diverged);
                }
                RollingControlCommand::ApplyProducerFlow {
                    producer_attempt,
                    desired_hold,
                    reply,
                } => {
                    let outcome = self.apply_producer_flow_intention_at(
                        published_at,
                        producer_attempt,
                        if desired_hold {
                            ProducerPhysicalFlowState::Held
                        } else {
                            ProducerPhysicalFlowState::Running
                        },
                    );
                    let _ = reply.send(outcome);
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
            let _ = self.settle_producer_deadline_at(published_at);
            let _ = self.maybe_commit_producer_decision_at(published_at);
            self.sync_install_authorization(&mut transition);
            drop(transition);
            self.producer_events
                .release_sealed_flow_barriers(sealed_flow_barriers);
            #[cfg(test)]
            let deferred_result = deferred_begin_reply;
            #[cfg(not(test))]
            let deferred_result = std::marker::PhantomData::<()>;
            (
                deferred_result,
                previous_decision_sequence,
                previous_probe_sequence,
                was_retired,
            )
        };
        #[cfg(test)]
        let (
            deferred_begin_reply,
            previous_decision_sequence,
            previous_probe_sequence,
            was_retired,
        ) = _deferred_begin_reply;
        #[cfg(not(test))]
        let (_, previous_decision_sequence, previous_probe_sequence, was_retired) =
            _deferred_begin_reply;
        // The actor state is committed and all actor/ingress locks are
        // released before settling/notifying the passive executor.
        self.wake_executor_after_transition(
            previous_decision_sequence,
            previous_probe_sequence,
            was_retired,
        );
        #[cfg(test)]
        if let Some(DeferredProducerAttemptReply {
            reply,
            outcome,
            reply_pause,
        }) = deferred_begin_reply
        {
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
        let mut transition = transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(envelope) = receiver.try_recv() {
            return Some(envelope);
        }
        let producer_events = Arc::clone(&self.producer_events);
        let previous_decision_sequence = self
            .pending_decision
            .as_ref()
            .map(|decision| decision.decision_sequence());
        let previous_probe_sequence = self
            .prepublication
            .as_ref()
            .and_then(|control| control.pending_probe)
            .map(|probe| probe.probe_sequence);
        let was_retired = self.retired;
        let mut ingress = producer_events
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let freed_flow_capacity = !ingress.flow.is_empty();
        let blocks = RollingProducerIngress::take_blocks(&mut ingress);
        let now = rolling_now();
        self.handle_producer_blocks_at(now, blocks);
        self.sync_install_authorization(&mut transition);
        drop(ingress);
        drop(transition);
        if freed_flow_capacity {
            producer_events.flow_capacity_available.notify_waiters();
        }
        self.wake_executor_after_transition(
            previous_decision_sequence,
            previous_probe_sequence,
            was_retired,
        );
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
        #[cfg(test)]
        self.actor_run_started.notify_one();
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
            let executor_wake = self.decision_wake.executor_wake.clone();
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    if let Some(command) = self.command_before_cutoff(&mut receiver) {
                        self.handle_command(command).await;
                    }
                }
                _ = executor_wake.closed(), if !self.executor_lost
                    && !self.decision_wake.executor_observation.is_expected_settled() => {
                    // Executor loss has no sender-side mailbox envelope, so
                    // capture its exact cutoff under the same publication
                    // fence. Commands already sealed before this coordinate,
                    // plus the remaining producer blocks through it, retain
                    // precedence. Publishers released afterward are ordered
                    // behind the loss verdict.
                    let (preceding_commands, preceding_producer, freed_flow_capacity, lost_at) = {
                        let transition = Arc::clone(&self.producer_transition);
                        let mut transition = transition
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        // Revoke the synchronous install surface at the
                        // cutoff itself. Commands already sealed are still
                        // folded before the loss verdict, but neither an old
                        // token nor a reply produced during that fold may
                        // publish a child across observed executor loss.
                        self.executor_loss_cutoff_pending = true;
                        transition.install_authorization = None;
                        let mut preceding_commands =
                            Vec::with_capacity(ROLLING_ACTOR_MAILBOX_CAPACITY);
                        while let Ok(command) = receiver.try_recv() {
                            preceding_commands.push(command);
                        }
                        let producer_events = Arc::clone(&self.producer_events);
                        let mut ingress = producer_events
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let freed_flow_capacity = !ingress.flow.is_empty();
                        let preceding_producer = RollingProducerIngress::take_blocks(&mut ingress);
                        (
                            preceding_commands,
                            preceding_producer,
                            freed_flow_capacity,
                            rolling_now(),
                        )
                    };
                    for command in preceding_commands {
                        self.handle_command(command).await;
                    }
                    {
                        let transition = Arc::clone(&self.producer_transition);
                        let mut transition = transition
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        self.handle_producer_blocks_at(lost_at, preceding_producer);
                        self.mark_executor_lost(lost_at);
                        self.executor_loss_cutoff_pending = false;
                        self.sync_install_authorization(&mut transition);
                    }
                    if freed_flow_capacity {
                        self.producer_events.flow_capacity_available.notify_waiters();
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

    /// Publish one command without allowing mailbox pressure or the
    /// synchronous producer fence to outlive the caller's absolute admission
    /// deadline. Response commands carry the same deadline into actor dispatch;
    /// Terminal instead treats successful enqueue as the irreversible handoff
    /// and waits for settlement without a second deadline.
    async fn enqueue_command_before(
        &self,
        command: RollingControlCommand,
        deadline: Instant,
    ) -> Result<(), RollingCommandAdmissionError> {
        if rolling_now() >= deadline {
            return Err(RollingCommandAdmissionError::Deadline);
        }
        let permit = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sender.reserve(),
        )
        .await
        .map_err(|_| RollingCommandAdmissionError::Deadline)?
        .map_err(|_| RollingCommandAdmissionError::ControlUnavailable)?;
        loop {
            if rolling_now() >= deadline {
                return Err(RollingCommandAdmissionError::Deadline);
            }
            match self.producer_transition.try_lock() {
                Ok(transition) => {
                    if rolling_now() >= deadline {
                        drop(transition);
                        return Err(RollingCommandAdmissionError::Deadline);
                    }
                    let envelope = self.producer_events.seal_command(command);
                    permit.send(envelope);
                    drop(transition);
                    return Ok(());
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    let transition = error.into_inner();
                    if rolling_now() >= deadline {
                        drop(transition);
                        return Err(RollingCommandAdmissionError::Deadline);
                    }
                    let envelope = self.producer_events.seal_command(command);
                    permit.send(envelope);
                    drop(transition);
                    return Ok(());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    // The match temporary may contain a non-Send mutex
                    // guard in its other variants, so end its scope before
                    // yielding back to the runtime.
                }
            }
            tokio::task::yield_now().await;
        }
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

    fn spawn_unbound(
        initial_kind: &'static str,
        prepublication_transcode: bool,
    ) -> (Self, RollingSessionExecutorInbox, Arc<tokio::sync::Notify>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(ROLLING_ACTOR_MAILBOX_CAPACITY);
        let retired = Arc::new(AtomicBool::new(false));
        let producer_attempt = Arc::new(AtomicU64::new(0));
        let now = rolling_now();
        let producer_transition = Arc::new(std::sync::Mutex::new(
            RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
        ));
        let flow_sync = Arc::new(RollingFlowSync::new());
        let producer_events = Arc::new(RollingProducerIngress::new());
        let (executor_wake, executor_inbox) = RollingSessionExecutorInbox::new();
        let executor_observation = Arc::new(RollingExecutorObservation::default());
        let decision_notify = Arc::new(tokio::sync::Notify::new());
        let terminal_projection = Arc::new(AtomicU8::new(0));
        #[cfg(test)]
        let executor_poll_pause = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let executor_observation_pause = Arc::new(std::sync::Mutex::new(None));
        let decision_wake = Arc::new(RollingDecisionWake {
            decision_notify: Arc::clone(&decision_notify),
            executor_wake,
            executor_observation: Arc::clone(&executor_observation),
            terminal_projection: Arc::clone(&terminal_projection),
        });
        let decision_transport = Arc::new(RollingDecisionTransport {
            sender: sender.clone(),
            producer_transition: Arc::clone(&producer_transition),
            producer_events: Arc::clone(&producer_events),
            executor_observation,
            terminal_projection,
            #[cfg(test)]
            executor_poll_pause,
            #[cfg(test)]
            executor_observation_pause,
        });
        #[cfg(test)]
        let producer_attempt_reply_pause = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let actor_exit_fence_started = Arc::new(tokio::sync::Notify::new());
        #[cfg(test)]
        let actor_run_started = Arc::new(tokio::sync::Notify::new());
        let actor = RollingControlActor::with_runtime(
            now,
            initial_kind,
            prepublication_transcode,
            RollingActorRuntime {
                retired_fence: Arc::clone(&retired),
                producer_attempt: Arc::clone(&producer_attempt),
                producer_transition: Arc::clone(&producer_transition),
                flow_sync: Arc::clone(&flow_sync),
                producer_events: Arc::clone(&producer_events),
                decision_wake,
                #[cfg(test)]
                producer_attempt_reply_pause: Arc::clone(&producer_attempt_reply_pause),
                #[cfg(test)]
                actor_run_started: Arc::clone(&actor_run_started),
                #[cfg(test)]
                actor_exit_fence_started: Arc::clone(&actor_exit_fence_started),
            },
        );
        let actor_task = tokio::spawn(actor.run(receiver));
        #[cfg(test)]
        let actor_abort = Some(actor_task.abort_handle());
        drop(actor_task);
        let handle = Self {
            sender,
            retired,
            producer_attempt,
            producer_transition,
            flow_sync,
            producer_events,
            decision_transport,
            #[cfg(test)]
            producer_attempt_reply_pause,
            #[cfg(test)]
            actor_abort,
            #[cfg(test)]
            executor_abort: None,
            #[cfg(test)]
            actor_run_started,
            #[cfg(test)]
            actor_exit_fence_started,
        };
        (handle, executor_inbox, decision_notify)
    }

    /// Preserve the merged legacy action-passive behavior. Its internal
    /// observer registers synchronously before the actor can arm a legacy
    /// attempt, and it never acknowledges or executes a producer decision.
    pub(crate) fn spawn(initial_kind: &'static str) -> Self {
        let (handle, executor_inbox, decision_notify) = Self::spawn_unbound(initial_kind, false);
        #[cfg(test)]
        let mut handle = handle;
        let _ = handle.decision_transport.executor_observation.register();
        let executor_task = handle.decision_transport.spawn_executor(
            Arc::downgrade(&handle.decision_transport),
            decision_notify,
            executor_inbox,
        );
        #[cfg(test)]
        {
            handle.executor_abort = Some(executor_task.abort_handle());
        }
        drop(executor_task);
        handle
    }

    pub(crate) fn spawn_prepublication_producer(
        initial_kind: &'static str,
    ) -> (Self, RollingProducerExecutorRegistration) {
        let (handle, inbox, _decision_notify) = Self::spawn_unbound(initial_kind, true);
        let registration = RollingProducerExecutorRegistration {
            transport: Arc::downgrade(&handle.decision_transport),
            terminal_projection: Arc::clone(&handle.decision_transport.terminal_projection),
            inbox,
            registered: false,
        };
        (handle, registration)
    }

    pub(crate) fn spawn_prepublication_transcode(
        initial_kind: &'static str,
    ) -> (Self, RollingProducerExecutorRegistration) {
        Self::spawn_prepublication_producer(initial_kind)
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(ROLLING_ACTOR_MAILBOX_CAPACITY);
        drop(receiver);
        let now = rolling_now();
        let producer_transition = Arc::new(std::sync::Mutex::new(
            RollingProducerTransitionFence::new(now + ROLLING_LEGACY_LEASE_TIMEOUT),
        ));
        let producer_events = Arc::new(RollingProducerIngress::new());
        let decision_transport = Arc::new(RollingDecisionTransport {
            sender: sender.clone(),
            producer_transition: Arc::clone(&producer_transition),
            producer_events: Arc::clone(&producer_events),
            executor_observation: Arc::new(RollingExecutorObservation::default()),
            terminal_projection: Arc::new(AtomicU8::new(0)),
            executor_poll_pause: Arc::new(std::sync::Mutex::new(None)),
            executor_observation_pause: Arc::new(std::sync::Mutex::new(None)),
        });
        Self {
            sender,
            retired: Arc::new(AtomicBool::new(false)),
            producer_attempt: Arc::new(AtomicU64::new(0)),
            producer_transition,
            flow_sync: Arc::new(RollingFlowSync::new()),
            producer_events,
            decision_transport,
            producer_attempt_reply_pause: Arc::new(std::sync::Mutex::new(None)),
            actor_abort: None,
            executor_abort: None,
            actor_run_started: Arc::new(tokio::sync::Notify::new()),
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

    #[cfg(test)]
    pub(crate) async fn begin_producer_attempt(&self) -> Result<u64, ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::BeginProducerAttempt { reply })
            .await
            .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    pub(crate) async fn begin_initial_producer_attempt(
        &self,
        policy: InitialProducerPolicy,
    ) -> Result<u64, ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::BeginInitialProducerAttempt { policy, reply })
            .await
            .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    /// Bind immutable response facts before producer-policy admission or
    /// compatibility registry publication. Compatibility-owned generations
    /// use both facts; actor-owned prepublication generations take their
    /// presentation identity from the matching initial policy and use this
    /// binding only to order `Session::fail` with response authorization.
    pub(crate) async fn bind_response_publication_contract(
        &self,
        presentation_contract_fingerprint: String,
        failure_fence: Arc<AtomicBool>,
    ) -> Result<(), ResponsePublicationRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::BindResponsePublicationContract {
            presentation_contract_fingerprint,
            failure_fence,
            reply,
        })
        .await
        .map_err(|_| ResponsePublicationRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ResponsePublicationRejection::ControlUnavailable))
    }

    pub(crate) async fn authorize_response_publication(
        &self,
        publication: RollingResponsePublication,
        handoff: Option<RollingFirstMediaPublicationHandoff>,
        deadline: Instant,
    ) -> Result<RollingResponseAuthorization, ResponsePublicationRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command_before(
            RollingControlCommand::AuthorizeResponsePublication {
                publication,
                handoff,
                deadline,
                reply,
            },
            deadline,
        )
        .await
        .map_err(|_| ResponsePublicationRejection::ControlUnavailable)?;
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(Err(ResponsePublicationRejection::ControlUnavailable))
    }

    pub(crate) async fn admit_producer_retry(
        &self,
        decision_sequence: u64,
        recipe_fingerprint: &str,
    ) -> Result<u64, ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::AdmitProducerRetry {
            decision_sequence,
            recipe_fingerprint: recipe_fingerprint.to_owned(),
            reply,
        })
        .await
        .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    pub(crate) async fn decision_applied(
        &self,
        decision_sequence: u64,
        installed_attempt: Option<u64>,
    ) -> Result<(), ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command(RollingControlCommand::DecisionApplied {
            decision_sequence,
            installed_attempt,
            reply,
        })
        .await
        .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        response
            .await
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    /// Publish the executor's one bounded, exact-attempt natural-exit proof.
    /// Command sealing orders the proof after its exit barrier; the actor's
    /// existing `ClassifyingExit` deadline remains the only verdict clock.
    /// Release the outstanding-signal claim after a signal attempt that
    /// published no acknowledgement barrier — a failed or refused syscall.
    ///
    /// A successful signal settles itself when its `FlowApplied` barrier is
    /// folded. Without this path a failure would leave the actor believing a
    /// signal were forever in flight, and every later desire would coalesce
    /// behind a signal that no longer exists. Returns whether the desire still
    /// diverges from the applied state.
    pub(crate) async fn settle_producer_flow_signal_before(
        &self,
        producer_attempt: u64,
        deadline: Instant,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command_before(
                RollingControlCommand::SettleProducerFlowSignal {
                    producer_attempt,
                    reply,
                },
                deadline,
            )
            .await
            .is_err()
        {
            return false;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }

    /// Put one desired physical-flow state on the shared actor sequence and
    /// learn whether this caller owns the next signal.
    ///
    /// The reply is the only authorization to signal. `Coalesced` means an
    /// earlier signal is still outstanding and this desire will be applied
    /// after its acknowledgement; `Settled` means the desire already matches
    /// the applied state. Neither is a licence to call the supervisor.
    pub(crate) async fn request_producer_flow_before(
        &self,
        producer_attempt: u64,
        desired_hold: bool,
        deadline: Instant,
    ) -> ProducerFlowIntentionOutcome {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command_before(
                RollingControlCommand::ApplyProducerFlow {
                    producer_attempt,
                    desired_hold,
                    reply,
                },
                deadline,
            )
            .await
            .is_err()
        {
            return ProducerFlowIntentionOutcome::Rejected;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(ProducerFlowIntentionOutcome::Rejected)
    }

    pub(crate) async fn classify_producer_exit_before(
        &self,
        evidence: RollingProducerCompletionEvidence,
        deadline: Instant,
    ) -> Result<RollingProducerCompletionDisposition, ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command_before(
            RollingControlCommand::ClassifyProducerExit {
                evidence,
                deadline,
                reply,
            },
            deadline,
        )
        .await
        .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(ProducerAttemptRejection::ControlUnavailable)
    }

    /// Publish one copy-reader result through the bounded actor mailbox. The
    /// command is sealed with preceding process facts and names the exact
    /// attempt, so a detached predecessor cannot classify its successor.
    pub(crate) async fn classify_copy_producer_exit_before(
        &self,
        producer_attempt: u64,
        classification: CopyProducerExitClassification,
        deadline: Instant,
    ) -> Result<(), ProducerAttemptRejection> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command_before(
            RollingControlCommand::ClassifyCopyProducerExit {
                producer_attempt,
                classification,
                deadline,
                reply,
            },
            deadline,
        )
        .await
        .map_err(|_| ProducerAttemptRejection::ControlUnavailable)?;
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(Err(ProducerAttemptRejection::ControlUnavailable))
    }

    /// Poll the immutable actor decision after an executor-owned sequence.
    /// Polling is a read and never clears or acknowledges the one-slot value.
    #[cfg(test)]
    pub(crate) async fn poll_producer_decision(
        &self,
        after_sequence: u64,
    ) -> Result<ProducerDecisionPoll, ()> {
        match self.decision_transport.poll_after(after_sequence).await {
            DecisionTransportPoll::Available(poll) => Ok(poll),
            DecisionTransportPoll::Unavailable => Err(()),
        }
    }

    #[cfg(test)]
    pub(crate) async fn install_producer_decision_for_test(
        &self,
        decision: ProducerDecision,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::InstallProducerDecision { decision, reply })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn executor_observation_for_test(&self) -> (String, u64) {
        (
            self.decision_transport
                .executor_observation
                .state()
                .to_owned(),
            self.decision_transport
                .executor_observation
                .last_observed_sequence
                .load(Ordering::Acquire),
        )
    }

    #[cfg(test)]
    pub(crate) fn pause_executor_poll_for_test(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .decision_transport
            .executor_poll_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    pub(crate) fn pause_executor_observation_for_test(&self, pause: Arc<tokio::sync::Barrier>) {
        *self
            .decision_transport
            .executor_observation_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
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
    ) -> Result<ProducerInstallAuthorization, ProducerAttemptRejection> {
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
        authorization: ProducerInstallAuthorization,
    ) -> Result<std::sync::MutexGuard<'_, RollingProducerTransitionFence>, ProducerAttemptRejection>
    {
        let mut transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.sender.is_closed() {
            return Err(ProducerAttemptRejection::ControlUnavailable);
        }
        if self.retired.load(Ordering::Acquire) || rolling_now() >= transition.lease_deadline {
            return Err(ProducerAttemptRejection::SessionEnded);
        }
        if self.producer_attempt.load(Ordering::Acquire)
            != authorization.coordinate.producer_attempt
        {
            return Err(ProducerAttemptRejection::StaleAttempt);
        }
        if authorization
            .coordinate
            .producer_deadline
            .is_some_and(|deadline| rolling_now() >= deadline)
        {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        if transition.install_authorization != Some(authorization.coordinate) {
            return Err(ProducerAttemptRejection::DecisionMismatch);
        }
        transition.install_authorization = None;
        Ok(transition)
    }

    pub(crate) async fn observe_publication(
        &self,
        observation: RollingPublicationObservation,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::ObservePublication {
                observation,
                deadline: None,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    /// Publish an observation required by one HTTP playlist response within
    /// that request's existing absolute deadline. Unlike the background flow
    /// refresh above, a queued HTTP observation is fail-closed when its reply
    /// receiver disappears or its deadline has elapsed, so cancelling the
    /// request cannot mutate actor publication state later.
    pub(crate) async fn observe_publication_before(
        &self,
        observation: RollingPublicationObservation,
        deadline: Instant,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command_before(
                RollingControlCommand::ObservePublication {
                    observation,
                    deadline: Some(deadline),
                    reply,
                },
                deadline,
            )
            .await
            .is_err()
        {
            return false;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }

    pub(crate) async fn commit_media(
        &self,
        kind: &'static str,
        producer_attempt: u64,
        segment_index: Option<i64>,
        segment_end_ms: Option<i64>,
        handoff: Option<RollingMediaCommitHandoff>,
        deadline: Instant,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command_before(
                RollingControlCommand::CommitMedia {
                    kind,
                    producer_attempt,
                    segment_index,
                    segment_end_ms,
                    handoff,
                    deadline,
                    reply,
                },
                deadline,
            )
            .await
            .is_err()
        {
            return false;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }

    pub(crate) async fn commit_generation_metadata(
        &self,
        presentation_contract_fingerprint: &str,
        kind: &'static str,
        deadline: Instant,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command_before(
                RollingControlCommand::CommitGenerationMetadata {
                    presentation_contract_fingerprint: presentation_contract_fingerprint.to_owned(),
                    kind,
                    deadline,
                    reply,
                },
                deadline,
            )
            .await
            .is_err()
        {
            return false;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
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

    /// Take the preparation slot for a successor whose durable row already
    /// exists. `false` when the slot is occupied or the playback is terminal —
    /// in both cases the caller must abort the row it just created, because
    /// nothing else knows about it.
    pub(crate) async fn stage_preparation(
        &self,
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::StagePreparation {
                staged_incarnation_id,
                predecessor_incarnation_id,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    /// Whether this exact successor may still be committed. Asked immediately
    /// before the durable CAS and re-checked after it, because the gate and
    /// the call cannot be one transaction.
    pub(crate) async fn may_commit_preparation(&self, staged_incarnation_id: &str) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::MayCommitPreparation {
                staged_incarnation_id: staged_incarnation_id.to_owned(),
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
    }

    /// Report a preparation's durable outcome and free the slot.
    ///
    /// A retired actor cannot be told, and that is not a failure: its
    /// `terminate` already moved the slot to aborting, which is the same
    /// conclusion this call would reach.
    pub(crate) async fn settle_preparation(
        &self,
        staged_incarnation_id: &str,
        committed: bool,
    ) -> bool {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .enqueue_command(RollingControlCommand::SettlePreparation {
                staged_incarnation_id: staged_incarnation_id.to_owned(),
                committed,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        response.await.unwrap_or(false)
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

    /// Admit Terminal only within the caller's absolute deadline. Once the
    /// envelope is in the mailbox, actor settlement is independently owned and
    /// cannot be reclassified as a pre-commit deadline merely because dispatch
    /// or reply delivery crosses that instant.
    async fn terminate_before(
        &self,
        cause: RollingTerminalCause,
        deadline: Instant,
    ) -> Result<RollingTerminalOutcome, RollingTerminalRequestError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.enqueue_command_before(RollingControlCommand::Terminal { cause, reply }, deadline)
            .await
            .map_err(|error| match error {
                RollingCommandAdmissionError::Deadline => {
                    RollingTerminalRequestError::AdmissionDeadline
                }
                RollingCommandAdmissionError::ControlUnavailable => {
                    RollingTerminalRequestError::ControlUnavailable
                }
            })?;
        response
            .await
            .map_err(|_| RollingTerminalRequestError::ControlUnavailable)
    }

    pub(crate) async fn end(&self) -> Result<RollingTerminalOutcome, ControlStateError> {
        self.terminate(RollingTerminalCause::End).await
    }

    pub(crate) async fn end_before(
        &self,
        deadline: Instant,
    ) -> Result<RollingTerminalOutcome, RollingTerminalRequestError> {
        self.terminate_before(RollingTerminalCause::End, deadline)
            .await
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
    pub(crate) fn abort_executor_for_test(&self) {
        self.executor_abort
            .as_ref()
            .expect("spawned executor abort handle")
            .abort();
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_actor_start_for_test(&self) {
        self.actor_run_started.notified().await;
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
        guard: &std::sync::MutexGuard<'_, RollingProducerTransitionFence>,
        producer_attempt: u64,
    ) -> bool {
        if self.is_retired()
            || self.sender.is_closed()
            || self.current_producer_attempt() != producer_attempt
            || !guard.producer_signal_authorized
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
        !self.is_retired()
            && !self.sender.is_closed()
            && guard.producer_signal_authorized
            && now < guard.lease_deadline
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

    /// Resolve a detached first-media waiter that reached its request
    /// deadline. The actor uses this same fence while mutating publication
    /// state and settling the handoff, so an already-accepted transfer wins
    /// intact; otherwise rejection and retirement linearize before the queued
    /// command can claim first media later.
    pub(crate) fn settle_first_media_at_deadline(
        &self,
        waiter: &RollingFirstMediaPublicationWaiter,
    ) -> bool {
        let _transition = self
            .producer_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(accepted) = waiter.settled_outcome() {
            return accepted;
        }
        waiter.settle_rejected();
        self.retired.store(true, Ordering::Release);
        self.producer_events.notify_flow_capacity_waiters();
        self.flow_sync.request();
        false
    }

    pub(crate) fn media_commit_handoff(
        &self,
        producer_attempt: u64,
        segment_index: i64,
        segment_end_ms: Option<i64>,
        compatibility_attempt: Arc<std::sync::Mutex<u64>>,
        high_segment: Arc<AtomicI64>,
        fetched_end_ms: Arc<AtomicI64>,
    ) -> RollingMediaCommitHandoff {
        RollingMediaCommitHandoff {
            producer_attempt,
            segment_index,
            segment_end_ms,
            compatibility_attempt,
            high_segment,
            fetched_end_ms,
            flow_sync: Arc::clone(&self.flow_sync),
        }
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
/// Resolved actions by kind and client platform.
static CONTROL_ACTIONS: [[AtomicU64; 3]; 4] = [const { [const { AtomicU64::new(0) }; 3] }; 4];
/// Holds actually sent, by reason.
static CONTROL_HOLD_REASONS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
/// What M6 *would* do about a selection change, by axis and outcome.
///
/// Shadow: nothing is staged and no behaviour depends on this. The point is
/// that `PREPARED_AXIS` and the throughput floor are currently arguments, and
/// this is what turns them into measurements on real traffic before anything
/// acts on them. In particular it is the only way to learn whether
/// `throughput_unproven` refuses so often that the prepared path would never
/// fire at all — a possibility the rule's own doc names and cannot settle.
///
/// Indexed `[axis][outcome]`. Axis order is `PreparationAxis`'s own; outcome
/// is prepare, then the four `FallbackReason`s in declaration order.
static PREPARATION_DECISIONS: [[AtomicU64; 5]; 5] = [const { [const { AtomicU64::new(0) }; 5] }; 5];
/// Exchanges where production was held and the client had not declared the
/// action, so it was told nothing. Watch this fall as clients roll out.
static CONTROL_ACTIONS_SUPPRESSED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// Clients by platform and whether they declared they accept a hold. This is
/// the fleet's rollout progress, readable without touching a device.
static CONTROL_VOCABULARY: [[AtomicU64; 3]; 2] = [const { [const { AtomicU64::new(0) }; 3] }; 2];
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
static ROLLING_PRODUCER_EXIT_CLASSIFICATIONS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static ROLLING_CONTROL_COMMANDS: [AtomicU64; 24] = [const { AtomicU64::new(0) }; 24];
static ROLLING_PRODUCER_ACTION_DEADLINES: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
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

fn platform_index(platform: ClientPlatform) -> usize {
    match platform {
        ClientPlatform::Web => 0,
        ClientPlatform::Apple => 1,
        ClientPlatform::Android => 2,
    }
}

/// Record what M6 would have done, without doing it.
///
/// Called only when the actor says the selection moved, so the denominator is
/// *viewer actions*, not exchanges. An `Unchanged` decision is not recorded at
/// all: it means the selection moved on an axis this type cannot see —
/// a seek, a different file — and counting it would put a bar labelled
/// "nothing to do" beside four that mean something.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn record_preparation_decision(decision: PreparationDecision) {
    let (axis, outcome) = match decision {
        PreparationDecision::Unchanged => return,
        PreparationDecision::Prepare { axis } => (axis, 0),
        PreparationDecision::Fallback { axis, reason } => (
            axis,
            1 + match reason {
                FallbackReason::ClientCannotPrepare => 0,
                FallbackReason::AxisNotProven => 1,
                FallbackReason::MultipleAxes => 2,
                FallbackReason::ThroughputUnproven => 3,
            },
        ),
    };
    let axis = match axis {
        PreparationAxis::ResolutionOrBitrate => 0,
        PreparationAxis::AudioTrackOrOffset => 1,
        PreparationAxis::SubtitleBurn => 2,
        PreparationAxis::DeliveryMethod => 3,
        PreparationAxis::DynamicRange => 4,
    };
    PREPARATION_DECISIONS[axis][outcome].fetch_add(1, Ordering::Relaxed);
}

/// Record what one accepted exchange's action was, and what it could not be.
pub(crate) fn record_action(
    action: &ControlAction,
    delivery: &DeliveryView,
    request: &ControlRequestV1,
    platform: ClientPlatform,
) {
    let metrics = action_metrics(action, delivery, request);
    let platform = platform_index(platform);
    CONTROL_ACTIONS[metrics.action as usize][platform].fetch_add(1, Ordering::Relaxed);
    if let Some(reason) = metrics.hold_reason {
        CONTROL_HOLD_REASONS[reason as usize].fetch_add(1, Ordering::Relaxed);
    }
    if metrics.suppressed {
        CONTROL_ACTIONS_SUPPRESSED[platform].fetch_add(1, Ordering::Relaxed);
    }
    // "Fully declared" means every action this server can send. A client that
    // names only some of them is still partially unmanaged, and a metric that
    // called it rolled out would hide that.
    let complete = request.accepts(HOLD_ACTION)
        && request.accepts(TERMINAL_ACTION)
        && request.accepts(RETRY_RESOURCE_ACTION);
    CONTROL_VOCABULARY[usize::from(complete)][platform].fetch_add(1, Ordering::Relaxed);
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
        "# HELP plurx_playback_control_actions_total Resolved playback-control actions by kind and client platform.\n\
         # TYPE plurx_playback_control_actions_total counter\n",
    );
    for (action_index, action) in ["none", "hold", "terminal", "retry_resource"]
        .iter()
        .enumerate()
    {
        for (platform_index, platform) in ["web", "apple", "android"].iter().enumerate() {
            output.push_str(&format!(
                "plurx_playback_control_actions_total{{action=\"{action}\",platform=\"{platform}\"}} {}\n",
                CONTROL_ACTIONS[action_index][platform_index].load(Ordering::Relaxed)
            ));
        }
    }
    output.push_str(
        "# HELP plurx_playback_preparation_decisions_total What M6 would do about a selection change, by axis and outcome. Shadow: nothing is staged.\n\
         # TYPE plurx_playback_preparation_decisions_total counter\n",
    );
    for (axis_index, axis) in [
        PreparationAxis::ResolutionOrBitrate,
        PreparationAxis::AudioTrackOrOffset,
        PreparationAxis::SubtitleBurn,
        PreparationAxis::DeliveryMethod,
        PreparationAxis::DynamicRange,
    ]
    .iter()
    .enumerate()
    {
        for (outcome_index, outcome) in [
            "prepare",
            FallbackReason::ClientCannotPrepare.as_str(),
            FallbackReason::AxisNotProven.as_str(),
            FallbackReason::MultipleAxes.as_str(),
            FallbackReason::ThroughputUnproven.as_str(),
        ]
        .iter()
        .enumerate()
        {
            output.push_str(&format!(
                "plurx_playback_preparation_decisions_total{{axis=\"{}\",outcome=\"{outcome}\"}} {}\n",
                axis.as_str(),
                PREPARATION_DECISIONS[axis_index][outcome_index].load(Ordering::Relaxed)
            ));
        }
    }
    output.push_str(
        "# HELP plurx_playback_control_holds_total Holds sent to a client, by the reason production is not advancing.\n\
         # TYPE plurx_playback_control_holds_total counter\n",
    );
    for (index, reason) in [
        "demand",
        "time",
        "bytes",
        "global",
        "ahead",
        "working_set",
        "no_room",
    ]
    .iter()
    .enumerate()
    {
        output.push_str(&format!(
            "plurx_playback_control_holds_total{{reason=\"{reason}\"}} {}\n",
            CONTROL_HOLD_REASONS[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(
        "# HELP plurx_playback_control_actions_suppressed_total Exchanges where production was held and the client had not declared the action, so it was told nothing.\n\
         # TYPE plurx_playback_control_actions_suppressed_total counter\n",
    );
    for (index, platform) in ["web", "apple", "android"].iter().enumerate() {
        output.push_str(&format!(
            "plurx_playback_control_actions_suppressed_total{{platform=\"{platform}\"}} {}\n",
            CONTROL_ACTIONS_SUPPRESSED[index].load(Ordering::Relaxed)
        ));
    }
    output.push_str(
        "# HELP plurx_playback_control_vocabulary_total Accepted exchanges by client platform and whether the client declared every action this server can send.\n\
         # TYPE plurx_playback_control_vocabulary_total counter\n",
    );
    for (accepts_index, accepts) in ["false", "true"].iter().enumerate() {
        for (platform_index, platform) in ["web", "apple", "android"].iter().enumerate() {
            output.push_str(&format!(
                "plurx_playback_control_vocabulary_total{{complete=\"{accepts}\",platform=\"{platform}\"}} {}\n",
                CONTROL_VOCABULARY[accepts_index][platform_index].load(Ordering::Relaxed)
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
        "# HELP plurx_playback_rolling_producer_deadline_observations_total Rolling producer deadlines observed due by bounded mode; action-passive for legacy actors and decision-bearing only for opted-in prepublication actors.\n\
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
        "# HELP plurx_playback_rolling_producer_exit_classifications_total Bounded exact-attempt natural-exit classification outcomes.\n\
         # TYPE plurx_playback_rolling_producer_exit_classifications_total counter\n",
    );
    for (index, outcome) in ["verified", "unverified", "partial_success", "stale"]
        .iter()
        .enumerate()
    {
        output.push_str(&format!(
            "plurx_playback_rolling_producer_exit_classifications_total{{outcome=\"{outcome}\"}} {}\n",
            ROLLING_PRODUCER_EXIT_CLASSIFICATIONS[index].load(Ordering::Relaxed),
        ));
    }
    output.push_str(
        "# HELP plurx_playback_rolling_producer_action_deadlines_total Actor-issued physical transactions that lapsed unacknowledged, by kind.\n\
         # TYPE plurx_playback_rolling_producer_action_deadlines_total counter\n",
    );
    for (index, kind) in ["signal_stop", "signal_resume"].iter().enumerate() {
        output.push_str(&format!(
            "plurx_playback_rolling_producer_action_deadlines_total{{kind=\"{kind}\"}} {}\n",
            ROLLING_PRODUCER_ACTION_DEADLINES[index].load(Ordering::Relaxed),
        ));
    }
    output.push_str(
        "# HELP plurx_playback_rolling_control_commands_total Sequenced rolling actor commands dequeued by bounded kind.\n\
         # TYPE plurx_playback_rolling_control_commands_total counter\n",
    );
    for (index, kind) in [
        "control",
        "begin_attempt",
        "poll_producer_decision",
        "authorize_install",
        "observe_publication",
        "commit_media",
        "snapshot",
        "claim_expiry",
        "terminal",
        "register_producer_executor",
        "begin_initial_producer_attempt",
        "authorize_response_publication",
        "admit_producer_retry",
        "decision_applied",
        "commit_generation_metadata",
        "bind_response_publication_contract",
        "classify_producer_exit",
        "executor_settled",
        "classify_copy_producer_exit",
        "apply_producer_flow",
        "settle_producer_flow_signal",
        "stage_preparation",
        "may_commit_preparation",
        "settle_preparation",
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
                    producer_decision: None,
                    hold_reason: None,
                    subtitle_readiness: None,
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
            supported_actions: None,
        }
    }

    fn delivery_with_hold(reason: Option<&str>) -> DeliveryView {
        DeliveryView {
            presentation: "live-recovery".to_owned(),
            producer_state: "held".to_owned(),
            produced_through_ms: Some(30_000),
            fetched_through_ms: 25_000,
            delivered_bps: Some(4_000_000),
            delivered_idle_ms: Some(0),
            recent_producer_speed: Some(1.4),
            client_runway_ms: 15_000,
            admitted: None,
            producer_decision: None,
            hold_reason: reason.map(str::to_owned),
            subtitle_readiness: None,
            owner_node_hash: "n-0123456789abcdef".to_owned(),
            owner_epoch: 1,
        }
    }

    #[test]
    fn a_suppressed_hold_is_counted_so_the_rollout_can_be_watched() {
        // The number that says how much a client upgrade would buy. Production
        // is held, the server knows why, and the client cannot be told because
        // it never declared the action.
        let passive = request();
        let held = delivery_with_hold(Some("demand"));
        let action = resolve_action(&ControlAction::None, &held, &passive);
        assert_eq!(action, ControlAction::None);
        assert_eq!(
            action_metrics(&action, &held, &passive),
            ActionMetrics {
                action: ActionKind::None,
                hold_reason: None,
                suppressed: true,
            },
        );

        // Nothing was held, so nothing was withheld.
        let flowing = delivery_with_hold(None);
        assert!(
            !action_metrics(
                &resolve_action(&ControlAction::None, &flowing, &passive),
                &flowing,
                &passive,
            )
            .suppressed,
        );

        // A reason this server cannot name was never a candidate instruction.
        // Counting it would inflate the gap with responses that no client
        // rollout would change.
        let unknown = delivery_with_hold(Some("a_reason_from_next_year"));
        assert!(
            !action_metrics(
                &resolve_action(&ControlAction::None, &unknown, &passive),
                &unknown,
                &passive,
            )
            .suppressed,
        );
    }

    #[test]
    fn a_sent_hold_is_counted_by_reason_and_never_as_suppressed() {
        let mut accepting = request();
        accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
        let held = delivery_with_hold(Some("working_set"));
        let action = resolve_action(&ControlAction::None, &held, &accepting);
        assert_eq!(
            action_metrics(&action, &held, &accepting),
            ActionMetrics {
                action: ActionKind::Hold,
                hold_reason: Some(HoldReason::WorkingSet),
                suppressed: false,
            },
        );
    }

    #[test]
    fn every_hold_reason_has_its_own_counter_slot() {
        // The exposition indexes `CONTROL_HOLD_REASONS` by `reason as usize`,
        // so a variant added in the middle would silently report under its
        // neighbour's name. This pins the discriminants to the exposed order.
        for (expected, reason) in [
            HoldReason::Demand,
            HoldReason::Time,
            HoldReason::Bytes,
            HoldReason::Global,
            HoldReason::Ahead,
            HoldReason::WorkingSet,
            HoldReason::NoRoom,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(reason as usize, expected, "{reason:?} moved slot");
        }
        assert_eq!(CONTROL_HOLD_REASONS.len(), 7);
    }

    #[test]
    fn only_a_verdict_about_the_source_itself_is_permanent() {
        // The split that decides whether a client should ever be told to give
        // up. Twelve of the fourteen are timing, process, or executor facts:
        // the same file on the same pipeline may work on the next attempt.
        // Two are verdicts about the source, and no retry changes those.
        for reason in ProducerDecisionReason::ALL {
            let expected = matches!(
                reason,
                ProducerDecisionReason::Unsupported | ProducerDecisionReason::InvalidConfiguration
            );
            assert_eq!(
                reason.is_permanent(),
                expected,
                "{} classified wrongly",
                reason.status(),
            );
        }
        assert_eq!(
            ProducerDecisionReason::ALL
                .into_iter()
                .filter(|reason| reason.is_permanent())
                .count(),
            2,
        );
    }

    #[test]
    fn every_decision_reason_round_trips_its_wire_name() {
        // `ALL` is what the wire is bounded against and what metrics will be
        // labelled by. A variant added to the enum and forgotten here would
        // be accepted by the relay check under no name at all.
        for reason in ProducerDecisionReason::ALL {
            assert_eq!(
                ProducerDecisionReason::from_status(reason.status()),
                Some(reason),
            );
        }
        assert_eq!(ProducerDecisionReason::ALL.len(), 14);
        assert_eq!(ProducerDecisionReason::from_status("invented"), None);
        // The names are wire-safe: lowercase, underscored, no spaces.
        for reason in ProducerDecisionReason::ALL {
            let name = reason.status();
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{name} is not a wire-safe name",
            );
        }
    }

    /// The readiness value is a fact about the *selected* track, and silence
    /// is a real answer rather than a missing one.
    #[test]
    fn subtitle_readiness_speaks_only_for_a_selected_native_track() {
        let native = |track| SubtitleSelection {
            mode: SubtitleMode::Native,
            track: Some(track),
        };

        assert_eq!(
            subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Ready)).as_deref(),
            Some("ready")
        );
        assert_eq!(
            subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Warming)).as_deref(),
            Some("warming")
        );
        assert_eq!(
            subtitle_readiness_value(&native(0), Some(SubtitleTrackCache::Unavailable)).as_deref(),
            Some("unavailable")
        );
        // A track the caller could not resolve at all: the client asked for a
        // specific one and deserves to be told to stop waiting, not silence.
        assert_eq!(
            subtitle_readiness_value(&native(0), None).as_deref(),
            Some("unavailable")
        );

        // Every other mode names no sidecar. Burn-in is video and its
        // readiness is the producer's, already reported; overlay is the bitmap
        // path's own lifecycle; off names no track.
        for mode in [SubtitleMode::Off, SubtitleMode::Overlay, SubtitleMode::Burn] {
            let selection = SubtitleSelection {
                mode,
                // `Off` carries no track and the others must; validation
                // enforces that upstream, so match it rather than fight it.
                track: (!matches!(mode, SubtitleMode::Off)).then_some(0),
            };
            assert_eq!(
                subtitle_readiness_value(&selection, Some(SubtitleTrackCache::Ready)),
                None,
                "{mode:?} must not report subtitle readiness"
            );
        }
    }

    /// An older peer relays a response it built before this field existed.
    ///
    /// `DeliveryView` is `deny_unknown_fields`, so the compatibility that
    /// matters runs the other way: absence must deserialize, and must not be
    /// read as `ready`. This mirrors the guarantee `producer_decision` already
    /// carries — absence means "not classified here", never "healthy".
    #[test]
    fn a_delivery_without_subtitle_readiness_round_trips_as_absent() {
        let json = serde_json::to_string(&delivery_with_hold(None)).expect("serialize");
        assert!(
            !json.contains("subtitle_readiness"),
            "an absent readiness must not be written to the wire at all: {json}"
        );

        let relayed: DeliveryView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(relayed.subtitle_readiness, None);

        let mut ready = delivery_with_hold(None);
        ready.subtitle_readiness = Some("ready".to_owned());
        let round_tripped: DeliveryView =
            serde_json::from_str(&serde_json::to_string(&ready).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_tripped.subtitle_readiness.as_deref(), Some("ready"));
    }

    /// Relays preserve extension values they do not understand. The consumer,
    /// not an intermediate server, decides whether a value means `ready`; if a
    /// relay normalized this to absence, a value introduced by a future peer
    /// could never cross an older node.
    #[test]
    fn subtitle_readiness_relay_preserves_an_unknown_value() {
        let mut delivery = delivery_with_hold(None);
        delivery.subtitle_readiness = Some("a_value_from_next_year".to_owned());

        let relayed: DeliveryView =
            serde_json::from_str(&serde_json::to_string(&delivery).expect("serialize"))
                .expect("deserialize");

        assert_eq!(
            relayed.subtitle_readiness.as_deref(),
            Some("a_value_from_next_year")
        );
    }

    fn delivery_with_decision(decision: &str) -> DeliveryView {
        let mut delivery = delivery_with_hold(None);
        delivery.producer_state = "failed".to_owned();
        delivery.producer_decision = Some(decision.to_owned());
        delivery
    }

    fn accepts_everything() -> ControlRequestV1 {
        let mut request = request();
        request.supported_actions = Some(vec![
            HOLD_ACTION.to_owned(),
            TERMINAL_ACTION.to_owned(),
            RETRY_RESOURCE_ACTION.to_owned(),
        ]);
        request
    }

    #[test]
    fn a_permanent_decision_ends_the_session_and_a_transient_one_does_not() {
        let client = accepts_everything();
        for reason in ProducerDecisionReason::ALL {
            let delivery = delivery_with_decision(reason.status());
            let action = resolve_action(&ControlAction::None, &delivery, &client);
            if reason.is_permanent() {
                let ControlAction::Terminal { code, message } = action else {
                    panic!("{} must end the session", reason.status());
                };
                assert_eq!(code, reason);
                assert!(!message.is_empty());
                assert!(message.len() <= MAX_TERMINAL_MESSAGE_BYTES);
            } else {
                assert_eq!(
                    action,
                    ControlAction::RetryResource {
                        after_ms: NEXT_EXCHANGE_MS,
                        reason,
                    },
                    "{} must be retryable",
                    reason.status(),
                );
            }
        }
    }

    #[test]
    fn a_stopped_producer_outranks_a_hold() {
        // A client told to hold while the producer is finished would sit
        // through a film that was never going to play. The ranking is what
        // stops that, so it is asserted rather than assumed.
        let client = accepts_everything();
        let mut delivery = delivery_with_decision("unsupported");
        delivery.hold_reason = Some("demand".to_owned());
        assert!(matches!(
            resolve_action(&ControlAction::None, &delivery, &client),
            ControlAction::Terminal { .. },
        ));

        let mut retryable = delivery_with_decision("reader_failed");
        retryable.hold_reason = Some("demand".to_owned());
        assert!(matches!(
            resolve_action(&ControlAction::None, &retryable, &client),
            ControlAction::RetryResource { .. },
        ));
    }

    #[test]
    fn an_undeclared_verdict_is_withheld_rather_than_softened_into_a_hold() {
        // The client accepts `hold` and nothing else. It must not be told to
        // hold when the truth is that the producer stopped: a producer that
        // has stopped is not holding, and saying so would be a comfortable
        // lie that leaves the client waiting on nothing.
        let mut hold_only = request();
        hold_only.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
        let mut delivery = delivery_with_decision("unsupported");
        delivery.hold_reason = Some("demand".to_owned());
        assert_eq!(
            resolve_action(&ControlAction::None, &delivery, &hold_only),
            ControlAction::None,
        );
        assert!(
            action_metrics(&ControlAction::None, &delivery, &hold_only).suppressed,
            "the withheld verdict is what the rollout metric counts",
        );
    }

    #[test]
    fn a_relayed_verdict_must_match_the_decision_and_its_permanence() {
        // A peer calling a retryable decision terminal would end a playable
        // session; the reverse would loop a client forever. Neither is
        // forwarded.
        let permanent = ProducerDecisionReason::Unsupported;
        let transient = ProducerDecisionReason::ReaderFailed;
        assert!(permanent.is_permanent() && !transient.is_permanent());
        assert_eq!(
            ControlAction::Terminal {
                code: permanent,
                message: terminal_message(permanent),
            }
            .vocabulary_name(),
            Some(TERMINAL_ACTION),
        );
        assert_eq!(
            ControlAction::RetryResource {
                after_ms: NEXT_EXCHANGE_MS,
                reason: transient,
            }
            .vocabulary_name(),
            Some(RETRY_RESOURCE_ACTION),
        );
        assert_eq!(ControlAction::None.vocabulary_name(), None);
    }

    #[test]
    fn the_new_actions_are_tagged_on_the_wire() {
        assert_eq!(
            serde_json::to_value(ControlAction::Terminal {
                code: ProducerDecisionReason::Unsupported,
                message: "nope".to_owned(),
            })
            .expect("action json"),
            serde_json::json!({
                "type": "terminal",
                "code": "unsupported",
                "message": "nope",
            }),
        );
        assert_eq!(
            serde_json::to_value(ControlAction::RetryResource {
                after_ms: 5_000,
                reason: ProducerDecisionReason::ReaderFailed,
            })
            .expect("action json"),
            serde_json::json!({
                "type": "retry_resource",
                "after_ms": 5_000,
                "reason": "reader_failed",
            }),
        );
    }

    #[test]
    fn a_passive_client_is_never_sent_a_hold_it_cannot_apply() {
        // The whole reason the vocabulary exists. Every M2 reporter stops on
        // an action it does not recognise, so a server that ignored this would
        // silence the fleet the moment production held for any reason at all.
        let passive = request();
        assert!(passive.supported_actions.is_none());
        assert_eq!(
            resolve_action(
                &ControlAction::None,
                &delivery_with_hold(Some("demand")),
                &passive,
            ),
            ControlAction::None,
        );

        let mut names_other_actions = request();
        names_other_actions.supported_actions =
            Some(vec!["terminal".to_owned(), "retry_resource".to_owned()]);
        assert_eq!(
            resolve_action(
                &ControlAction::None,
                &delivery_with_hold(Some("demand")),
                &names_other_actions,
            ),
            ControlAction::None,
        );
    }

    #[test]
    fn a_client_that_accepts_hold_is_told_why_production_is_held() {
        let mut accepting = request();
        accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
        for (delivery_reason, expected) in [
            ("demand", HoldReason::Demand),
            ("time", HoldReason::Time),
            ("bytes", HoldReason::Bytes),
            ("global", HoldReason::Global),
            ("ahead", HoldReason::Ahead),
            ("working_set", HoldReason::WorkingSet),
            ("no_room", HoldReason::NoRoom),
        ] {
            assert_eq!(
                resolve_action(
                    &ControlAction::None,
                    &delivery_with_hold(Some(delivery_reason)),
                    &accepting,
                ),
                ControlAction::Hold { reason: expected },
                "delivery reported {delivery_reason}",
            );
        }
        // No hold reported is not a hold. A client that accepts the action
        // must still receive `none` while production is advancing.
        assert_eq!(
            resolve_action(&ControlAction::None, &delivery_with_hold(None), &accepting),
            ControlAction::None,
        );
        // A reason this server does not know is not promoted to an
        // instruction. Reporting it in the delivery view is harmless; telling
        // a client to hold on a word we cannot explain is not.
        assert_eq!(
            resolve_action(
                &ControlAction::None,
                &delivery_with_hold(Some("something_new")),
                &accepting,
            ),
            ControlAction::None,
        );
    }

    #[test]
    fn a_decided_action_outranks_the_advisory_hold() {
        // An action the actor decided is a transaction fenced by
        // `prior_action`; the advisory hold is derived per response. When both
        // exist the transaction is the one with identity, so it wins and its
        // reason is not quietly rewritten by current delivery.
        let mut accepting = request();
        accepting.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
        assert_eq!(
            resolve_action(
                &ControlAction::Hold {
                    reason: HoldReason::Demand,
                },
                &delivery_with_hold(Some("bytes")),
                &accepting,
            ),
            ControlAction::Hold {
                reason: HoldReason::Demand,
            },
        );
    }

    #[test]
    fn every_hold_reason_round_trips_its_delivery_spelling() {
        // The action and `DeliveryView::hold_reason` are one vocabulary read
        // twice. If either side gains a value without the other, an operator
        // reading the two fields sees different words for one fact — and the
        // relay check below starts refusing valid responses.
        for reason in [
            HoldReason::Demand,
            HoldReason::Time,
            HoldReason::Bytes,
            HoldReason::Global,
            HoldReason::Ahead,
            HoldReason::WorkingSet,
            HoldReason::NoRoom,
        ] {
            assert_eq!(
                HoldReason::from_delivery(reason.as_delivery_str()),
                Some(reason),
            );
        }
        assert_eq!(HoldReason::from_delivery("unheard_of"), None);
    }

    #[test]
    fn the_action_is_tagged_on_the_wire() {
        assert_eq!(
            serde_json::to_value(ControlAction::Hold {
                reason: HoldReason::WorkingSet,
            })
            .expect("action json"),
            serde_json::json!({"type": "hold", "reason": "working_set"}),
        );
        assert_eq!(
            serde_json::to_value(ControlAction::None).expect("action json"),
            serde_json::json!({"type": "none"}),
        );
    }

    #[test]
    fn the_action_vocabulary_is_bounded_but_not_policed() {
        let target = 60_000;
        let mut unknown_names = request();
        // A client newer than this server names actions it has never heard of.
        // Refusing that would be a 400, which the reporter reads as terminal.
        unknown_names.supported_actions = Some(vec![
            "hold".to_owned(),
            "an_action_from_next_year".to_owned(),
        ]);
        assert!(unknown_names.validate(None, target).is_ok());
        assert!(unknown_names.accepts_hold());

        let mut too_many = request();
        too_many.supported_actions = Some(vec!["hold".to_owned(); MAX_SUPPORTED_ACTIONS + 1]);
        assert_eq!(too_many.validate(None, target), Err("supported_actions"),);

        let mut empty_name = request();
        empty_name.supported_actions = Some(vec![String::new()]);
        assert_eq!(empty_name.validate(None, target), Err("supported_actions"));

        let mut long_name = request();
        long_name.supported_actions = Some(vec!["x".repeat(MAX_ACTION_NAME_LEN + 1)]);
        assert_eq!(long_name.validate(None, target), Err("supported_actions"));

        // Absent is the passive default, and stays valid.
        let mut absent = request();
        absent.supported_actions = None;
        assert!(absent.validate(None, target).is_ok());
        assert!(!absent.accepts_hold());
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

    /// M6's gate: the actor says when a selection moved, because it is the
    /// only place both selections are in hand for free.
    ///
    /// The expensive half — two store reads to resolve a candidate — is what
    /// this exists to skip, on an exchange that runs about once a second per
    /// client under an absolute deadline.
    #[test]
    fn the_actor_reports_when_a_selection_moved() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let base = request();

        let mut first = base.clone();
        first.sequence = 1;
        let outcome = actor
            .control_at(started + Duration::from_secs(1), owned_control(&first))
            .expect("sequence 1 accepted");
        assert!(
            !outcome.selection_changed,
            "a session that has just been created from an intent has not since \
             departed from it",
        );

        let mut same = base.clone();
        same.sequence = 2;
        // A different playhead, buffer and render state — everything an
        // ordinary exchange carries — with the same selection.
        same.position_ms = base.position_ms + 4_000;
        same.buffered_through_ms = base.buffered_through_ms + 4_000;
        let outcome = actor
            .control_at(started + Duration::from_secs(2), owned_control(&same))
            .expect("sequence 2 accepted");
        assert!(
            !outcome.selection_changed,
            "an ordinary exchange must not spend the reads the gate exists to save",
        );

        let mut moved = base.clone();
        moved.sequence = 3;
        moved.selection.audio_track = Some(base.selection.audio_track.unwrap_or(0) + 1);
        let outcome = actor
            .control_at(started + Duration::from_secs(3), owned_control(&moved))
            .expect("sequence 3 accepted");
        assert!(outcome.selection_changed, "the viewer picked another track");

        // And it is a comparison against the *previous* selection, not a
        // latch: holding the new selection is not a fresh change.
        let mut held = moved.clone();
        held.sequence = 4;
        let outcome = actor
            .control_at(started + Duration::from_secs(4), owned_control(&held))
            .expect("sequence 4 accepted");
        assert!(
            !outcome.selection_changed,
            "the selection is now the previous one",
        );
    }

    /// The comparison is against what the client last asked for, never against
    /// what it is being served — and this is the case that decides it.
    ///
    /// An explicit rung snaps onto the ladder, so a client asking 1079 is
    /// delivered 1080 and keeps asking 1079. A gate that compared the ask to
    /// the delivered height would read *changed* on every exchange for the
    /// rest of that session, spend two store reads each time, and produce a
    /// candidate identical to what is already playing.
    #[test]
    fn a_snapped_ask_is_not_a_change_every_second() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let mut base = request();
        base.selection.quality = QualitySelection::Manual {
            height: crate::transcode::snap_height(1079) - 1,
        };
        assert_ne!(
            crate::transcode::snap_height(1079),
            1079,
            "the fixture is only meaningful if 1079 actually snaps",
        );

        for (sequence, seconds) in [(1, 1), (2, 2), (3, 3)] {
            let mut exchange = base.clone();
            exchange.sequence = sequence;
            let outcome = actor
                .control_at(
                    started + Duration::from_secs(seconds),
                    owned_control(&exchange),
                )
                .expect("accepted");
            assert!(
                !outcome.selection_changed,
                "sequence {sequence}: an unchanged ask is unchanged however it \
                 was served",
            );
        }
    }

    /// A replay is the same exchange arriving twice. It changed the selection
    /// the first time or not at all, and answering `true` again would spend
    /// the reads twice for one viewer action.
    #[test]
    fn a_replay_is_not_a_second_change() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let base = request();
        let mut first = base.clone();
        first.sequence = 1;
        actor
            .control_at(started + Duration::from_secs(1), owned_control(&first))
            .expect("sequence 1 accepted");

        let mut moved = base.clone();
        moved.sequence = 2;
        moved.selection.audio_offset_ms = 250;
        let accepted = actor
            .control_at(started + Duration::from_secs(2), owned_control(&moved))
            .expect("sequence 2 accepted");
        assert!(accepted.selection_changed);

        let replayed = actor
            .control_at(started + Duration::from_secs(3), owned_control(&moved))
            .expect("sequence 2 replayed");
        assert_eq!(replayed.disposition, ControlDisposition::Replay);
        assert!(
            !replayed.selection_changed,
            "one viewer action, one resolution",
        );
    }

    /// The wire lets a client send capabilities once. Anything that reads
    /// the live snapshot therefore sees `None` from sequence 2 onward — and
    /// for `dual_player_preparation` that means refusing every transition a
    /// capable client ever makes, which is every transition that will
    /// actually happen.
    #[test]
    fn the_capability_document_is_retained_across_exchanges_that_omit_it() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert!(
            actor.retained_capabilities().is_none(),
            "a session that has not been told holds nothing"
        );

        let base = request();
        let mut first = base.clone();
        first.sequence = 1;
        first.capabilities = first.capabilities.map(|mut caps| {
            caps.dual_player_preparation = true;
            caps
        });
        actor
            .control_at(started + Duration::from_secs(1), owned_control(&first))
            .expect("sequence 1 accepted");
        assert!(
            actor
                .retained_capabilities()
                .expect("retained after sequence 1")
                .dual_player_preparation
        );

        // Two exchanges that say nothing about capabilities, which is what
        // the wire contract permits and what real clients do.
        for (sequence, seconds) in [(2, 2), (3, 3)] {
            let mut later = base.clone();
            later.sequence = sequence;
            later.capabilities = None;
            actor
                .control_at(
                    started + Duration::from_secs(seconds),
                    owned_control(&later),
                )
                .expect("later exchange accepted");
            assert!(
                actor
                    .retained_capabilities()
                    .expect("still retained")
                    .dual_player_preparation,
                "sequence {sequence} omitted capabilities; it did not withdraw them",
            );
        }

        // A client that sends a new document has changed its answer about a
        // device that changed, so the newer one wins.
        let mut revised = base.clone();
        revised.sequence = 4;
        revised.capabilities = revised.capabilities.map(|mut caps| {
            caps.dual_player_preparation = false;
            caps
        });
        actor
            .control_at(started + Duration::from_secs(4), owned_control(&revised))
            .expect("revision accepted");
        assert!(
            !actor
                .retained_capabilities()
                .expect("retained after revision")
                .dual_player_preparation,
            "last write wins over Some",
        );
    }

    #[test]
    fn a_seek_storm_settles_on_one_target_and_supersedes_every_earlier_one() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let base = request();
        let mut asked = Vec::new();
        for step in 0..20u64 {
            let mut seek = base.clone();
            seek.sequence = step + 1;
            seek.render_state = RenderState::Seeking;
            let target = 600_000 + (step as i64 % 5) * 300_000 - (step as i64) * 7_000;
            seek.seek_target_ms = Some(target);
            seek.position_ms = target;
            seek.buffered_from_ms = None;
            seek.buffered_through_ms = target;
            let outcome = actor
                .control_at(
                    // Above the server's own 250 ms exchange floor. A storm
                    // faster than that is already refused as `RateLimited`, so
                    // spacing these tighter would measure the rate limiter
                    // rather than the latch -- and is worth knowing on its own:
                    // the cadence already bounds how many restarts one storm
                    // can ask for.
                    started + Duration::from_millis(300 * (step + 1)),
                    owned_control(&seek),
                )
                .expect("each seek in the storm is accepted");
            assert_eq!(outcome.disposition, ControlDisposition::Accepted);
            asked.push((seek.sequence, target));
        }
        let settled = actor.settled_target.expect("a storm settles a target");
        let (last_sequence, last_target) = *asked.last().expect("twenty seeks");
        assert_eq!(settled.sequence, last_sequence);
        assert_eq!(settled.anchor_ms, last_target);
        let superseded = asked
            .iter()
            .filter(|(sequence, target)| settled.supersedes(*sequence, *target))
            .count();
        assert_eq!(superseded, 19, "nineteen of twenty seeks are obsolete");
        assert!(!settled.supersedes(last_sequence, last_target));
    }

    #[test]
    fn a_settled_target_never_supersedes_an_exchange_it_has_not_seen() {
        let settled = SettledTarget {
            sequence: 12,
            anchor_ms: 900_000,
        };
        assert!(settled.supersedes(11, 100_000));
        assert!(!settled.supersedes(12, 100_000), "equal is not later");
        assert!(
            !settled.supersedes(13, 100_000),
            "later demand is not stale"
        );
    }

    #[test]
    fn a_seek_that_lands_beside_its_target_is_not_a_new_destination() {
        let settled = SettledTarget {
            sequence: 40,
            anchor_ms: 1_800_000,
        };
        assert!(!settled.supersedes(39, 1_799_640));
        assert!(!settled.supersedes(39, 1_800_000 + SETTLED_TARGET_SLACK_MS));
        assert!(settled.supersedes(39, 1_800_000 + SETTLED_TARGET_SLACK_MS + 1));
    }

    #[test]
    fn the_latch_follows_ordinary_playback_once_the_seek_is_over() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let base = request();
        let mut seek = base.clone();
        seek.sequence = 1;
        seek.render_state = RenderState::Seeking;
        seek.seek_target_ms = Some(1_500_000);
        seek.position_ms = 1_500_000;
        seek.buffered_from_ms = None;
        seek.buffered_through_ms = 1_500_000;
        actor
            .control_at(started + Duration::from_secs(1), owned_control(&seek))
            .expect("seek accepted");
        assert_eq!(
            actor.settled_target.expect("seek settles").anchor_ms,
            1_500_000
        );
        let mut playing = base.clone();
        playing.sequence = 2;
        playing.position_ms = 1_512_000;
        playing.buffered_from_ms = Some(1_511_000);
        playing.buffered_through_ms = 1_540_000;
        actor
            .control_at(started + Duration::from_secs(2), owned_control(&playing))
            .expect("playback accepted");
        let settled = actor.settled_target.expect("playback settles too");
        assert_eq!(settled.sequence, 2);
        assert_eq!(settled.anchor_ms, 1_512_000);
    }

    fn staged_actor() -> (RollingControlActor, Instant) {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert!(actor.stage_preparation("successor-1".to_owned(), "current-1".to_owned()));
        (actor, started)
    }

    /// The slot is only useful if the executor can actually reach it, and the
    /// executor reaches it through the actor's bounded mailbox rather than by
    /// touching state. These drive a real spawned actor.
    #[tokio::test]
    async fn a_staged_successor_round_trips_through_the_actor_mailbox() {
        let handle = RollingControlHandle::spawn("session-start");
        assert!(
            handle
                .stage_preparation("successor-1".to_owned(), "current-1".to_owned())
                .await
        );
        assert!(handle.may_commit_preparation("successor-1").await);
        assert!(
            !handle.may_commit_preparation("successor-2").await,
            "a stale executor must not be told it may commit"
        );
        assert!(handle.settle_preparation("successor-1", true).await);
        assert!(
            handle
                .stage_preparation("successor-2".to_owned(), "successor-1".to_owned())
                .await,
            "a committed successor frees the slot for the next preparation"
        );
        handle.abort_actor_for_test();
    }

    /// A commit that lost its CAS reports `committed: false`, and that must
    /// free the slot exactly as a completed abort does. The successor still
    /// exists durably, so leaving the slot occupied would strand the playback
    /// with a preparation nothing can finish.
    #[tokio::test]
    async fn a_lost_commit_frees_the_slot_and_refuses_a_retry() {
        let handle = RollingControlHandle::spawn("session-start");
        assert!(
            handle
                .stage_preparation("successor-1".to_owned(), "current-1".to_owned())
                .await
        );
        assert!(handle.settle_preparation("successor-1", false).await);
        assert!(
            !handle.may_commit_preparation("successor-1").await,
            "a lost commit cannot be retried into a win"
        );
        assert!(
            handle
                .stage_preparation("successor-2".to_owned(), "current-1".to_owned())
                .await
        );
        handle.abort_actor_for_test();
    }

    /// The end-to-end shape of "a disconnect does not imply a commit", driven
    /// through the mailbox the executor actually uses.
    #[tokio::test]
    async fn an_ended_playback_refuses_the_commit_its_executor_was_about_to_make() {
        let handle = RollingControlHandle::spawn("session-start");
        assert!(
            handle
                .stage_preparation("successor-1".to_owned(), "current-1".to_owned())
                .await
        );
        assert!(handle.may_commit_preparation("successor-1").await);

        let ended = handle.terminate(RollingTerminalCause::End).await;
        assert!(ended.is_ok());

        assert!(
            !handle.may_commit_preparation("successor-1").await,
            "the executor's gate closes with the playback"
        );
        handle.abort_actor_for_test();
    }

    /// One preparation per playback is the store's invariant. The actor
    /// refuses a second rather than believing in one the store would reject —
    /// a slot holding a successor the ledger never accepted is how a commit
    /// ends up naming the wrong incarnation.
    #[test]
    fn the_slot_holds_one_successor_and_refuses_a_competitor() {
        let (mut actor, _) = staged_actor();
        assert!(!actor.stage_preparation("successor-2".to_owned(), "current-1".to_owned()));
        assert_eq!(
            actor.preparation.staged_incarnation_id(),
            Some("successor-1")
        );
    }

    /// A commit names the exact successor it prepared. A stale executor
    /// speaking for a preparation that has since been replaced must not be
    /// able to advance the pointer to an incarnation the actor has forgotten.
    #[test]
    fn only_the_successor_the_slot_holds_may_commit() {
        let (actor, _) = staged_actor();
        assert!(actor.preparation.may_commit("successor-1"));
        assert!(!actor.preparation.may_commit("successor-2"));
    }

    /// **Disconnect does not imply commit.** Whatever ends the playback, a
    /// successor that was staged and never committed is torn down, and a
    /// commit arriving afterwards is refused — otherwise a viewer who closed
    /// the tab could publish a generation they never waited for.
    #[test]
    fn a_terminal_playback_aborts_a_staged_successor_and_refuses_a_late_commit() {
        for cause in [
            RollingTerminalCause::End,
            RollingTerminalCause::LeaseExpired,
        ] {
            let (mut actor, _) = staged_actor();
            assert!(actor.preparation.may_commit("successor-1"));

            let outcome = actor.terminate(cause);
            assert!(matches!(outcome, RollingTerminalOutcome::Won(_)));

            assert!(
                matches!(actor.preparation, PreparationSlot::Aborting { .. }),
                "{cause:?} must leave the successor aborting"
            );
            assert!(
                !actor.preparation.may_commit("successor-1"),
                "{cause:?} must refuse a late commit"
            );
        }
    }

    /// An abort under way cannot become a commit. The transition is one-way
    /// because the alternative is a race in which a slow abort and a slow
    /// commit both believe they won.
    #[test]
    fn an_abort_in_flight_never_becomes_a_commit() {
        let (mut actor, _) = staged_actor();
        assert!(
            actor.abort_preparation("successor-1"),
            "first abort moves it"
        );
        assert!(
            !actor.abort_preparation("successor-1"),
            "a retried abort reports no movement rather than a spurious loss"
        );
        assert!(!actor.preparation.may_commit("successor-1"));
        assert!(actor.preparation.may_abort("successor-1"));
    }

    /// A stale executor cannot tear down a successor that replaced the one it
    /// knew about, and cannot clear a slot it does not hold.
    #[test]
    fn a_stale_executor_moves_nothing() {
        let (mut actor, _) = staged_actor();
        assert!(!actor.abort_preparation("successor-2"));
        assert!(!actor.settle_preparation("successor-2"));
        assert_eq!(
            actor.preparation.staged_incarnation_id(),
            Some("successor-1")
        );
    }

    /// Settling clears the slot for a committed successor and a completed
    /// abort alike: after either, this playback has no staged generation and
    /// may prepare again.
    #[test]
    fn settling_frees_the_slot_for_the_next_preparation() {
        for abort_first in [false, true] {
            let (mut actor, _) = staged_actor();
            if abort_first {
                assert!(actor.abort_preparation("successor-1"));
            }
            assert!(actor.settle_preparation("successor-1"));
            assert_eq!(actor.preparation, PreparationSlot::Empty);
            assert!(actor.stage_preparation("successor-2".to_owned(), "current-1".to_owned()));
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
    fn an_unacknowledged_hold_signal_becomes_a_typed_flow_stop_deadline() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        assert!(actor.pending_producer_action.is_some());

        // Before the budget: nothing lapses.
        assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(5)));
        assert!(actor.pending_producer_action.is_some());

        // At the budget: one typed failure, and the signal token is fenced so
        // a late acknowledgement cannot revive the producer.
        assert!(actor.settle_producer_action_deadline_at(started + Duration::from_secs(11)));
        assert!(actor.pending_producer_action.is_none());
        assert!(!actor.producer_signal_authorized);
        assert!(!actor.producer_flow_signal_outstanding);
    }

    #[test]
    fn an_unacknowledged_resume_signal_is_distinguishable_from_a_hold() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        // Reach Held through an acknowledgement so the resume intention is a
        // real divergence rather than a no-op.
        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(1),
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(1),
            },
        );
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(2),
                1,
                ProducerPhysicalFlowState::Running,
            ),
            ProducerFlowIntentionOutcome::Issue { hold: false, .. }
        ));
        assert_eq!(
            actor.pending_producer_action.map(|action| action.kind),
            Some(ProducerActionKind::SignalResume),
        );
        assert_eq!(
            ProducerActionKind::SignalResume.deadline_reason(),
            ProducerDecisionReason::FlowResumeDeadline,
        );
        assert_eq!(
            ProducerActionKind::SignalStop.deadline_reason(),
            ProducerDecisionReason::FlowStopDeadline,
        );
    }

    #[test]
    fn an_acknowledged_signal_leaves_no_action_deadline_behind() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(2),
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(2),
            },
        );
        assert!(
            actor.pending_producer_action.is_none(),
            "an acknowledged transaction owes nothing"
        );
        assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(30)));
    }

    #[test]
    fn a_lapsed_action_from_a_previous_attempt_is_observation_only() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        // Succession takes the slot; the predecessor's deadline is stale.
        assert_eq!(
            actor.begin_producer_attempt_at(started + Duration::from_secs(2)),
            Ok(2)
        );
        assert!(actor.pending_producer_action.is_none());
        assert!(!actor.settle_producer_action_deadline_at(started + Duration::from_secs(30)));
        assert!(
            actor.producer_signal_authorized,
            "a successor must not inherit its predecessor's fenced token"
        );
    }

    #[test]
    fn the_next_wake_accounts_for_an_outstanding_action() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let without = actor.next_deadline();
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        let with = actor.next_deadline();
        assert!(
            with <= without,
            "a signal that never comes back has to be able to wake the actor"
        );
        assert!(with <= started + Duration::from_secs(11));
    }

    #[test]
    fn a_flow_intention_never_moves_the_applied_state_or_the_deadline() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        let armed = actor.producer_progress_deadline;
        assert!(armed.is_some(), "an admitted attempt arms a deadline");

        // Requesting a hold does not change the running deadline.
        assert_eq!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue {
                revision: 1,
                hold: true
            },
        );
        assert_eq!(actor.producer_progress_deadline, armed);
        assert_eq!(
            actor.producer_physical_flow,
            ProducerPhysicalFlowState::Running
        );
    }

    #[test]
    fn a_flow_intention_coalesces_while_a_signal_is_outstanding() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));

        assert_eq!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue {
                revision: 1,
                hold: true
            },
        );
        // A second desire while that signal is in flight must not authorize a
        // second syscall; it becomes the newest revision instead.
        assert_eq!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(2),
                1,
                ProducerPhysicalFlowState::Running,
            ),
            ProducerFlowIntentionOutcome::Coalesced,
        );
        assert_eq!(actor.producer_flow_intention.revision, 2);
        assert_eq!(
            actor.producer_flow_intention.desired,
            ProducerPhysicalFlowState::Running
        );

        // The hold acknowledgement lands; the newest desire still diverges, so
        // exactly one further signal is owed.
        actor.apply_producer_flow_applied_at(
            started + Duration::from_secs(3),
            ProducerFlowApplied {
                revision: 1,
                producer_attempt: 1,
                state: ProducerPhysicalFlowState::Held,
                published_at: started + Duration::from_secs(3),
            },
        );
        assert!(actor.settle_producer_flow_signal(1));
    }

    #[test]
    fn a_failed_signal_releases_the_outstanding_claim() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        // No barrier is published for a failed syscall. Without settling here
        // the actor would believe a signal were forever in flight.
        assert!(
            actor.settle_producer_flow_signal(1),
            "the desire still diverges, so another signal is owed"
        );
        assert_eq!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(2),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue {
                revision: 2,
                hold: true
            },
        );
    }

    #[test]
    fn a_flow_intention_for_another_attempt_is_rejected() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert_eq!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                2,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Rejected,
            "a predecessor or successor attempt may not signal this child"
        );
    }

    #[test]
    fn attempt_succession_clears_a_predecessor_flow_intention() {
        let started = Instant::now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(actor.begin_producer_attempt_at(started), Ok(1));
        assert!(matches!(
            actor.apply_producer_flow_intention_at(
                started + Duration::from_secs(1),
                1,
                ProducerPhysicalFlowState::Held,
            ),
            ProducerFlowIntentionOutcome::Issue { .. }
        ));
        assert_eq!(
            actor.begin_producer_attempt_at(started + Duration::from_secs(2)),
            Ok(2)
        );
        assert_eq!(actor.producer_flow_intention.revision, 0);
        assert_eq!(
            actor.producer_flow_intention.desired,
            ProducerPhysicalFlowState::Running
        );
        assert!(
            !actor.producer_flow_signal_outstanding,
            "a successor must not inherit an outstanding signal claim"
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
        assert!(actor
            .authorize_producer_install_at(started + Duration::from_secs(1), attempt,)
            .is_ok());
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
                producer_decision: None,
                hold_reason: None,
                subtitle_readiness: None,
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
        let mut invented_state = response.clone();
        invented_state.delivery.producer_state = "probably_running".to_owned();
        assert!(!invented_state.is_valid_for(&request));

        // A relayed hold is an instruction arriving from another node. The
        // relaying node forwards it only when the same response's delivery
        // reports the same hold and the client actually asked for the action —
        // otherwise a peer could manufacture an instruction out of a fact it
        // never sent, and the client would obey it.
        let mut accepting = request.clone();
        accepting.control.supported_actions = Some(vec![HOLD_ACTION.to_owned()]);
        let mut held = response.clone();
        held.delivery.hold_reason = Some("working_set".to_owned());
        held.action = ControlAction::Hold {
            reason: HoldReason::WorkingSet,
        };
        assert!(held.is_valid_for(&accepting));

        let mut disagreeing = held.clone();
        disagreeing.delivery.hold_reason = Some("no_room".to_owned());
        assert!(
            !disagreeing.is_valid_for(&accepting),
            "the action and the delivery fact it came from must agree"
        );

        let mut unreported = held.clone();
        unreported.delivery.hold_reason = None;
        assert!(!unreported.is_valid_for(&accepting));

        assert!(
            !held.is_valid_for(&request),
            "a passive client must never be relayed an action it cannot apply"
        );

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

    /// The shadow metric's full cross product is published from boot, and its
    /// labels are the decision's own vocabulary.
    ///
    /// Published at zero rather than on first observation, because the useful
    /// reading is a *ratio* — how much of the fallback volume is
    /// `client_cannot_prepare` against `throughput_unproven` — and a series
    /// that appears only once it is nonzero cannot be divided by one that has
    /// not appeared yet.
    #[test]
    fn the_shadow_decision_metric_publishes_every_axis_and_outcome() {
        let metrics = prometheus();
        for axis in [
            PreparationAxis::ResolutionOrBitrate,
            PreparationAxis::AudioTrackOrOffset,
            PreparationAxis::SubtitleBurn,
            PreparationAxis::DeliveryMethod,
            PreparationAxis::DynamicRange,
        ] {
            for outcome in [
                "prepare",
                FallbackReason::ClientCannotPrepare.as_str(),
                FallbackReason::AxisNotProven.as_str(),
                FallbackReason::MultipleAxes.as_str(),
                FallbackReason::ThroughputUnproven.as_str(),
            ] {
                let series = format!(
                    "plurx_playback_preparation_decisions_total{{axis=\"{}\",outcome=\"{outcome}\"}}",
                    axis.as_str()
                );
                assert!(metrics.contains(&series), "missing {series}");
            }
        }
    }

    /// `Unchanged` is not an outcome, and recording it would put a bar
    /// labelled "nothing to do" beside four that mean something.
    ///
    /// It is reachable: the actor gates on the *selection*, and a selection
    /// can move on an axis `EffectiveSelection` cannot see — a seek, a
    /// different file. The decision then correctly answers `Unchanged`, and
    /// the right thing to do with that is nothing.
    #[test]
    fn an_unchanged_decision_is_not_recorded() {
        let before = prometheus();
        record_preparation_decision(PreparationDecision::Unchanged);
        assert_eq!(
            preparation_series_total(&before),
            preparation_series_total(&prometheus()),
        );

        record_preparation_decision(PreparationDecision::Fallback {
            axis: PreparationAxis::DynamicRange,
            reason: FallbackReason::ThroughputUnproven,
        });
        assert_eq!(
            preparation_series_total(&prometheus()),
            preparation_series_total(&before) + 1,
            "and a real decision is",
        );
    }

    /// Sum every `plurx_playback_preparation_decisions_total` series. The
    /// counters are process-global, so a test that asserted an absolute value
    /// would depend on which other tests had run.
    fn preparation_series_total(metrics: &str) -> u64 {
        metrics
            .lines()
            .filter(|line| line.starts_with("plurx_playback_preparation_decisions_total{"))
            .filter_map(|line| line.rsplit(' ').next()?.parse::<u64>().ok())
            .sum()
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
        assert!(metrics.contains(
            "plurx_playback_rolling_producer_exit_classifications_total{outcome=\"partial_success\"}"
        ));
        assert!(
            metrics.contains("plurx_playback_rolling_control_commands_total{kind=\"snapshot\"}")
        );
        assert!(metrics.contains(
            "plurx_playback_rolling_control_commands_total{kind=\"classify_producer_exit\"}"
        ));
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
        let activation = plurx_core::domain::MediaSessionActivation {
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
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms,
            lease_expires_at_ms,
        };
        store
            .activate_media_session(&activation)
            .await
            .expect("activate route")
            .expect("activation accepted");
        store
            .settle_media_session_activation(
                &activation,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now_ms,
            )
            .await
            .expect("confirm route")
            .expect("route confirmed");
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
            .end_media_session(&session, "deleted", now + 1)
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

    fn playing(height: i64) -> EffectiveSelection {
        EffectiveSelection {
            quality_auto: false,
            height,
            audio_track: Some(0),
            subtitle_burn: None,
            audio_offset_ms: 0,
            codec: "server_selected".to_owned(),
            dynamic_range: Some("hdr10".to_owned()),
        }
    }

    /// A recipe's grade intent. `sdr()` is the default both sides use, so a
    /// test that does not mention the grade is testing a transition that does
    /// not cross it.
    fn sdr() -> GradeIntent {
        GradeIntent {
            hdr10: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        }
    }

    fn hdr10() -> GradeIntent {
        GradeIntent {
            hdr10: true,
            ..sdr()
        }
    }

    fn view(selection: &EffectiveSelection) -> RecipeView<'_> {
        RecipeView {
            selection,
            grade: sdr(),
        }
    }

    fn view_at(selection: &EffectiveSelection, grade: GradeIntent) -> RecipeView<'_> {
        RecipeView { selection, grade }
    }

    fn can_prepare(dual_player_preparation: bool) -> DynamicCapabilities {
        DynamicCapabilities {
            platform: ClientPlatform::Apple,
            max_height: 2160,
            codecs: vec![CodecPolicy::H264, CodecPolicy::Hevc],
            dynamic_ranges: vec![DynamicRangePolicy::Sdr, DynamicRangePolicy::Hdr10],
            dual_player_preparation,
        }
    }

    /// A link with room for a second pipeline: twice what this session is
    /// delivering, with margin.
    fn roomy() -> PreparationConditions {
        PreparationConditions {
            observed_download_bps: Some(80_000_000),
            delivered_bps: Some(12_000_000),
        }
    }

    /// The axis M6 prepares across, on a client that says it can, on a link
    /// that has shown headroom — in **both** directions, because upshift is
    /// the direction that raises demand and a rule that only ever tested
    /// downshifts would not notice being restricted to them.
    #[test]
    fn a_resolution_change_on_a_capable_client_is_prepared() {
        for (from, to) in [(2160, 1080), (1080, 2160)] {
            assert_eq!(
                decide_preparation(
                    view(&playing(from)),
                    view(&playing(to)),
                    Some(&can_prepare(true)),
                    roomy(),
                ),
                PreparationDecision::Prepare {
                    axis: PreparationAxis::ResolutionOrBitrate,
                },
                "{from} -> {to}",
            );
        }
        // Auto ↔ manual at the same delivered height is still a change: the
        // ladder the successor climbs differs even when the first rung matches.
        let mut auto = playing(1080);
        auto.quality_auto = true;
        assert_eq!(
            decide_preparation(
                view(&playing(1080)),
                view(&auto),
                Some(&can_prepare(true)),
                roomy(),
            ),
            PreparationDecision::Prepare {
                axis: PreparationAxis::ResolutionOrBitrate,
            },
        );
    }

    /// The capability is read, never re-derived. This is the test that fails
    /// if someone infers "Apple can prepare" from the platform enum — the
    /// mistake M5.5 exists because of.
    #[test]
    fn a_client_that_did_not_say_it_can_prepare_gets_the_fallback() {
        assert_eq!(
            decide_preparation(
                view(&playing(2160)),
                view(&playing(1080)),
                Some(&can_prepare(false)),
                roomy(),
            ),
            PreparationDecision::Fallback {
                axis: PreparationAxis::ResolutionOrBitrate,
                reason: FallbackReason::ClientCannotPrepare,
            },
            "the platform is Apple and Apple measured true; the literal still says false",
        );
        assert_eq!(
            decide_preparation(view(&playing(2160)), view(&playing(1080)), None, roomy()),
            PreparationDecision::Fallback {
                axis: PreparationAxis::ResolutionOrBitrate,
                reason: FallbackReason::ClientCannotPrepare,
            },
            "a session that has never been told has not been told",
        );
        // And the capability outranks the axis: an incapable client crossing
        // an unproven axis is reported as incapable, because that is the fact
        // that would change if the literal flipped.
        let playing = playing(2160);
        assert_eq!(
            decide_preparation(
                view(&playing),
                view_at(&playing, hdr10()),
                Some(&can_prepare(false)),
                roomy(),
            ),
            PreparationDecision::Fallback {
                axis: PreparationAxis::DynamicRange,
                reason: FallbackReason::ClientCannotPrepare,
            },
        );
    }

    /// Every other axis falls back even on a capable client, and names itself.
    #[test]
    fn the_unproven_axes_fall_back_and_name_themselves() {
        let caps = can_prepare(true);
        let mut method = playing(2160);
        method.codec = "source".to_owned();
        let mut audio = playing(2160);
        audio.audio_track = Some(1);
        let mut offset = playing(2160);
        offset.audio_offset_ms = 250;
        let mut burn = playing(2160);
        burn.subtitle_burn = Some(3);
        // Each Dolby Vision answer is its own crossing: preserving decides
        // whether the RPUs survive the filter and converting decides whether
        // they are rewritten, and a viewer can see the difference.
        let preserve = GradeIntent {
            preserve_dolby_vision: true,
            ..sdr()
        };
        let convert = GradeIntent {
            preserve_dolby_vision: true,
            convert_dolby_vision: true,
            ..sdr()
        };

        for (candidate, grade, axis) in [
            (&method, sdr(), PreparationAxis::DeliveryMethod),
            (&playing(2160), hdr10(), PreparationAxis::DynamicRange),
            (&playing(2160), preserve, PreparationAxis::DynamicRange),
            (&playing(2160), convert, PreparationAxis::DynamicRange),
            (&audio, sdr(), PreparationAxis::AudioTrackOrOffset),
            (&offset, sdr(), PreparationAxis::AudioTrackOrOffset),
            (&burn, sdr(), PreparationAxis::SubtitleBurn),
        ] {
            assert_eq!(
                decide_preparation(
                    view(&playing(2160)),
                    view_at(candidate, grade),
                    Some(&caps),
                    roomy(),
                ),
                PreparationDecision::Fallback {
                    axis,
                    reason: FallbackReason::AxisNotProven,
                },
                "{}",
                axis.as_str(),
            );
        }
    }

    fn session_request(kind: SessionKind) -> crate::transcode::SessionRequest {
        crate::transcode::SessionRequest {
            file_id: 5615,
            playback_id: "player-a".to_owned(),
            request_id: None,
            control_sequence: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind,
            start_seconds: 12.5,
            audio_index: Some(0),
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
        }
    }

    fn converting_copy() -> crate::transcode::SessionRequest {
        session_request(SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: true,
            convert_dolby_vision: true,
        })
    }

    fn selection_at(quality: QualitySelection) -> ClientSelection {
        ClientSelection {
            quality,
            audio_track: Some(0),
            subtitle: SubtitleSelection {
                mode: SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: CodecPolicy::Auto,
            dynamic_range: DynamicRangePolicy::Auto,
        }
    }

    /// The property that ruled the candidate's shape: a converting Profile 7
    /// session, with the client changing nothing, must read `Unchanged`.
    ///
    /// It fails on both obvious implementations. A candidate round-tripped
    /// through a `CreateSession` loses `convert_dolby_vision` — `into_request`
    /// hardcodes it false, because a client cannot ask to be handed a
    /// conversion — so the candidate's `GradeIntent` differs from the
    /// delivered one on that field alone, and every exchange reports a grade
    /// crossing for a viewer who changed nothing. Dolby Vision titles would
    /// simply never prepare, and the metric would look entirely plausible.
    #[test]
    fn a_converting_session_that_changed_nothing_is_unchanged() {
        let current = converting_copy();
        let candidate = candidate_request(
            &current,
            &selection_at(QualitySelection::Manual { height: 2160 }),
            2160,
            Some(2160),
        );
        assert_eq!(
            candidate.kind, current.kind,
            "a source-height ask on a copy is that copy, asked for by name",
        );
        assert_eq!(
            GradeIntent::from_request(&candidate),
            GradeIntent::from_request(&current),
            "the conversion must survive the round trip, or DV never prepares",
        );

        let delivered =
            EffectiveSelection::from_request(&current, 2160, Some("dolby_vision".into()));
        let proposed = EffectiveSelection::from_request(&candidate, 2160, None);
        assert_eq!(
            decide_preparation(
                RecipeView {
                    selection: &delivered,
                    grade: GradeIntent::from_request(&current),
                },
                RecipeView {
                    selection: &proposed,
                    grade: GradeIntent::from_request(&candidate),
                },
                Some(&can_prepare(true)),
                roomy(),
            ),
            PreparationDecision::Unchanged,
        );
    }

    /// A copy has no rung, so a manual rung that is not the source's own is a
    /// request to stop copying — and the candidate must say so rather than
    /// silently keep copying. The decision then refuses it, which is the
    /// point: the candidate reports what was asked for, and the decision is
    /// what says no.
    ///
    /// It is refused as a **grade** change, not a delivery-method one, and
    /// that is not an accident of this fixture: leaving a Dolby Vision copy
    /// *always* crosses the grade axis, because a transcode carries no RPUs at
    /// all — `GradeIntent::from_request` answers both DV questions `false` for
    /// a transcode as a statement about its output. So the hardest axis names
    /// it, and for a DV title that is `dynamic_range`.
    #[test]
    fn a_rung_a_copy_cannot_serve_becomes_a_transcode() {
        let current = converting_copy();
        let candidate = candidate_request(
            &current,
            &selection_at(QualitySelection::Manual { height: 1080 }),
            1080,
            Some(2160),
        );
        assert_eq!(candidate.kind, SessionKind::Transcode { height: 1080 });

        let delivered = EffectiveSelection::from_request(&current, 2160, None);
        let proposed = EffectiveSelection::from_request(&candidate, 1080, None);
        assert_eq!(
            decide_preparation(
                RecipeView {
                    selection: &delivered,
                    grade: GradeIntent::from_request(&current),
                },
                RecipeView {
                    selection: &proposed,
                    grade: GradeIntent::from_request(&candidate),
                },
                Some(&can_prepare(true)),
                roomy(),
            ),
            PreparationDecision::Fallback {
                axis: PreparationAxis::DynamicRange,
                reason: FallbackReason::MultipleAxes,
            },
            "method, height and grade all move; the hardest names it",
        );
        // A copy with no Dolby Vision to lose crosses method and height only,
        // and then the delivery method is the hardest axis moving.
        let plain = session_request(SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        });
        let plain_candidate = candidate_request(
            &plain,
            &selection_at(QualitySelection::Manual { height: 1080 }),
            1080,
            Some(2160),
        );
        let plain_delivered = EffectiveSelection::from_request(&plain, 2160, None);
        let plain_proposed = EffectiveSelection::from_request(&plain_candidate, 1080, None);
        assert_eq!(
            decide_preparation(
                RecipeView {
                    selection: &plain_delivered,
                    grade: GradeIntent::from_request(&plain),
                },
                RecipeView {
                    selection: &plain_proposed,
                    grade: GradeIntent::from_request(&plain_candidate),
                },
                Some(&can_prepare(true)),
                roomy(),
            ),
            PreparationDecision::Fallback {
                axis: PreparationAxis::DeliveryMethod,
                reason: FallbackReason::MultipleAxes,
            },
        );
    }

    /// The comparison that decides it is against the **source** height, never
    /// against the resolved one. A 1080 ask on a 2160 source resolves to 1080
    /// — it is already a ladder rung, so the snap is the identity — and
    /// comparing the ask to the result would find them equal and leave a copy
    /// copying a rung it cannot serve.
    #[test]
    fn the_copy_escape_is_the_source_height_not_the_resolved_one() {
        let current = converting_copy();
        let resolved_equals_ask = candidate_request(
            &current,
            &selection_at(QualitySelection::Manual { height: 1080 }),
            1080,
            Some(2160),
        );
        assert_eq!(
            resolved_equals_ask.kind,
            SessionKind::Transcode { height: 1080 },
            "ask == resolved height is not the escape; ask == source height is",
        );
    }

    /// Auto never changes the delivery method. It asks the server to choose a
    /// rung, not to stop copying — turning a direct play into a transcode
    /// because the viewer selected Auto would be a downgrade nobody asked for.
    #[test]
    fn auto_leaves_a_copy_copying() {
        let current = converting_copy();
        let candidate = candidate_request(
            &current,
            &selection_at(QualitySelection::Auto),
            1080,
            Some(2160),
        );
        assert_eq!(candidate.kind, current.kind);
        assert!(candidate.automatic, "and it is Auto now");
    }

    /// Only a burn is a burn. Off, Native and Overlay are not video
    /// replacements at all, and mapping their track into `subtitle_burn` would
    /// turn every native-subtitle change into a whole new encode recipe.
    #[test]
    fn only_a_burn_reaches_subtitle_burn() {
        let mut current = session_request(SessionKind::Transcode { height: 1080 });
        current.subtitle_burn = Some(2);

        for mode in [
            SubtitleMode::Off,
            SubtitleMode::Native,
            SubtitleMode::Overlay,
        ] {
            let mut selection = selection_at(QualitySelection::Manual { height: 1080 });
            selection.subtitle = SubtitleSelection {
                mode,
                track: if matches!(mode, SubtitleMode::Off) {
                    None
                } else {
                    Some(3)
                },
            };
            let candidate = candidate_request(&current, &selection, 1080, Some(2160));
            assert_eq!(candidate.subtitle_burn, None, "{mode:?} is not a burn");
        }

        let mut burning = selection_at(QualitySelection::Manual { height: 1080 });
        burning.subtitle = SubtitleSelection {
            mode: SubtitleMode::Burn,
            track: Some(3),
        };
        assert_eq!(
            candidate_request(&current, &burning, 1080, Some(2160)).subtitle_burn,
            Some(3),
        );
    }

    /// The client's codec and dynamic-range answers are *policies*, and the
    /// server's are answers. A selection carries the first and cannot carry the
    /// second, so the session's own are carried through untouched — a change on
    /// those axes means re-review, which nothing does yet.
    #[test]
    fn a_selection_cannot_rewrite_the_servers_plan_answers() {
        let mut current = converting_copy();
        current.hdr10 = true;
        let mut selection = selection_at(QualitySelection::Auto);
        selection.codec = CodecPolicy::Av1;
        selection.dynamic_range = DynamicRangePolicy::Sdr;

        let candidate = candidate_request(&current, &selection, 1080, Some(2160));
        assert_eq!(
            GradeIntent::from_request(&candidate),
            GradeIntent::from_request(&current),
            "a policy is not an answer",
        );
        // And everything the selection does not name survives.
        assert_eq!(candidate.file_id, current.file_id);
        assert_eq!(candidate.start_seconds, current.start_seconds);
        assert_eq!(candidate.playback_id, current.playback_id);
    }

    /// The grade intent is read off the request, and a transcode answers both
    /// Dolby Vision questions `false` as a statement rather than a default:
    /// a transcode never carries RPUs through.
    #[test]
    fn a_grade_intent_comes_off_the_request() {
        use crate::transcode::SessionRequest;

        fn request(kind: SessionKind, hdr10: bool) -> SessionRequest {
            SessionRequest {
                file_id: 1,
                playback_id: "player-a".to_owned(),
                request_id: None,
                control_sequence: None,
                automatic: false,
                previous_session_id: None,
                reopen_reason: None,
                kind,
                start_seconds: 0.0,
                audio_index: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                hdr10,
                presentation: crate::transcode::Presentation::Vod,
                block_budget_secs: None,
            }
        }

        assert_eq!(
            GradeIntent::from_request(&request(SessionKind::Transcode { height: 1080 }, true)),
            GradeIntent {
                hdr10: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
        );
        assert_eq!(
            GradeIntent::from_request(&request(
                SessionKind::Copy {
                    aac: true,
                    preserve_dolby_vision: true,
                    convert_dolby_vision: true,
                },
                false,
            )),
            GradeIntent {
                hdr10: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
            },
            "the AAC re-encode is an audio fact and is not a grade fact",
        );
        // Preserving without converting is a third answer, not a rounding of
        // the other two: the RPUs survive but are not rewritten.
        assert_ne!(
            GradeIntent::from_request(&request(
                SessionKind::Copy {
                    aac: false,
                    preserve_dolby_vision: true,
                    convert_dolby_vision: false,
                },
                false,
            )),
            GradeIntent::from_request(&request(
                SessionKind::Copy {
                    aac: false,
                    preserve_dolby_vision: true,
                    convert_dolby_vision: true,
                },
                false,
            )),
        );
    }

    /// The wire vocabulary is the operator's, so it is pinned rather than
    /// merely produced. Nothing else in the tree asserts these strings, and a
    /// swapped pair would make every dashboard say the opposite of the truth
    /// while every other test still passed.
    #[test]
    fn the_decision_vocabulary_is_fixed() {
        assert_eq!(
            PreparationAxis::ResolutionOrBitrate.as_str(),
            "resolution_or_bitrate"
        );
        assert_eq!(
            PreparationAxis::AudioTrackOrOffset.as_str(),
            "audio_track_or_offset"
        );
        assert_eq!(PreparationAxis::SubtitleBurn.as_str(), "subtitle_burn");
        assert_eq!(PreparationAxis::DeliveryMethod.as_str(), "delivery_method");
        assert_eq!(PreparationAxis::DynamicRange.as_str(), "dynamic_range");
        assert_eq!(
            FallbackReason::ClientCannotPrepare.as_str(),
            "client_cannot_prepare"
        );
        assert_eq!(FallbackReason::AxisNotProven.as_str(), "axis_not_proven");
        assert_eq!(FallbackReason::MultipleAxes.as_str(), "multiple_axes");
        assert_eq!(
            FallbackReason::ThroughputUnproven.as_str(),
            "throughput_unproven"
        );
    }

    /// The hardest axis names the transition, by the enum's ordering rather
    /// than by which comparison happens to be written first — so regrouping
    /// those statements cannot silently change what an operator reads.
    #[test]
    fn two_axes_at_once_are_never_prepared_and_the_hardest_names_them() {
        let caps = can_prepare(true);
        let mut method_and_height = playing(1080);
        method_and_height.codec = "source".to_owned();
        let mut audio_and_burn = playing(2160);
        audio_and_burn.audio_track = Some(1);
        audio_and_burn.subtitle_burn = Some(3);
        let mut burn = playing(2160);
        burn.subtitle_burn = Some(3);
        let mut everything = playing(720);
        everything.codec = "source".to_owned();
        everything.audio_track = Some(2);
        everything.subtitle_burn = Some(1);

        for (candidate, grade, axis) in [
            (&method_and_height, sdr(), PreparationAxis::DeliveryMethod),
            (&audio_and_burn, sdr(), PreparationAxis::SubtitleBurn),
            (&burn, hdr10(), PreparationAxis::DynamicRange),
            (&everything, hdr10(), PreparationAxis::DynamicRange),
        ] {
            assert_eq!(
                decide_preparation(
                    view(&playing(2160)),
                    view_at(candidate, grade),
                    Some(&caps),
                    roomy(),
                ),
                PreparationDecision::Fallback {
                    axis,
                    reason: FallbackReason::MultipleAxes,
                },
                "{}",
                axis.as_str(),
            );
        }
    }

    /// A link that has not shown room for a second pipeline does not get one.
    ///
    /// Handoff §8 asks for this by name: the spike ran on a steady shaped
    /// link, so the constrained case is measured by nothing, and a prepared
    /// handoff that causes the stall it exists to prevent is the worst
    /// outcome available.
    #[test]
    fn a_link_that_has_not_shown_headroom_does_not_get_a_second_pipeline() {
        let caps = can_prepare(true);
        let refusals = [
            (
                "no margin at all",
                PreparationConditions {
                    observed_download_bps: Some(12_000_000),
                    delivered_bps: Some(12_000_000),
                },
            ),
            (
                "a hair under twice",
                PreparationConditions {
                    observed_download_bps: Some(23_999_999),
                    delivered_bps: Some(12_000_000),
                },
            ),
            (
                "the client never reported one",
                PreparationConditions {
                    observed_download_bps: None,
                    delivered_bps: Some(12_000_000),
                },
            ),
            (
                "the server has not measured delivery yet",
                PreparationConditions {
                    observed_download_bps: Some(80_000_000),
                    delivered_bps: None,
                },
            ),
        ];
        for (case, conditions) in refusals {
            assert_eq!(
                decide_preparation(
                    view(&playing(2160)),
                    view(&playing(1080)),
                    Some(&caps),
                    conditions,
                ),
                PreparationDecision::Fallback {
                    axis: PreparationAxis::ResolutionOrBitrate,
                    reason: FallbackReason::ThroughputUnproven,
                },
                "{case}",
            );
        }
        // Exactly twice is the floor, and the floor passes.
        assert_eq!(
            decide_preparation(
                view(&playing(2160)),
                view(&playing(1080)),
                Some(&caps),
                PreparationConditions {
                    observed_download_bps: Some(24_000_000),
                    delivered_bps: Some(12_000_000),
                },
            ),
            PreparationDecision::Prepare {
                axis: PreparationAxis::ResolutionOrBitrate,
            },
        );
    }

    /// The delivered grade never votes. It is the encoder's answer, and a
    /// candidate has no encoder — so a session whose HDR10 rung the encoder
    /// refused must not read as a grade crossing on every exchange for the
    /// rest of its life.
    #[test]
    fn the_encoders_answer_is_not_the_grade_the_decision_reads() {
        let caps = can_prepare(true);
        // The session asked for HDR10 and the encoder refused it: delivered
        // says `sdr`, the request still says HDR10.
        let mut refused = playing(2160);
        refused.dynamic_range = Some("sdr".to_owned());
        let mut lower = playing(1080);
        lower.dynamic_range = Some("sdr".to_owned());
        assert_eq!(
            decide_preparation(
                view_at(&refused, hdr10()),
                view_at(&lower, hdr10()),
                Some(&caps),
                roomy(),
            ),
            PreparationDecision::Prepare {
                axis: PreparationAxis::ResolutionOrBitrate,
            },
            "a plain quality change on a refused-HDR10 session is still a plain \
             quality change",
        );
        // And the reverse: two sessions the encoder happened to deliver
        // identically are still a grade change if their requests differ.
        let mut delivered_alike = playing(2160);
        delivered_alike.dynamic_range = Some("sdr".to_owned());
        assert_eq!(
            decide_preparation(
                view_at(&delivered_alike, sdr()),
                view_at(&delivered_alike, hdr10()),
                Some(&caps),
                roomy(),
            ),
            PreparationDecision::Fallback {
                axis: PreparationAxis::DynamicRange,
                reason: FallbackReason::AxisNotProven,
            },
            "the encoder agreeing does not make two different asks the same ask",
        );
    }

    /// The same delivered selection is not a transition, whatever else is
    /// true — and the early return is before every other gate, so a throttled
    /// link and an incapable client both still read `Unchanged`.
    #[test]
    fn an_unchanged_selection_prepares_nothing() {
        let starved = PreparationConditions {
            observed_download_bps: Some(1),
            delivered_bps: Some(12_000_000),
        };
        for caps in [Some(can_prepare(true)), Some(can_prepare(false)), None] {
            for conditions in [roomy(), starved] {
                assert_eq!(
                    decide_preparation(
                        view(&playing(2160)),
                        view(&playing(2160)),
                        caps.as_ref(),
                        conditions,
                    ),
                    PreparationDecision::Unchanged,
                );
            }
        }
    }

    fn staged_preparation(
        incarnation_id: &str,
        predecessor: &str,
        now_ms: i64,
    ) -> plurx_core::domain::MediaSessionPreparation {
        plurx_core::domain::MediaSessionPreparation {
            incarnation_id: incarnation_id.to_owned(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: 7,
            playback_id: "player-a".to_owned(),
            expected_predecessor_incarnation_id: predecessor.to_owned(),
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node-a".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: r#"{"session":"staged"}"#.to_owned(),
            media_origin_ms: 0,
            now_ms,
            // Derived, not literal: `validate_preparation` requires
            // `deadline_ms > now_ms`, and the neighbouring tests in this
            // module take `now_ms` from `unix_ms()`. A constant deadline
            // silently becomes invalid the moment someone copies this helper
            // into a test with a real clock, and it surfaces as a bare
            // `.expect("stage")` panic with no hint of why.
            deadline_ms: now_ms.saturating_add(798_000),
        }
    }

    /// A live playback with one current generation, and the executor pointed
    /// at it. Returns the predecessor's incarnation id alongside the pieces,
    /// because every preparation names it and a preparation with the wrong
    /// predecessor is refused before it reaches the slot.
    async fn preparation_fixture(
        now_ms: i64,
    ) -> (
        String,
        Arc<dyn plurx_core::store::Store>,
        RollingControlHandle,
        PreparationExecutor,
    ) {
        use plurx_core::store::SqliteStore;

        let concrete = SqliteStore::open_in_memory().expect("store");
        let (predecessor, _) = activate_route(&concrete, now_ms, 900_000).await;
        let store: Arc<dyn plurx_core::store::Store> = Arc::new(concrete);
        let control = RollingControlHandle::spawn("session-start");
        let executor = PreparationExecutor::new(
            Arc::clone(&store),
            control.clone(),
            7,
            "player-a".to_owned(),
        );
        (predecessor, store, control, executor)
    }

    /// M6 acceptance 1 — commit durability, on the happy path.
    ///
    /// The claim is not only that the pointer advances. It is that the
    /// executor leaves nothing behind: no ledger row, and no slot the actor is
    /// still holding for a successor it has already become.
    #[tokio::test]
    async fn preparation_executor_commits_a_staged_successor() {
        let now_ms = 2_000;
        let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

        let successor = uuid::Uuid::new_v4().to_string();
        assert!(
            executor
                .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
                .await
                .expect("stage"),
            "a preparation naming the current pointer is admissible"
        );
        assert_eq!(
            store
                .staged_media_session_for_playback(7, "player-a")
                .await
                .expect("ledger")
                .expect("a successor is staged")
                .staged_incarnation_id,
            successor,
        );
        assert_eq!(
            store
                .media_session_route_for_playback(7, "player-a")
                .await
                .expect("route while staged")
                .expect("the playback still has a pointer")
                .incarnation_id,
            predecessor,
            "staging must not advance the pointer",
        );

        assert!(
            executor
                .commit(&successor, now_ms + 200, 900_000)
                .await
                .expect("commit"),
            "the pointer still names the recorded predecessor, so the CAS wins"
        );
        assert_eq!(
            store
                .media_session_route_for_playback(7, "player-a")
                .await
                .expect("route after commit")
                .expect("the playback has a pointer")
                .incarnation_id,
            successor,
        );
        assert!(
            store
                .staged_media_session_for_playback(7, "player-a")
                .await
                .expect("ledger after commit")
                .is_none(),
            "a committed successor is no longer a preparation",
        );
        // The pointer advancing is not the same as the viewer having a
        // stream, and this is the line that keeps the two apart. A commit
        // moves the successor off its preparation deadline and nothing else:
        // the publication fence is still at the sentinel, and arming it is
        // `arm_media_session_handoff`'s job, exactly as it is for an
        // activated replacement.
        let committed = store
            .media_session_route_by_incarnation(&successor)
            .await
            .expect("committed route")
            .expect("the successor exists");
        assert_eq!(committed.lease_expires_at_ms, 900_000);
        assert_eq!(
            committed.publication_ready_at_ms,
            plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            "commit advances the pointer; it does not publish",
        );
        assert!(
            control
                .stage_preparation(uuid::Uuid::new_v4().to_string(), successor.clone())
                .await,
            "the slot is free again, so the next preparation can take it",
        );
    }

    /// M6 acceptance 1, the half that matters — a lost CAS aborts the staged
    /// generation and never reaps the newer player generation that won.
    ///
    /// The pointer is advanced out from under the preparation by an ordinary
    /// activation, which is what a client restart does. The staged successor
    /// must lose, and the generation that replaced its predecessor must still
    /// be current and playable afterwards.
    #[tokio::test]
    async fn preparation_executor_abandons_a_successor_whose_pointer_moved() {
        let now_ms = 2_000;
        let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

        let successor = uuid::Uuid::new_v4().to_string();
        assert!(executor
            .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
            .await
            .expect("stage"));

        let winner = uuid::Uuid::new_v4().to_string();
        let advance = plurx_core::domain::MediaSessionActivation {
            incarnation_id: winner.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: 7,
            playback_id: "player-a".to_owned(),
            expected_predecessor_incarnation_id: Some(predecessor.clone()),
            fence_predecessor: true,
            request_id: None,
            request_fingerprint: "c".repeat(64),
            owner_node_id: "node-a".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: r#"{"session":"advanced"}"#.to_owned(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now_ms + 150,
            lease_expires_at_ms: 900_000,
        };
        store
            .activate_media_session(&advance)
            .await
            .expect("advance the pointer")
            .expect("the advance wins");
        // A replacement carries a predecessor, so its confirmation must name a
        // publication instant a full handoff safety window out. Zero is only
        // valid for a first activation, which is what `activate_route` does.
        store
            .settle_media_session_activation(
                &advance,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: advance
                        .now_ms
                        .saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS),
                },
                advance.now_ms,
            )
            .await
            .expect("confirm the advance")
            .expect("the advance is confirmed");

        assert!(
            !executor
                .commit(&successor, now_ms + 200, 900_000)
                .await
                .expect("commit"),
            "the pointer no longer names the recorded predecessor",
        );
        assert_eq!(
            store
                .media_session_route_for_playback(7, "player-a")
                .await
                .expect("route after the lost CAS")
                .expect("the playback has a pointer")
                .incarnation_id,
            winner,
            "a lost commit must never reap the newer player generation",
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(&winner)
                .await
                .expect("winner route")
                .expect("the winner exists")
                .state,
            "active",
        );
        assert!(
            store
                .staged_media_session_for_playback(7, "player-a")
                .await
                .expect("ledger after the lost CAS")
                .is_none(),
            "the abandoned successor releases the one-per-playback slot",
        );
        // Delisting the ledger row is not tearing the successor down. A row
        // left `active` on its preparation lease counts against the user's
        // admission caps, and the maintenance reaper keys on the *session's*
        // state — so a successor that stays active is never swept at all.
        let abandoned = store
            .media_session_route_by_incarnation(&successor)
            .await
            .expect("abandoned route")
            .expect("the abandoned row is still readable");
        assert_eq!(abandoned.state, "ended");
        assert_eq!(abandoned.terminal_reason.as_deref(), Some("replaced"));
        assert!(
            control
                .stage_preparation(uuid::Uuid::new_v4().to_string(), winner)
                .await,
            "and the actor's slot with it",
        );
    }

    /// M6 acceptance 2 — abort tears down only the successor.
    #[tokio::test]
    async fn preparation_executor_aborts_only_the_successor() {
        let now_ms = 2_000;
        let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

        let successor = uuid::Uuid::new_v4().to_string();
        assert!(executor
            .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
            .await
            .expect("stage"));
        executor
            .abort(&successor, now_ms + 200)
            .await
            .expect("abort");

        let current = store
            .media_session_route_for_playback(7, "player-a")
            .await
            .expect("route after abort")
            .expect("the current stream survives an abort");
        assert_eq!(current.incarnation_id, predecessor);
        assert_eq!(
            current.state, "active",
            "the predecessor stays authoritative and playable",
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(&successor)
                .await
                .expect("successor route")
                .expect("the successor row is still readable")
                .state,
            "ended",
        );
        assert!(store
            .staged_media_session_for_playback(7, "player-a")
            .await
            .expect("ledger after abort")
            .is_none());

        // An owner retrying an abort after a crash reads back the same
        // outcome rather than a spurious loss. Ordered before the slot is
        // reused, because a replay against a *reused* slot is a different
        // property — the one asserted below.
        executor
            .abort(&successor, now_ms + 300)
            .await
            .expect("abort replay");

        let replacement = uuid::Uuid::new_v4().to_string();
        assert!(
            control
                .stage_preparation(replacement.clone(), predecessor)
                .await,
            "the slot is free, so the next preparation can take it",
        );
        // And a stale executor cannot settle the successor it used to hold:
        // identity is checked rather than assumed, or a crashed owner's late
        // abort would free a slot holding something else entirely.
        assert!(
            !control.settle_preparation(&successor, false).await,
            "a stale settle must not free a slot holding a replacement",
        );
        assert!(
            control.may_commit_preparation(&replacement).await,
            "and the replacement is still the successor the actor holds",
        );
    }

    /// The actor's gate is the first of the three authorities, and it is the
    /// only one that can refuse a commit the store would happily accept.
    ///
    /// That is the whole point of asking. Here the pointer still names the
    /// recorded predecessor, so the CAS would win — the commit is stopped
    /// solely because the actor no longer holds this successor, which is what
    /// a viewer disconnecting mid-preparation looks like: `terminate` moves
    /// the slot to aborting and the successor stops being wanted. Without the
    /// gate the pointer would advance to a generation nobody waited for.
    #[tokio::test]
    async fn preparation_executor_refuses_a_commit_the_actor_no_longer_wants() {
        let now_ms = 2_000;
        let (predecessor, store, control, executor) = preparation_fixture(now_ms).await;

        let successor = uuid::Uuid::new_v4().to_string();
        assert!(executor
            .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
            .await
            .expect("stage"));

        // The actor forgets the successor without the store being told.
        assert!(control.settle_preparation(&successor, false).await);
        assert!(!control.may_commit_preparation(&successor).await);

        assert!(
            !executor
                .commit(&successor, now_ms + 200, 900_000)
                .await
                .expect("commit"),
            "the gate refuses before the store is asked",
        );
        assert_eq!(
            store
                .media_session_route_for_playback(7, "player-a")
                .await
                .expect("route after the refused commit")
                .expect("the playback has a pointer")
                .incarnation_id,
            predecessor,
            "a refused commit must not advance the pointer",
        );
        // And the store still holds the successor, untouched: the gate stops
        // short of the durable call rather than tearing anything down. Who
        // reaps it is the disconnect path's problem, and `terminate` aborts
        // the slot it is holding for exactly this reason.
        assert_eq!(
            store
                .staged_media_session_for_playback(7, "player-a")
                .await
                .expect("ledger after the refused commit")
                .expect("the staged row survives a gate refusal")
                .staged_incarnation_id,
            successor,
        );
    }

    /// The durable row is created before the slot is taken, so the slot
    /// refusing must undo the row. Otherwise a successor nobody owns counts
    /// against the user's admission cap until the maintenance backstop runs.
    #[tokio::test]
    async fn preparation_executor_rolls_back_a_row_the_slot_refused() {
        use plurx_core::store::SqliteStore;

        let now_ms = 2_000;
        let concrete = SqliteStore::open_in_memory().expect("store");
        let (predecessor, _) = activate_route(&concrete, now_ms, 900_000).await;
        let store: Arc<dyn plurx_core::store::Store> = Arc::new(concrete);
        let executor = PreparationExecutor::new(
            Arc::clone(&store),
            RollingControlHandle::unavailable_for_test(),
            7,
            "player-a".to_owned(),
        );

        let successor = uuid::Uuid::new_v4().to_string();
        assert!(
            !executor
                .stage(&staged_preparation(&successor, &predecessor, now_ms + 100))
                .await
                .expect("stage"),
            "an actor that cannot be reached cannot take the slot",
        );
        assert!(
            store
                .staged_media_session_for_playback(7, "player-a")
                .await
                .expect("ledger after the refused stage")
                .is_none(),
            "the durable row is undone rather than left for the backstop",
        );
        // An empty ledger alone would also be satisfied by never having made
        // the row, which is the opposite ordering and a different design.
        // The successor's own session row is what says the row existed and
        // was aborted: `replaced` is the abort's signature.
        let rolled_back = store
            .media_session_route_by_incarnation(&successor)
            .await
            .expect("rolled-back route")
            .expect("the durable row was created before the slot was asked");
        assert_eq!(rolled_back.state, "ended");
        assert_eq!(rolled_back.terminal_reason.as_deref(), Some("replaced"));
        assert_eq!(
            store
                .media_session_route_for_playback(7, "player-a")
                .await
                .expect("route after the refused stage")
                .expect("the current stream is untouched")
                .incarnation_id,
            predecessor,
        );
    }

    fn prepublication_actor(now: Instant) -> RollingControlActor {
        let mut actor =
            RollingControlActor::new(now, "session-start", Arc::new(AtomicBool::new(false)));
        actor.prepublication = Some(PrepublicationProducerControl::new());
        actor
    }

    fn retry_recipe(contract: &str, fingerprint: &str) -> ValidatedRetryRecipe {
        ValidatedRetryRecipe::new(
            "cpu-safe".to_owned(),
            fingerprint.to_owned(),
            contract.to_owned(),
            ProducerStartupKind::Software,
        )
    }

    fn hardware_policy(contract: &str, fingerprint: &str) -> InitialProducerPolicy {
        InitialProducerPolicy::hardware(
            contract.to_owned(),
            PRODUCER_PROGRESS_BUDGET,
            retry_recipe(contract, fingerprint),
        )
    }

    fn copy_policy(contract: &str, fingerprint: &str) -> InitialProducerPolicy {
        InitialProducerPolicy::copy(
            contract.to_owned(),
            PRODUCER_PROGRESS_BUDGET,
            Some(
                ValidatedRetryRecipe::new(
                    "legacy-copy".to_owned(),
                    fingerprint.to_owned(),
                    contract.to_owned(),
                    ProducerStartupKind::Copy,
                )
                .with_exit_classifier(ProducerExitClassifier::Immediate),
            ),
        )
    }

    fn registered_prepublication_actor(now: Instant) -> RollingControlActor {
        let mut actor = prepublication_actor(now);
        assert_eq!(actor.register_producer_executor_at(), Ok(()));
        actor
    }

    #[tokio::test(start_paused = true)]
    async fn prepublication_executor_registration_gates_the_initial_policy() {
        let (handle, mut registration) =
            RollingControlHandle::spawn_prepublication_transcode("session-start");
        let policy = hardware_policy("presentation-a", "recipe-a");

        assert_eq!(
            handle.begin_initial_producer_attempt(policy.clone()).await,
            Err(ProducerAttemptRejection::ExecutorNotRegistered)
        );
        assert_eq!(
            registration.next_decision().await,
            RollingProducerExecutorPoll::Unavailable
        );
        assert_eq!(registration.register().await, Ok(()));
        assert_eq!(registration.register().await, Ok(()));
        assert_eq!(handle.begin_initial_producer_attempt(policy).await, Ok(1));
        assert_eq!(
            handle
                .begin_initial_producer_attempt(InitialProducerPolicy::software(
                    "presentation-a".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ))
                .await,
            Err(ProducerAttemptRejection::InitialAttemptAlreadyAdmitted)
        );

        tokio::time::advance(PREPUBLICATION_HARDWARE_STARTUP_BUDGET).await;
        let RollingProducerExecutorPoll::Decision(decision) = registration.next_decision().await
        else {
            panic!("registered executor must receive the retained producer decision");
        };
        assert!(matches!(
            decision.as_ref(),
            ProducerDecision::Retry {
                decision_sequence: 1,
                failed_attempt: 1,
                reason: ProducerDecisionReason::StartupDeadline,
                ..
            }
        ));
    }

    #[test]
    fn prepublication_startup_and_progress_budgets_are_policy_owned() {
        let started = Instant::now();
        let mut hardware = registered_prepublication_actor(started);
        assert_eq!(
            hardware.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-hardware", "recipe-hardware"),
            ),
            Ok(1)
        );
        assert_eq!(
            hardware.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                instant: started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET,
            })
        );

        let mut software = registered_prepublication_actor(started);
        let software_progress_budget = Duration::from_secs(17);
        assert_eq!(
            software.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-software".to_owned(),
                    software_progress_budget,
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            software.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                instant: started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
            })
        );
        assert_eq!(
            software.producer_progress_budget(),
            software_progress_budget
        );

        let mut copy = registered_prepublication_actor(started);
        assert_eq!(
            copy.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy", "recipe-copy"),
            ),
            Ok(1)
        );
        assert_eq!(
            copy.producer_exit_classifier,
            ProducerExitClassifier::CopyReader
        );
        assert_eq!(
            copy.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                instant: started + PREPUBLICATION_COPY_STARTUP_BUDGET,
            })
        );

        let mut copy_without_retry = registered_prepublication_actor(started);
        assert_eq!(
            copy_without_retry.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::copy(
                    "presentation-copy-without-retry".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                    None,
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            copy_without_retry.producer_exit_classifier,
            ProducerExitClassifier::CopyReader
        );

        let mut immediate_copy = registered_prepublication_actor(started);
        assert_eq!(
            immediate_copy.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::copy_immediate(
                    "presentation-copy-immediate".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            immediate_copy.producer_exit_classifier,
            ProducerExitClassifier::Immediate
        );
        assert_eq!(
            immediate_copy.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::Starting,
                instant: started + PREPUBLICATION_COPY_STARTUP_BUDGET,
            })
        );
    }

    #[test]
    fn generation_metadata_preserves_retry_and_obeys_pending_decision_kind() {
        let started = Instant::now();
        let mut retrying = registered_prepublication_actor(started);
        assert_eq!(
            retrying.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-stable", "recipe-stable"),
            ),
            Ok(1)
        );
        let metadata_at = started + Duration::from_secs(1);
        assert_eq!(
            retrying.authorize_response_publication_at(
                metadata_at,
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "presentation-stable".to_owned(),
                ),
            ),
            Ok(RollingResponseAuthorization {
                first_producer_media_publication: false,
            })
        );
        assert_eq!(
            retrying.last_renewal, started,
            "authorization alone cannot renew the generation lease"
        );
        assert!(retrying.commit_generation_metadata_at(
            metadata_at,
            "presentation-stable",
            "generation-metadata-eof",
        ));
        assert_eq!(retrying.last_renewal, metadata_at);
        assert!(!retrying.commit_generation_metadata_at(
            metadata_at + Duration::from_millis(1),
            "wrong-presentation",
            "generation-metadata-eof",
        ));
        assert!(matches!(
            retrying
                .prepublication
                .as_ref()
                .expect("pre-publication state")
                .retry_state,
            PrepublicationRetryState::Available(_)
        ));
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(retrying.settle_due_deadlines_at(deadline).is_some());
        assert!(matches!(
            retrying.pending_decision.as_deref(),
            Some(ProducerDecision::Retry { .. })
        ));
        assert!(retrying
            .authorize_response_publication_at(
                deadline,
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "presentation-stable".to_owned(),
                ),
            )
            .is_ok());
        assert_eq!(retrying.last_renewal, metadata_at);
        assert!(retrying.commit_generation_metadata_at(
            deadline,
            "presentation-stable",
            "generation-metadata-eof",
        ));

        let mut failing = registered_prepublication_actor(started);
        assert_eq!(
            failing.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-fail".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        assert!(failing
            .settle_due_deadlines_at(started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET)
            .is_some());
        assert!(matches!(
            failing.pending_decision.as_deref(),
            Some(ProducerDecision::Fail { .. })
        ));
        assert_eq!(
            failing.authorize_response_publication_at(
                started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "presentation-fail".to_owned(),
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert_eq!(
            failing.authorize_response_publication_at(
                started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
                RollingResponsePublication::protocol_only(
                    RollingResponseObject::ProtocolResponse,
                    1,
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert!(!failing.commit_generation_metadata_at(
            started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET,
            "presentation-fail",
            "generation-metadata-eof",
        ));
    }

    #[test]
    fn compatibility_response_contract_orders_failure_with_publication() {
        let started = Instant::now();
        let failed = Arc::new(AtomicBool::new(false));
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        assert_eq!(
            actor.authorize_response_publication_at(
                started,
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    0,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::ProducerNotAdmitted)
        );
        assert_eq!(
            actor.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::clone(&failed),
            ),
            Ok(())
        );
        assert_eq!(
            actor.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::clone(&failed),
            ),
            Ok(()),
            "the exact immutable binding is idempotent"
        );
        assert_eq!(
            actor.bind_response_publication_contract_at(
                "other-contract".to_owned(),
                Arc::clone(&failed),
            ),
            Err(ResponsePublicationRejection::PresentationContractMismatch)
        );
        assert_eq!(
            actor.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::new(AtomicBool::new(false)),
            ),
            Err(ResponsePublicationRejection::InvalidBinding),
            "the same presentation cannot be rebound to a different failure owner"
        );
        assert!(actor
            .authorize_response_publication_at(
                started + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    0,
                    Some(0),
                ),
            )
            .is_ok());

        failed.store(true, Ordering::Release);
        assert_eq!(
            actor.authorize_response_publication_at(
                started + Duration::from_millis(2),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    0,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::SessionEnded),
            "a compatibility failure ordered first must close actor publication"
        );
        assert!(!actor.commit_media_at(
            started + Duration::from_millis(2),
            "failed-copy-eof",
            0,
            Some(0),
            Some(4_000),
        ));
        assert!(!actor.commit_generation_metadata_at(
            started + Duration::from_millis(2),
            "copy-contract",
            "failed-copy-master-eof",
        ));
    }

    #[test]
    fn prepublication_response_contract_orders_failure_and_policy_binding() {
        let started = Instant::now();
        let failed = Arc::new(AtomicBool::new(false));
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::clone(&failed),
            ),
            Ok(())
        );
        assert_eq!(
            actor.authorize_response_publication_at(
                started,
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "copy-contract".to_owned(),
                ),
            ),
            Err(ResponsePublicationRejection::ProducerNotAdmitted),
            "the failure fence cannot stand in for actor-owned producer policy"
        );
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                copy_policy("copy-contract", "copy-retry"),
            ),
            Ok(1)
        );
        failed.store(true, Ordering::Release);
        assert_eq!(
            actor.authorize_response_publication_at(
                started + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::SessionEnded)
        );
        let control = actor
            .prepublication
            .as_ref()
            .expect("prepublication control");
        assert!(!control.producer_media_published);
        assert!(matches!(
            &control.retry_state,
            PrepublicationRetryState::Available(_)
        ));

        let mut mismatch = registered_prepublication_actor(started);
        assert_eq!(
            mismatch.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::new(AtomicBool::new(false)),
            ),
            Ok(())
        );
        assert_eq!(
            mismatch.begin_initial_producer_attempt_at(
                started,
                copy_policy("other-contract", "other-retry"),
            ),
            Err(ProducerAttemptRejection::PresentationContractMismatch)
        );
        assert_eq!(mismatch.delivery.producer_attempt, 0);

        let failed = Arc::new(AtomicBool::new(true));
        let mut failed_before_policy = registered_prepublication_actor(started);
        assert_eq!(
            failed_before_policy.bind_response_publication_contract_at(
                "copy-contract".to_owned(),
                Arc::clone(&failed),
            ),
            Ok(())
        );
        assert_eq!(
            failed_before_policy.begin_initial_producer_attempt_at(
                started,
                copy_policy("copy-contract", "copy-retry"),
            ),
            Err(ProducerAttemptRejection::SessionEnded)
        );
        assert_eq!(failed_before_policy.delivery.producer_attempt, 0);
        let control = failed_before_policy
            .prepublication
            .as_ref()
            .expect("prepublication control");
        assert!(control.initial_policy.is_none());
        assert!(matches!(
            &control.retry_state,
            PrepublicationRetryState::Unavailable
        ));
    }

    #[tokio::test]
    async fn exact_deadline_attempt_media_wins_then_retains_one_published_failure() {
        let started = rolling_now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-media", "recipe-media"),
            ),
            Ok(1)
        );
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.observe_publication_at(
            deadline - Duration::from_millis(1),
            RollingPublicationObservation {
                producer_attempt: 1,
                playlist_ready: true,
                published_segment: Some(0),
                published_end_ms: Some(6_000),
                next_media_sequence: 1,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            },
        ));
        let handoff = RollingFirstMediaPublicationHandoff::new();
        let handoff_result = handoff.waiter();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: deadline,
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::AuthorizeResponsePublication {
                    publication: RollingResponsePublication::attempt_media(
                        RollingResponseObject::VideoMediaPlaylist,
                        1,
                        None,
                    ),
                    handoff: Some(handoff),
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;
        assert_eq!(
            response.await.expect("publication reply"),
            Ok(RollingResponseAuthorization {
                first_producer_media_publication: true,
            })
        );
        assert!(handoff_result.is_accepted());
        let retained = actor
            .pending_decision
            .clone()
            .expect("exact-deadline publication retains one post-publication failure");
        let ProducerDecision::Fail {
            reason,
            proposal: Some(proposal),
            cleanup,
            ..
        } = retained.as_ref()
        else {
            panic!("published deadline must fail with an immutable proposal");
        };
        assert_eq!(*reason, ProducerDecisionReason::StartupDeadline);
        assert!(uuid::Uuid::parse_str(&proposal.proposal_id).is_ok());
        assert_eq!(cleanup.cleanup_policy, CleanupPolicy::RetainPublished);
        assert!(actor.producer_progress_deadline.is_none());
        assert!(actor.producer_deadline_due.is_none());
        assert!(actor.producer_process_exit_due.is_none());
        assert!(actor
            .settle_due_deadlines_at(deadline + Duration::from_secs(1))
            .is_none());
        assert_eq!(
            actor.observe_producer_exit_at(
                deadline + Duration::from_secs(2),
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(1),
                    signal: None,
                    observed_at: deadline + Duration::from_secs(2),
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
        actor.mark_executor_lost(deadline + Duration::from_secs(3));
        assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
        assert!(actor
            .authorize_response_publication_at(
                deadline + Duration::from_secs(3),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(0),
                ),
            )
            .is_ok());
        assert!(actor
            .authorize_response_publication_at(
                deadline + Duration::from_secs(3),
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "presentation-media".to_owned(),
                ),
            )
            .is_ok());
        assert!(actor.commit_generation_metadata_at(
            deadline + Duration::from_secs(3),
            "presentation-media",
            "generation-metadata-eof",
        ));
        let status = actor.producer_operational_snapshot_at(deadline);
        assert_eq!(status.phase, "producer_ended_with_proposal");
        assert_eq!(status.action_owner, "producer_ended_with_proposal");
        assert!(status.producer_media_published);
        assert!(status.producer_ended_with_proposal);
        assert_eq!(
            status
                .proposal
                .as_ref()
                .map(|proposal| proposal.proposal_id.as_str()),
            Some(proposal.proposal_id.as_str())
        );
    }

    #[test]
    fn first_media_retains_advancing_deadline_and_published_failure_is_immutable() {
        let started = Instant::now();
        let progressed_at = started + Duration::from_secs(1);
        let published_at = started + Duration::from_secs(2);
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-published", "recipe-published"),
            ),
            Ok(1)
        );
        assert!(actor.observe_producer_progress_at(
            progressed_at,
            RollingProducerProgressObservation {
                producer_attempt: 1,
                out_time_ms: Some(1_000),
                speed_milli: Some(1_000),
                recent_speed_milli: Some(1_000),
                observed_at: progressed_at,
            },
        ));
        let advancing = actor
            .producer_progress_deadline
            .expect("advancing deadline before publication");
        assert_eq!(advancing.mode, ProducerProgressDeadlineMode::Advancing);
        assert!(actor.observe_publication_at(
            published_at - Duration::from_millis(1),
            RollingPublicationObservation {
                producer_attempt: 1,
                playlist_ready: true,
                published_segment: Some(1),
                published_end_ms: Some(12_000),
                next_media_sequence: 2,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            },
        ));
        assert!(actor
            .authorize_response_publication_at(
                published_at,
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.producer_progress_deadline,
            Some(advancing),
            "first media must not create an unmonitored published interval"
        );

        assert!(actor.settle_due_deadlines_at(advancing.instant).is_some());
        let retained = actor.pending_decision.clone().expect("published failure");
        let ProducerDecision::Fail {
            decision_sequence: 1,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ProgressDeadline,
            proposal: Some(proposal),
            cleanup,
        } = retained.as_ref()
        else {
            panic!("published deadline emits one retain-published failure");
        };
        assert!(uuid::Uuid::parse_str(&proposal.proposal_id).is_ok());
        assert_eq!(cleanup.cleanup_policy, CleanupPolicy::RetainPublished);
        assert_eq!(
            actor.admit_producer_retry_at(advancing.instant, 1, "recipe-published"),
            Err(ProducerAttemptRejection::RetryUnavailable)
        );
        assert!(actor
            .settle_due_deadlines_at(advancing.instant + Duration::from_secs(30))
            .is_none());
        assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
        assert_eq!(
            actor.authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted),
            "new playlist demand must lose to the retained producer verdict"
        );
        for object in [
            RollingResponseObject::MediaSegment,
            RollingResponseObject::ByteRange,
            RollingResponseObject::NotModified,
        ] {
            assert_eq!(
                actor.authorize_response_publication_at(
                    advancing.instant + Duration::from_millis(1),
                    RollingResponsePublication::attempt_media(object, 1, Some(2)),
                ),
                Err(ResponsePublicationRejection::DecisionCommitted),
                "numeric media beyond the frozen frontier must lose atomically"
            );
        }
        assert!(actor
            .authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::InitializationSegment,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    None,
                ),
            ),
            Err(ResponsePublicationRejection::InvalidBinding)
        );
        assert_eq!(
            actor.authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::SubtitleSegment,
                    1,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::InvalidBinding)
        );
        assert!(actor
            .authorize_response_publication_at(
                advancing.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(0),
                ),
            )
            .is_ok());
        let status = actor.producer_operational_snapshot_at(advancing.instant);
        assert!(status.producer_ended_with_proposal);
        assert_eq!(status.phase, "producer_ended_with_proposal");
        assert_eq!(
            status
                .proposal
                .as_ref()
                .map(|value| value.proposal_id.as_str()),
            Some(proposal.proposal_id.as_str())
        );
        assert!(!actor.producer_signal_authorized);
        assert!(actor.authorized_install.is_none());
        assert_eq!(
            actor.settle_executor_expected_at(),
            Err(ProducerAttemptRejection::DecisionMismatch),
            "an unexecuted RetainPublished decision cannot suppress executor loss"
        );
        assert_eq!(actor.decision_applied_at(1, None), Ok(()));
        assert_eq!(actor.settle_executor_expected_at(), Ok(()));
        actor.mark_executor_lost(advancing.instant + Duration::from_secs(1));
        let settled = actor.producer_operational_snapshot_at(advancing.instant);
        assert_eq!(settled.executor_state, "settled");
        assert_eq!(settled.executor_last_action_failure, None);
        assert!(!actor.executor_lost);
    }

    #[test]
    fn post_failure_observation_cannot_advance_frozen_published_frontier() {
        let started = Instant::now();
        let published_at = started + Duration::from_secs(2);
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-frozen-frontier", "recipe-frozen-frontier"),
            ),
            Ok(1)
        );
        assert!(actor.observe_publication_at(
            published_at - Duration::from_millis(1),
            RollingPublicationObservation {
                producer_attempt: 1,
                playlist_ready: true,
                published_segment: Some(1),
                published_end_ms: Some(12_000),
                next_media_sequence: 2,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            },
        ));
        assert!(actor
            .authorize_response_publication_at(
                published_at,
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());

        let deadline = actor
            .producer_progress_deadline
            .expect("published producer keeps its exact deadline");
        assert!(actor.settle_due_deadlines_at(deadline.instant).is_some());
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                proposal: Some(_),
                cleanup: ProducerFailureCleanup {
                    cleanup_policy: CleanupPolicy::RetainPublished,
                    ..
                },
                ..
            })
        ));
        let frozen = actor.delivery.clone();

        assert!(
            !actor.observe_publication_at(
                deadline.instant + Duration::from_millis(1),
                RollingPublicationObservation {
                    producer_attempt: 1,
                    playlist_ready: true,
                    published_segment: Some(2),
                    published_end_ms: Some(18_000),
                    next_media_sequence: 3,
                    resolved_fetched_segment: None,
                    resolved_fetched_end_ms: None,
                },
            ),
            "a playlist observation sequenced after Fail must lose"
        );
        assert_eq!(actor.delivery.published_segment, frozen.published_segment);
        assert_eq!(actor.delivery.published_end_ms, frozen.published_end_ms);
        assert_eq!(
            actor.delivery.next_media_sequence,
            frozen.next_media_sequence
        );
        assert_eq!(
            actor.authorize_response_publication_at(
                deadline.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(2),
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert!(actor
            .authorize_response_publication_at(
                deadline.instant + Duration::from_millis(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(1),
                ),
            )
            .is_ok());
    }

    #[test]
    fn copy_retry_is_unsupported_only_and_successor_classifier_is_recipe_owned() {
        let started = Instant::now();
        let classified_at = started + Duration::from_secs(1);
        let mut unsupported = registered_prepublication_actor(started);
        assert_eq!(
            unsupported.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-retry", "recipe-copy-retry"),
            ),
            Ok(1)
        );
        assert_eq!(
            unsupported.classify_copy_producer_exit_at(
                classified_at,
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Ok(())
        );
        let Some(ProducerDecision::Retry {
            decision_sequence: 1,
            failed_attempt: 1,
            recipe,
            reason: ProducerDecisionReason::Unsupported,
        }) = unsupported.pending_decision.as_deref()
        else {
            panic!("unsupported copy input must select the one validated fallback");
        };
        assert_eq!(recipe.startup_kind, ProducerStartupKind::Copy);
        assert_eq!(recipe.exit_classifier, ProducerExitClassifier::Immediate);
        assert_eq!(
            unsupported.classify_copy_producer_exit_at(
                classified_at + Duration::from_nanos(1),
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Err(ProducerAttemptRejection::DuplicateClassification)
        );
        assert_eq!(
            unsupported.admit_producer_retry_at(classified_at, 1, "recipe-copy-retry"),
            Ok(2)
        );
        assert_eq!(
            unsupported.producer_exit_classifier,
            ProducerExitClassifier::Immediate,
            "the fallback attempt must use its frozen recipe classifier"
        );
        assert_eq!(
            unsupported.classify_copy_producer_exit_at(
                classified_at + Duration::from_nanos(2),
                1,
                CopyProducerExitClassification::Completed,
            ),
            Err(ProducerAttemptRejection::StaleAttempt)
        );
        assert_eq!(
            unsupported.classify_copy_producer_exit_at(
                classified_at + Duration::from_nanos(2),
                2,
                CopyProducerExitClassification::Completed,
            ),
            Err(ProducerAttemptRejection::ClassifierMismatch)
        );

        for (classification, reason) in [
            (
                CopyProducerExitClassification::InvalidConfiguration,
                ProducerDecisionReason::InvalidConfiguration,
            ),
            (
                CopyProducerExitClassification::ReaderFailed,
                ProducerDecisionReason::ReaderFailed,
            ),
        ] {
            let mut actor = registered_prepublication_actor(started);
            assert_eq!(
                actor.begin_initial_producer_attempt_at(
                    started,
                    copy_policy("presentation-copy-fail", "recipe-copy-fail"),
                ),
                Ok(1)
            );
            assert_eq!(
                actor.classify_copy_producer_exit_at(classified_at, 1, classification),
                Ok(())
            );
            assert!(matches!(
                actor.pending_decision.as_deref(),
                Some(ProducerDecision::Fail {
                    decision_sequence: 1,
                    failed_attempt: 1,
                    reason: actual_reason,
                    ..
                }) if *actual_reason == reason
            ));
        }

        let mut unsupported_without_fallback = registered_prepublication_actor(started);
        assert_eq!(
            unsupported_without_fallback.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::copy(
                    "presentation-copy-no-fallback".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                    None,
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            unsupported_without_fallback.classify_copy_producer_exit_at(
                classified_at,
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Ok(())
        );
        assert!(matches!(
            unsupported_without_fallback.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::Unsupported,
                ..
            })
        ));

        let mut startup_timeout = registered_prepublication_actor(started);
        assert_eq!(
            startup_timeout.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-timeout", "recipe-copy-timeout"),
            ),
            Ok(1)
        );
        assert!(startup_timeout
            .settle_due_deadlines_at(started + PREPUBLICATION_COPY_STARTUP_BUDGET)
            .is_some());
        assert!(matches!(
            startup_timeout.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::StartupDeadline,
                ..
            })
        ));
    }

    #[test]
    fn copy_unsupported_and_nonzero_exit_use_actor_ingress_order() {
        let started = Instant::now();
        let first_fact_at = started + Duration::from_secs(1);
        let second_fact_at = started + Duration::from_secs(2);

        let mut unsupported_first = registered_prepublication_actor(started);
        assert_eq!(
            unsupported_first.begin_initial_producer_attempt_at(
                started,
                copy_policy(
                    "presentation-copy-unsupported-first",
                    "recipe-copy-unsupported-first"
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            unsupported_first.classify_copy_producer_exit_at(
                first_fact_at,
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Ok(())
        );
        assert_eq!(
            unsupported_first.observe_producer_exit_at(
                second_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(32),
                    signal: None,
                    observed_at: second_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert!(matches!(
            unsupported_first.pending_decision.as_deref(),
            Some(ProducerDecision::Retry {
                reason: ProducerDecisionReason::Unsupported,
                ..
            })
        ));

        let mut process_first = registered_prepublication_actor(started);
        assert_eq!(
            process_first.begin_initial_producer_attempt_at(
                started,
                copy_policy(
                    "presentation-copy-process-first",
                    "recipe-copy-process-first"
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            process_first.observe_producer_exit_at(
                first_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(32),
                    signal: None,
                    observed_at: first_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert_eq!(
            process_first.classify_copy_producer_exit_at(
                second_fact_at,
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );
        assert!(matches!(
            process_first.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::ProcessExit,
                ..
            })
        ));
    }

    #[test]
    fn copy_unsupported_retry_closes_only_after_actor_media_admission() {
        let started = Instant::now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                copy_policy(
                    "presentation-copy-published-unsupported",
                    "recipe-copy-published-unsupported",
                ),
            ),
            Ok(1)
        );
        assert!(actor
            .authorize_response_publication_at(
                started + Duration::from_millis(500),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.classify_copy_producer_exit_at(
                started + Duration::from_secs(1),
                1,
                CopyProducerExitClassification::Unsupported,
            ),
            Ok(())
        );
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                failed_attempt: 1,
                reason: ProducerDecisionReason::Unsupported,
                proposal: Some(_),
                cleanup: ProducerFailureCleanup {
                    cleanup_policy: CleanupPolicy::RetainPublished,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn copy_completed_and_success_exit_rendezvous_under_the_first_facts_deadline() {
        let started = Instant::now();
        let first_fact_at = started + Duration::from_secs(1);
        let second_fact_at = started + Duration::from_secs(2);

        let mut completed_first = registered_prepublication_actor(started);
        assert_eq!(
            completed_first.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-complete-a", "recipe-copy-complete-a"),
            ),
            Ok(1)
        );
        assert_eq!(
            completed_first.classify_copy_producer_exit_at(
                first_fact_at,
                1,
                CopyProducerExitClassification::Completed,
            ),
            Ok(())
        );
        let first_deadline = first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
        assert_eq!(
            completed_first.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 1,
                mode: ProducerProgressDeadlineMode::ClassifyingExit,
                instant: first_deadline,
            })
        );
        assert_eq!(
            completed_first.poll_producer_decision_at(0),
            ProducerDecisionPoll::Idle,
            "Completed alone is not a natural-exit proof"
        );
        assert!(completed_first.observe_producer_progress_at(
            first_fact_at + Duration::from_millis(100),
            producer_progress(
                1,
                1_000,
                1_000,
                1_000,
                first_fact_at + Duration::from_millis(100),
            ),
        ));
        assert_eq!(
            completed_first
                .producer_progress_deadline
                .map(|deadline| deadline.instant),
            Some(first_deadline),
            "progress cannot restart a classification rendezvous"
        );
        assert_eq!(
            completed_first.observe_producer_exit_at(
                second_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: second_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        let ProducerDecisionPoll::ClassifyExit(completed_first_probe) =
            completed_first.poll_producer_decision_at(0)
        else {
            panic!("both copy exit halves must publish one completion probe");
        };
        assert_eq!(completed_first_probe.deadline, first_deadline);

        let mut process_first = registered_prepublication_actor(started);
        assert_eq!(
            process_first.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-complete-b", "recipe-copy-complete-b"),
            ),
            Ok(1)
        );
        assert_eq!(
            process_first.observe_producer_exit_at(
                first_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: first_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert_eq!(
            process_first.poll_producer_decision_at(0),
            ProducerDecisionPoll::Idle,
            "a successful copy process exit still needs Completed"
        );
        assert_eq!(
            process_first.classify_copy_producer_exit_at(
                second_fact_at,
                1,
                CopyProducerExitClassification::Completed,
            ),
            Ok(())
        );
        let ProducerDecisionPoll::ClassifyExit(process_first_probe) =
            process_first.poll_producer_decision_at(0)
        else {
            panic!("Completed must rendezvous with the retained successful exit");
        };
        assert_eq!(
            process_first_probe.deadline,
            first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET,
            "the second half cannot mint a fresh classifier budget"
        );
    }

    #[test]
    fn copy_nonzero_exit_wins_completed_and_a_missing_half_times_out() {
        let started = Instant::now();
        let first_fact_at = started + Duration::from_secs(1);
        let second_fact_at = started + Duration::from_secs(2);
        let mut nonzero = registered_prepublication_actor(started);
        assert_eq!(
            nonzero.begin_initial_producer_attempt_at(
                started,
                copy_policy("presentation-copy-nonzero", "recipe-copy-nonzero"),
            ),
            Ok(1)
        );
        assert_eq!(
            nonzero.classify_copy_producer_exit_at(
                first_fact_at,
                1,
                CopyProducerExitClassification::Completed,
            ),
            Ok(())
        );
        assert_eq!(
            nonzero.observe_producer_exit_at(
                second_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(9),
                    signal: None,
                    observed_at: second_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert!(nonzero.maybe_commit_producer_decision_at(second_fact_at));
        assert!(matches!(
            nonzero.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::ProcessExit,
                ..
            })
        ));

        let mut nonzero_first = registered_prepublication_actor(started);
        assert_eq!(
            nonzero_first.begin_initial_producer_attempt_at(
                started,
                copy_policy(
                    "presentation-copy-nonzero-first",
                    "recipe-copy-nonzero-first",
                ),
            ),
            Ok(1)
        );
        assert_eq!(
            nonzero_first.observe_producer_exit_at(
                first_fact_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(7),
                    signal: None,
                    observed_at: first_fact_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        assert_eq!(
            nonzero_first.classify_copy_producer_exit_at(
                second_fact_at,
                1,
                CopyProducerExitClassification::Completed,
            ),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );
        assert!(matches!(
            nonzero_first.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                reason: ProducerDecisionReason::ProcessExit,
                ..
            })
        ));

        for completed_first in [false, true] {
            let mut missing = registered_prepublication_actor(started);
            assert_eq!(
                missing.begin_initial_producer_attempt_at(
                    started,
                    copy_policy("presentation-copy-missing", "recipe-copy-missing"),
                ),
                Ok(1)
            );
            if completed_first {
                assert_eq!(
                    missing.classify_copy_producer_exit_at(
                        first_fact_at,
                        1,
                        CopyProducerExitClassification::Completed,
                    ),
                    Ok(())
                );
            } else {
                assert_eq!(
                    missing.observe_producer_exit_at(
                        first_fact_at,
                        RollingProducerExitObservation {
                            producer_attempt: 1,
                            success: true,
                            code: Some(0),
                            signal: None,
                            observed_at: first_fact_at,
                        },
                    ),
                    ProducerExitAcceptance::Accepted
                );
            }
            assert!(missing
                .settle_due_deadlines_at(first_fact_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET)
                .is_some());
            assert!(matches!(
                missing.pending_decision.as_deref(),
                Some(ProducerDecision::Fail {
                    reason: ProducerDecisionReason::ExitClassificationDeadline,
                    ..
                })
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_copy_classification_is_sequenced_after_preceding_process_exit() {
        let (handle, mut registration) =
            RollingControlHandle::spawn_prepublication_producer("copy-classification");
        assert_eq!(registration.register().await, Ok(()));
        assert_eq!(
            handle
                .begin_initial_producer_attempt(copy_policy(
                    "presentation-copy-sequenced",
                    "recipe-copy-sequenced",
                ))
                .await,
            Ok(1)
        );
        handle.observe_producer_exit(1, true, Some(0), None);
        assert_eq!(
            handle
                .classify_copy_producer_exit_before(
                    1,
                    CopyProducerExitClassification::Completed,
                    rolling_now() + Duration::from_secs(1),
                )
                .await,
            Ok(())
        );
        let RollingProducerExecutorPoll::ClassifyExit(probe) = registration.next_decision().await
        else {
            panic!("the sealed copy result must observe the preceding successful exit");
        };
        assert_eq!(probe.producer_attempt, 1);
        assert!(probe.deadline > rolling_now());
    }

    #[test]
    fn successful_exit_requires_attempt_fenced_completion_before_classification_deadline() {
        let started = Instant::now();
        let exit_at = started + Duration::from_secs(2);
        let mut actor = registered_prepublication_actor(started);
        let policy = InitialProducerPolicy::software(
            "presentation-complete".to_owned(),
            PRODUCER_PROGRESS_BUDGET,
        )
        .with_completion_expectation(Some(20_000), 10_000);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(started, policy),
            Ok(1)
        );
        assert!(actor.observe_producer_progress_at(
            started + Duration::from_secs(1),
            RollingProducerProgressObservation {
                producer_attempt: 1,
                out_time_ms: Some(10_000),
                speed_milli: None,
                recent_speed_milli: None,
                observed_at: started + Duration::from_secs(1),
            },
        ));
        assert!(actor
            .authorize_response_publication_at(
                started + Duration::from_millis(1500),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.observe_producer_exit_at(
                exit_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: exit_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        let ProducerDecisionPoll::ClassifyExit(probe) = actor.poll_producer_decision_at(0) else {
            panic!("successful exit must wake one exact classifier probe");
        };
        assert_eq!(probe.producer_attempt, 1);
        assert_eq!(
            probe.deadline,
            exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET
        );
        assert_eq!(
            actor.classify_producer_exit_at(
                exit_at + Duration::from_millis(1),
                RollingProducerCompletionEvidence {
                    probe_sequence: probe.probe_sequence,
                    producer_attempt: 1,
                    end_list: true,
                    final_segment: Some(1),
                    final_end_ms: Some(20_000),
                },
            ),
            RollingProducerCompletionDisposition::CompleteVerifiedDuration
        );
        assert_eq!(
            actor.poll_producer_decision_at(0),
            ProducerDecisionPoll::Idle
        );
        assert!(actor.pending_decision.is_none());
        let status = actor.producer_operational_snapshot_at(exit_at + Duration::from_secs(1));
        assert_eq!(status.phase, "complete");
        assert_eq!(status.completion, "complete_verified_duration");
        assert_eq!(status.completion_attempt, Some(1));
        assert_eq!(status.completion_final_segment, Some(1));
        assert_eq!(status.completion_final_end_ms, Some(20_000));
        assert!(!status.producer_ended_with_proposal);
        assert_eq!(status.last_probe_outcome, "published");
        assert!(!actor.observe_publication_at(
            exit_at + Duration::from_millis(2),
            RollingPublicationObservation {
                producer_attempt: 1,
                playlist_ready: true,
                published_segment: Some(2),
                published_end_ms: Some(26_000),
                next_media_sequence: 3,
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            },
        ));
        assert_eq!(
            actor.classify_producer_exit_at(
                exit_at + Duration::from_millis(3),
                RollingProducerCompletionEvidence {
                    probe_sequence: probe.probe_sequence,
                    producer_attempt: 1,
                    end_list: false,
                    final_segment: None,
                    final_end_ms: None,
                },
            ),
            RollingProducerCompletionDisposition::Stale
        );
        assert_eq!(
            actor
                .producer_operational_snapshot_at(exit_at + Duration::from_secs(1))
                .completion,
            "complete_verified_duration"
        );
        assert_eq!(
            actor
                .producer_operational_snapshot_at(exit_at + Duration::from_secs(1))
                .last_probe_outcome,
            "published"
        );
        assert_eq!(actor.settle_executor_expected_at(), Ok(()));
        actor.mark_executor_lost(exit_at + Duration::from_secs(2));
        let settled = actor.producer_operational_snapshot_at(exit_at + Duration::from_secs(2));
        assert_eq!(settled.executor_state, "settled");
        assert_eq!(settled.executor_last_action_failure, None);
        assert!(!actor.executor_lost);
    }

    #[test]
    fn missing_completion_evidence_yields_one_retain_published_proposal() {
        let started = Instant::now();
        let exit_at = started + Duration::from_secs(1);
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-partial".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        assert!(actor
            .authorize_response_publication_at(
                started + Duration::from_millis(500),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.observe_producer_exit_at(
                exit_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: exit_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        let deadline = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
        assert!(actor.settle_due_deadlines_at(deadline).is_some());
        let retained = actor
            .pending_decision
            .clone()
            .expect("classification failure");
        assert!(matches!(
            retained.as_ref(),
            ProducerDecision::Fail {
                reason: ProducerDecisionReason::ExitClassificationDeadline,
                proposal: Some(_),
                cleanup: ProducerFailureCleanup {
                    cleanup_policy: CleanupPolicy::RetainPublished,
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            actor
                .prepublication
                .as_ref()
                .expect("producer control")
                .last_probe_outcome,
            ProducerProbeOutcome::Cancelled
        );
        assert_eq!(actor.pending_decision.as_ref(), Some(&retained));
    }

    #[test]
    fn incomplete_exit_evidence_fails_once_without_waiting_for_the_deadline() {
        let started = Instant::now();
        let exit_at = started + Duration::from_secs(1);
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-partial-now".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        assert!(actor
            .authorize_response_publication_at(
                started + Duration::from_millis(500),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::VideoMediaPlaylist,
                    1,
                    None,
                ),
            )
            .is_ok());
        assert_eq!(
            actor.observe_producer_exit_at(
                exit_at,
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: exit_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        let ProducerDecisionPoll::ClassifyExit(probe) = actor.poll_producer_decision_at(0) else {
            panic!("successful exit must expose its exact evidence request");
        };
        assert_eq!(
            actor.classify_producer_exit_at(
                exit_at + Duration::from_millis(1),
                RollingProducerCompletionEvidence {
                    probe_sequence: probe.probe_sequence,
                    producer_attempt: 1,
                    end_list: false,
                    final_segment: Some(0),
                    final_end_ms: Some(6_000),
                },
            ),
            RollingProducerCompletionDisposition::FailedPartialSuccess
        );
        let retained = actor
            .pending_decision
            .clone()
            .expect("partial success verdict");
        assert!(matches!(
            retained.as_ref(),
            ProducerDecision::Fail {
                decision_sequence: 1,
                failed_attempt: 1,
                reason: ProducerDecisionReason::PartialSuccessExit,
                proposal: Some(_),
                cleanup: ProducerFailureCleanup {
                    cleanup_policy: CleanupPolicy::RetainPublished,
                    ..
                },
            }
        ));
        assert_eq!(
            actor.poll_producer_decision_at(0),
            ProducerDecisionPoll::Decision(retained)
        );
    }

    #[tokio::test]
    async fn generation_and_protocol_responses_are_due_first_at_exact_deadline() {
        let started = rolling_now();
        let deadline = started + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET;
        for publication in [
            RollingResponsePublication::generation_metadata(
                RollingResponseObject::MasterPlaylist,
                "presentation-due-first".to_owned(),
            ),
            RollingResponsePublication::protocol_only(RollingResponseObject::ProtocolResponse, 1),
        ] {
            let mut actor = registered_prepublication_actor(started);
            assert_eq!(
                actor.begin_initial_producer_attempt_at(
                    started,
                    InitialProducerPolicy::software(
                        "presentation-due-first".to_owned(),
                        PRODUCER_PROGRESS_BUDGET,
                    ),
                ),
                Ok(1)
            );
            let (reply, response) = tokio::sync::oneshot::channel();
            actor
                .handle_command(RollingControlEnvelope {
                    sequence: 1,
                    published_at: deadline,
                    preceding_producer: Vec::new(),
                    sealed_flow_barriers: 0,
                    command: RollingControlCommand::AuthorizeResponsePublication {
                        publication,
                        handoff: None,
                        deadline: rolling_now() + Duration::from_secs(1),
                        reply,
                    },
                })
                .await;
            assert_eq!(
                response.await.expect("response admission reply"),
                Err(ResponsePublicationRejection::DecisionCommitted)
            );
            assert!(matches!(
                actor.pending_decision.as_deref(),
                Some(ProducerDecision::Fail {
                    reason: ProducerDecisionReason::StartupDeadline,
                    ..
                })
            ));
        }
    }

    #[tokio::test]
    async fn cancelled_attempt_media_admission_cannot_mutate_or_win_the_deadline() {
        let started = rolling_now();
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-cancelled", "recipe-cancelled"),
            ),
            Ok(1)
        );
        let handoff = RollingFirstMediaPublicationHandoff::new();
        let handoff_result = handoff.waiter();
        let (reply, response) = tokio::sync::oneshot::channel();
        drop(response);
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: deadline,
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::AuthorizeResponsePublication {
                    publication: RollingResponsePublication::attempt_media(
                        RollingResponseObject::VideoMediaPlaylist,
                        1,
                        None,
                    ),
                    handoff: Some(handoff),
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;
        assert!(!handoff_result.is_accepted());
        assert!(
            !actor
                .prepublication
                .as_ref()
                .expect("pre-publication state")
                .producer_media_published
        );
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Retry {
                reason: ProducerDecisionReason::StartupDeadline,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn cancelled_or_expired_response_commits_do_not_mutate_actor_delivery() {
        let started = rolling_now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        let before = actor.snapshot_at(started);

        let (reply, response) = tokio::sync::oneshot::channel();
        drop(response);
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: started + Duration::from_millis(1),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::CommitMedia {
                    kind: "cancelled-response-eof",
                    producer_attempt: attempt,
                    segment_index: Some(7),
                    segment_end_ms: Some(28_000),
                    handoff: None,
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 2,
                published_at: started + Duration::from_millis(2),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::CommitMedia {
                    kind: "expired-response-eof",
                    producer_attempt: attempt,
                    segment_index: Some(8),
                    segment_end_ms: Some(32_000),
                    handoff: None,
                    deadline: rolling_now() - Duration::from_millis(1),
                    reply,
                },
            })
            .await;
        assert!(!response.await.expect("expired commit reply"));
        let after = actor.snapshot_at(started + Duration::from_millis(2));
        assert_eq!(after.last_renewal_kind, before.last_renewal_kind);
        assert_eq!(
            after.delivery.fetched_segment,
            before.delivery.fetched_segment
        );
        assert_eq!(
            after.delivery.fetched_end_ms,
            before.delivery.fetched_end_ms
        );
    }

    #[tokio::test]
    async fn accepted_segment_eof_projects_frontier_and_flow_before_reply() {
        let started = rolling_now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        let compatibility_attempt = Arc::new(std::sync::Mutex::new(attempt));
        let high_segment = Arc::new(AtomicI64::new(-1));
        let fetched_end_ms = Arc::new(AtomicI64::new(0));
        let flow_sync = Arc::new(RollingFlowSync::new());
        let handoff = RollingMediaCommitHandoff {
            producer_attempt: attempt,
            segment_index: 7,
            segment_end_ms: Some(28_000),
            compatibility_attempt,
            high_segment: Arc::clone(&high_segment),
            fetched_end_ms: Arc::clone(&fetched_end_ms),
            flow_sync: Arc::clone(&flow_sync),
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: started + Duration::from_millis(1),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::CommitMedia {
                    kind: "accepted-response-eof",
                    producer_attempt: attempt,
                    segment_index: Some(7),
                    segment_end_ms: Some(28_000),
                    handoff: Some(handoff),
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;

        assert_eq!(high_segment.load(Ordering::Acquire), 7);
        assert_eq!(fetched_end_ms.load(Ordering::Acquire), 28_000);
        assert_eq!(flow_sync.requested.load(Ordering::Acquire), 1);
        drop(response);
        assert_eq!(high_segment.load(Ordering::Acquire), 7);
        assert_eq!(
            actor
                .snapshot_at(started + Duration::from_millis(1))
                .delivery
                .fetched_segment,
            Some(7)
        );
    }

    #[tokio::test]
    async fn first_media_deadline_rejects_and_fences_a_late_handoff() {
        let (control, mut registration) =
            RollingControlHandle::spawn_prepublication_transcode("first-media-deadline");
        assert_eq!(registration.register().await, Ok(()));
        assert_eq!(
            control
                .begin_initial_producer_attempt(hardware_policy(
                    "presentation-first-media-deadline",
                    "recipe-first-media-deadline",
                ))
                .await,
            Ok(1)
        );
        let handoff = RollingFirstMediaPublicationHandoff::new();
        let waiter = handoff.waiter();

        assert!(!control.settle_first_media_at_deadline(&waiter));
        assert!(control.is_retired());
        handoff.settle_for_test(true);
        assert_eq!(waiter.settled_outcome(), Some(false));

        // This represents a response that already resolved the exact owner
        // and reached actor admission after the request ahead of it timed out.
        // The next actor command must import the external retirement fence,
        // not let a fresh handoff revive first-media publication.
        let follower_handoff = RollingFirstMediaPublicationHandoff::new();
        let follower_waiter = follower_handoff.waiter();
        assert_eq!(
            control
                .authorize_response_publication(
                    RollingResponsePublication::attempt_media(
                        RollingResponseObject::VideoMediaPlaylist,
                        1,
                        None,
                    ),
                    Some(follower_handoff),
                    rolling_now() + Duration::from_secs(1),
                )
                .await,
            Err(ResponsePublicationRejection::SessionEnded)
        );
        assert_eq!(follower_waiter.settled_outcome(), Some(false));

        let Ok(RollingExpiryClaim::Retired(snapshot)) = control.claim_expiry().await else {
            panic!("external first-media retirement must become actor terminal state");
        };
        assert_eq!(
            snapshot.terminal,
            Some(RollingTerminalCause::AuthorityFence)
        );
        assert!(!snapshot.producer_control.producer_media_published);
    }

    #[tokio::test]
    async fn terminal_queued_before_deadline_settles_after_deadline_without_reclassification() {
        let handle = RollingControlHandle::spawn("queued-terminal-deadline");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_producer_attempt_reply(Arc::clone(&pause));
        let begin = tokio::spawn({
            let handle = handle.clone();
            async move { handle.begin_producer_attempt().await }
        });
        pause.wait().await;

        let deadline = rolling_now() + Duration::from_millis(100);
        let ending = tokio::spawn({
            let handle = handle.clone();
            async move { handle.end_before(deadline).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while handle.sender.capacity() == ROLLING_ACTOR_MAILBOX_CAPACITY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Terminal must enter the mailbox before its deadline");
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline + Duration::from_millis(20),
        ))
        .await;
        assert!(
            !ending.is_finished(),
            "an admitted Terminal waits for actor settlement instead of timing out"
        );

        pause.wait().await;
        assert!(begin.await.expect("begin task").is_ok());
        assert_eq!(
            ending.await.expect("End task"),
            Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
        );
    }

    #[tokio::test]
    async fn cancelled_or_expired_http_observations_do_not_mutate_actor_delivery() {
        let started = rolling_now();
        let mut actor =
            RollingControlActor::new(started, "session-start", Arc::new(AtomicBool::new(false)));
        let attempt = actor
            .begin_producer_attempt_at(started)
            .expect("producer attempt");
        let observed_at = started + Duration::from_millis(2);
        let before = actor.snapshot_at(observed_at).delivery;

        let (reply, response) = tokio::sync::oneshot::channel();
        drop(response);
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: started + Duration::from_millis(1),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::ObservePublication {
                    observation: publication(attempt, true, 7, 28_000, None),
                    deadline: Some(rolling_now() + Duration::from_secs(1)),
                    reply,
                },
            })
            .await;

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 2,
                published_at: started + Duration::from_millis(2),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::ObservePublication {
                    observation: publication(attempt, true, 8, 32_000, None),
                    deadline: Some(rolling_now() - Duration::from_millis(1)),
                    reply,
                },
            })
            .await;
        assert!(!response.await.expect("expired observation reply"));
        assert_eq!(actor.snapshot_at(observed_at).delivery, before);
    }

    #[tokio::test]
    async fn cancellation_after_attempt_media_mutation_keeps_the_synchronous_handoff() {
        let started = rolling_now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-handoff", "recipe-handoff"),
            ),
            Ok(1)
        );
        let handoff = RollingFirstMediaPublicationHandoff::new();
        let handoff_result = handoff.waiter();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: started + Duration::from_secs(1),
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::AuthorizeResponsePublication {
                    publication: RollingResponsePublication::attempt_media(
                        RollingResponseObject::VideoMediaPlaylist,
                        1,
                        None,
                    ),
                    handoff: Some(handoff),
                    deadline: rolling_now() + Duration::from_secs(1),
                    reply,
                },
            })
            .await;
        drop(response);
        assert!(handoff_result.is_accepted());
        assert!(
            actor
                .prepublication
                .as_ref()
                .expect("pre-publication state")
                .producer_media_published
        );
        assert!(actor.pending_decision.is_none());
    }

    #[tokio::test]
    async fn applied_failure_consumes_a_preceding_exit_and_replays_permanently() {
        let started = rolling_now();
        let exit_at = started + Duration::from_secs(1);
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy::software(
                    "presentation-applied-fail".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Ok(1)
        );
        let exit = RollingProducerIngressBlock::Barrier(SequencedProducerBarrier {
            preceding_progress: None,
            event: SequencedProducerEvent {
                sequence: 1,
                published_at: exit_at,
                event: RollingProducerEvent::Exit(RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(1),
                    signal: None,
                    observed_at: exit_at,
                }),
            },
        });
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 2,
                published_at: exit_at,
                preceding_producer: vec![exit],
                sealed_flow_barriers: 0,
                command: RollingControlCommand::DecisionApplied {
                    decision_sequence: 1,
                    installed_attempt: None,
                    reply,
                },
            })
            .await;
        assert_eq!(response.await.expect("decision applied reply"), Ok(()));
        assert_eq!(actor.decision_applied_at(1, None), Ok(()));
        assert!(actor.pending_decision.is_none());
        assert!(actor.producer_progress_deadline.is_none());
        assert!(actor.producer_deadline_due.is_none());
        assert!(actor.producer_process_exit_due.is_none());
        assert!(
            actor
                .prepublication
                .as_ref()
                .expect("pre-publication state")
                .failure_applied
        );
        actor.arm_producer_deadline(
            1,
            ProducerProgressDeadlineMode::Advancing,
            exit_at + PRODUCER_PROGRESS_BUDGET,
        );
        assert!(actor.producer_progress_deadline.is_none());
        assert_eq!(
            actor.authorize_response_publication_at(
                exit_at,
                RollingResponsePublication::protocol_only(
                    RollingResponseObject::ProtocolResponse,
                    1,
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert_eq!(
            actor.authorize_response_publication_at(
                exit_at,
                RollingResponsePublication::attempt_status(RollingResponseObject::MediaSegment, 1,),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
    }

    #[test]
    fn attempt_media_and_retry_are_exact_first_winner_races() {
        let started = Instant::now();
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;

        let mut decision_first = registered_prepublication_actor(started);
        assert_eq!(
            decision_first.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-race-a", "recipe-race-a"),
            ),
            Ok(1)
        );
        assert!(decision_first.settle_due_deadlines_at(deadline).is_some());
        assert_eq!(
            decision_first.authorize_response_publication_at(
                deadline,
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );

        let mut publication_late = registered_prepublication_actor(started);
        assert_eq!(
            publication_late.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-race-b", "recipe-race-b"),
            ),
            Ok(1)
        );
        assert_eq!(
            publication_late.authorize_response_publication_at(
                deadline + Duration::from_nanos(1),
                RollingResponsePublication::attempt_media(
                    RollingResponseObject::MediaSegment,
                    1,
                    Some(0),
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert!(matches!(
            publication_late.pending_decision.as_deref(),
            Some(ProducerDecision::Retry { .. })
        ));
    }

    #[test]
    fn retry_admission_and_application_are_exact_replay_fenced() {
        let started = Instant::now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-retry", "recipe-retry"),
            ),
            Ok(1)
        );
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.settle_due_deadlines_at(deadline).is_some());
        assert_eq!(
            actor.admit_producer_retry_at(deadline, 2, "recipe-retry"),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );
        assert_eq!(
            actor.admit_producer_retry_at(deadline, 1, "wrong-recipe"),
            Err(ProducerAttemptRejection::RecipeMismatch)
        );
        actor.delivery.playlist_ready = true;
        assert_eq!(
            actor.admit_producer_retry_at(deadline, 1, "recipe-retry"),
            Ok(2),
            "catalog readiness is not client response publication"
        );
        assert_eq!(
            actor.admit_producer_retry_at(deadline, 1, "recipe-retry"),
            Ok(2),
            "exact retry admission is replayable"
        );
        assert_eq!(
            actor.admit_producer_retry_at(deadline, 1, "wrong-recipe"),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );
        assert_eq!(
            actor.decision_applied_at(1, Some(1)),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );
        assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
        assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
        assert_eq!(
            actor.decision_applied_at(1, None),
            Err(ProducerAttemptRejection::DecisionMismatch)
        );

        let invalid_recipe = retry_recipe("different-contract", "invalid-recipe");
        let mut invalid = registered_prepublication_actor(started);
        assert_eq!(
            invalid.begin_initial_producer_attempt_at(
                started,
                InitialProducerPolicy {
                    startup_kind: ProducerStartupKind::Hardware,
                    progress_budget: PRODUCER_PROGRESS_BUDGET,
                    retry_recipe: Some(invalid_recipe),
                    presentation_contract_fingerprint: "presentation-retry".to_owned(),
                    exit_classifier: ProducerExitClassifier::Immediate,
                    retry_eligibility: ProducerRetryEligibility::AnyPrepublicationFailure,
                    expected_remaining_ms: None,
                    completion_tolerance_ms: 10_000,
                },
            ),
            Err(ProducerAttemptRejection::InvalidPolicy)
        );
    }

    #[test]
    fn successor_failure_is_final_and_cannot_mint_a_second_retry() {
        let started = Instant::now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-successor", "recipe-successor"),
            ),
            Ok(1)
        );
        let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
        assert_eq!(
            actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor"),
            Ok(2)
        );
        let successor_deadline = first_deadline + PREPUBLICATION_SOFTWARE_STARTUP_BUDGET;
        let expected_successor_deadline = Some(ProducerProgressDeadline {
            producer_attempt: 2,
            mode: ProducerProgressDeadlineMode::Starting,
            instant: successor_deadline,
        });
        assert_eq!(
            actor.producer_progress_deadline, expected_successor_deadline,
            "the successor owns its startup clock while Retry remains pending"
        );
        assert_eq!(actor.decision_applied_at(1, Some(2)), Ok(()));
        assert_eq!(
            actor.producer_progress_deadline, expected_successor_deadline,
            "DecisionApplied must preserve the admitted successor clock"
        );
        assert!(actor.settle_due_deadlines_at(successor_deadline).is_some());
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                decision_sequence: 2,
                failed_attempt: 2,
                reason: ProducerDecisionReason::StartupDeadline,
                ..
            })
        ));
        assert_eq!(
            actor.admit_producer_retry_at(successor_deadline, 2, "recipe-successor"),
            Err(ProducerAttemptRejection::RetryUnavailable)
        );
        assert_eq!(actor.decision_applied_at(2, None), Ok(()));
        assert_eq!(actor.decision_applied_at(2, None), Ok(()));
    }

    #[tokio::test]
    async fn successor_progress_before_retry_ack_advances_and_preserves_its_exact_deadline() {
        let started = Instant::now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy(
                    "presentation-successor-progress",
                    "recipe-successor-progress"
                ),
            ),
            Ok(1)
        );
        let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
        assert_eq!(
            actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor-progress"),
            Ok(2)
        );

        let progressed_at = first_deadline + Duration::from_secs(1);
        actor.producer_events.publish_at(
            RollingProducerEvent::Progress(producer_progress(
                2,
                1_000,
                1_000,
                1_000,
                progressed_at,
            )),
            false,
            progressed_at,
        );
        let preceding_producer = actor.producer_events.drain_blocks();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 2,
                published_at: progressed_at,
                preceding_producer,
                sealed_flow_barriers: 0,
                command: RollingControlCommand::DecisionApplied {
                    decision_sequence: 1,
                    installed_attempt: Some(2),
                    reply,
                },
            })
            .await;
        assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));

        let advancing = Some(ProducerProgressDeadline {
            producer_attempt: 2,
            mode: ProducerProgressDeadlineMode::Advancing,
            instant: progressed_at + PRODUCER_PROGRESS_BUDGET,
        });
        assert_eq!(
            actor.producer_progress_deadline, advancing,
            "timely successor progress must replace Starting before Retry is acknowledged"
        );
        assert_eq!(actor.delivery.producer_out_time_ms, Some(1_000));
        assert!(!actor.observe_producer_progress_at(
            progressed_at + Duration::from_nanos(1),
            producer_progress(
                1,
                99_000,
                9_000,
                9_000,
                progressed_at + Duration::from_nanos(1),
            ),
        ));
        assert_eq!(
            actor.producer_progress_deadline, advancing,
            "stale predecessor progress cannot disturb the successor clock"
        );
    }

    #[tokio::test]
    async fn successful_successor_exit_before_retry_ack_keeps_its_exact_classifier() {
        fn pending_successor(started: Instant, suffix: &str) -> (RollingControlActor, Instant) {
            let mut actor = registered_prepublication_actor(started);
            let presentation = format!("presentation-successor-success-{suffix}");
            let recipe = format!("recipe-successor-success-{suffix}");
            assert_eq!(
                actor.begin_initial_producer_attempt_at(
                    started,
                    hardware_policy(&presentation, &recipe),
                ),
                Ok(1)
            );
            let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
            assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
            assert_eq!(
                actor.admit_producer_retry_at(first_deadline, 1, &recipe),
                Ok(2)
            );
            (actor, first_deadline)
        }

        async fn acknowledge_after_successful_exit(
            actor: &mut RollingControlActor,
            exit_at: Instant,
        ) {
            actor.producer_events.publish_at(
                RollingProducerEvent::Exit(RollingProducerExitObservation {
                    producer_attempt: 2,
                    success: true,
                    code: Some(0),
                    signal: None,
                    observed_at: exit_at,
                }),
                true,
                exit_at,
            );
            actor.producer_events.publish_at(
                RollingProducerEvent::Exit(RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(99),
                    signal: None,
                    observed_at: exit_at + Duration::from_nanos(1),
                }),
                true,
                exit_at + Duration::from_nanos(1),
            );
            let preceding_producer = actor.producer_events.drain_blocks();
            let (reply, response) = tokio::sync::oneshot::channel();
            actor
                .handle_command(RollingControlEnvelope {
                    sequence: 2,
                    published_at: exit_at,
                    preceding_producer,
                    sealed_flow_barriers: 0,
                    command: RollingControlCommand::DecisionApplied {
                        decision_sequence: 1,
                        installed_attempt: Some(2),
                        reply,
                    },
                })
                .await;
            assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));
        }

        let started = Instant::now();
        let (mut completed, first_deadline) = pending_successor(started, "complete");
        let exit_at = first_deadline + Duration::from_secs(1);
        acknowledge_after_successful_exit(&mut completed, exit_at).await;
        let classification_deadline = exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
        assert_eq!(
            completed.producer_progress_deadline,
            Some(ProducerProgressDeadline {
                producer_attempt: 2,
                mode: ProducerProgressDeadlineMode::ClassifyingExit,
                instant: classification_deadline,
            })
        );
        let ProducerDecisionPoll::ClassifyExit(probe) = completed.poll_producer_decision_at(0)
        else {
            panic!("successful successor exit must retain its exact classifier");
        };
        assert_eq!(probe.producer_attempt, 2);
        assert_eq!(probe.deadline, classification_deadline);
        assert_eq!(
            completed.observe_producer_exit_at(
                exit_at + Duration::from_nanos(2),
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(88),
                    signal: None,
                    observed_at: exit_at + Duration::from_nanos(2),
                },
            ),
            ProducerExitAcceptance::Rejected
        );
        assert_eq!(
            completed.poll_producer_decision_at(0),
            ProducerDecisionPoll::ClassifyExit(probe),
            "stale predecessor exit cannot replace the successor classifier"
        );
        assert_eq!(
            completed.classify_producer_exit_at(
                exit_at + Duration::from_millis(1),
                RollingProducerCompletionEvidence {
                    probe_sequence: probe.probe_sequence,
                    producer_attempt: 2,
                    end_list: true,
                    final_segment: Some(0),
                    final_end_ms: Some(6_000),
                },
            ),
            RollingProducerCompletionDisposition::CompleteUnverifiedDuration
        );
        assert_eq!(
            completed.poll_producer_decision_at(0),
            ProducerDecisionPoll::Idle
        );
        assert_eq!(
            completed
                .prepublication
                .as_ref()
                .expect("producer control")
                .completion,
            ProducerCompletionState::CompleteUnverifiedDuration {
                producer_attempt: 2,
                final_segment: 0,
                final_end_ms: 6_000,
            }
        );

        let late_started = started + Duration::from_secs(1);
        let (mut late, late_first_deadline) = pending_successor(late_started, "deadline");
        let late_exit_at = late_first_deadline + Duration::from_secs(1);
        acknowledge_after_successful_exit(&mut late, late_exit_at).await;
        let late_classification_deadline = late_exit_at + PRODUCER_EXIT_CLASSIFICATION_BUDGET;
        assert!(late
            .settle_due_deadlines_at(late_classification_deadline)
            .is_some());
        assert!(matches!(
            late.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                decision_sequence: 2,
                failed_attempt: 2,
                reason: ProducerDecisionReason::ExitClassificationDeadline,
                ..
            })
        ));
        assert_eq!(
            late.admit_producer_retry_at(
                late_classification_deadline,
                2,
                "recipe-successor-success-deadline",
            ),
            Err(ProducerAttemptRejection::RetryUnavailable)
        );
    }

    #[tokio::test]
    async fn successor_exit_before_retry_ack_is_retained_as_the_final_decision() {
        let started = Instant::now();
        let mut actor = registered_prepublication_actor(started);
        assert_eq!(
            actor.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-successor-exit", "recipe-successor-exit"),
            ),
            Ok(1)
        );
        let first_deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(actor.settle_due_deadlines_at(first_deadline).is_some());
        assert!(
            !actor.pending_retry_precedes_successor(1),
            "a pending Retry never exempts its failed predecessor"
        );
        assert_eq!(
            actor.admit_producer_retry_at(first_deadline, 1, "recipe-successor-exit"),
            Ok(2)
        );
        assert!(actor.pending_retry_precedes_successor(2));

        let exit_at = first_deadline + Duration::from_secs(1);
        assert_eq!(
            actor.observe_producer_exit_at(
                exit_at,
                RollingProducerExitObservation {
                    producer_attempt: 2,
                    success: false,
                    code: Some(23),
                    signal: None,
                    observed_at: exit_at,
                },
            ),
            ProducerExitAcceptance::Accepted
        );
        let successor_exit = Some(ProducerProcessExitDue {
            producer_attempt: 2,
            published_at: exit_at,
            code: Some(23),
            signal: None,
        });
        assert_eq!(actor.producer_process_exit_due, successor_exit);
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Retry {
                decision_sequence: 1,
                failed_attempt: 1,
                ..
            })
        ));

        assert_eq!(
            actor.observe_producer_exit_at(
                exit_at + Duration::from_nanos(1),
                RollingProducerExitObservation {
                    producer_attempt: 1,
                    success: false,
                    code: Some(99),
                    signal: None,
                    observed_at: exit_at + Duration::from_nanos(1),
                },
            ),
            ProducerExitAcceptance::Rejected
        );
        assert_eq!(
            actor.producer_process_exit_due, successor_exit,
            "the stale predecessor cannot replace the retained successor exit"
        );

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(RollingControlEnvelope {
                sequence: 1,
                published_at: exit_at,
                preceding_producer: Vec::new(),
                sealed_flow_barriers: 0,
                command: RollingControlCommand::DecisionApplied {
                    decision_sequence: 1,
                    installed_attempt: Some(2),
                    reply,
                },
            })
            .await;
        assert_eq!(response.await.expect("retry acknowledgement"), Ok(()));
        assert!(matches!(
            actor.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                decision_sequence: 2,
                failed_attempt: 2,
                reason: ProducerDecisionReason::ProcessExit,
                ..
            })
        ));
        assert!(
            !actor.pending_retry_precedes_successor(2),
            "a pending Fail never gains the retry-overlap exemption"
        );
        assert_eq!(
            actor.admit_producer_retry_at(exit_at, 2, "recipe-successor-exit"),
            Err(ProducerAttemptRejection::RetryUnavailable)
        );
    }

    #[test]
    fn terminal_and_executor_loss_fence_prepublication_decisions() {
        let started = Instant::now();
        let mut terminal = registered_prepublication_actor(started);
        assert_eq!(
            terminal.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-terminal", "recipe-terminal"),
            ),
            Ok(1)
        );
        assert_eq!(
            terminal.terminate(RollingTerminalCause::End),
            RollingTerminalOutcome::Won(RollingTerminalCause::End)
        );
        assert!(terminal
            .settle_due_deadlines_at(started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET)
            .is_none());
        assert_eq!(
            terminal.poll_producer_decision_at(0),
            ProducerDecisionPoll::Terminal(RollingTerminalCause::End)
        );

        let mut lost = registered_prepublication_actor(started);
        assert_eq!(
            lost.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-lost", "recipe-lost"),
            ),
            Ok(1)
        );
        lost.mark_executor_lost(started + Duration::from_secs(1));
        assert!(matches!(
            lost.pending_decision.as_deref(),
            Some(ProducerDecision::Fail {
                decision_sequence: 1,
                failed_attempt: 1,
                reason: ProducerDecisionReason::ExecutorLost,
                ..
            })
        ));
        let status = lost.producer_operational_snapshot_at(started + Duration::from_secs(1));
        assert_eq!(status.executor_state, "lost");
        assert_eq!(
            lost.begin_initial_producer_attempt_at(
                started + Duration::from_secs(1),
                InitialProducerPolicy::software(
                    "presentation-lost".to_owned(),
                    PRODUCER_PROGRESS_BUDGET,
                ),
            ),
            Err(ProducerAttemptRejection::ExecutorLost)
        );

        let mut lost_with_retry = registered_prepublication_actor(started);
        assert_eq!(
            lost_with_retry.begin_initial_producer_attempt_at(
                started,
                hardware_policy("presentation-lost-retry", "recipe-lost-retry"),
            ),
            Ok(1)
        );
        let deadline = started + PREPUBLICATION_HARDWARE_STARTUP_BUDGET;
        assert!(lost_with_retry.settle_due_deadlines_at(deadline).is_some());
        let retained_retry = lost_with_retry
            .pending_decision
            .clone()
            .expect("hardware deadline retains one retry decision");
        lost_with_retry.mark_executor_lost(deadline + Duration::from_nanos(1));
        assert_eq!(
            lost_with_retry.pending_decision.as_ref(),
            Some(&retained_retry),
            "executor loss cannot replace the immutable pending retry"
        );
        assert!(lost_with_retry.producer_progress_deadline.is_none());
        assert!(lost_with_retry.producer_deadline_due.is_none());
        assert!(lost_with_retry.producer_process_exit_due.is_none());
        assert!(matches!(
            lost_with_retry
                .prepublication
                .as_ref()
                .expect("pre-publication state")
                .retry_state,
            PrepublicationRetryState::Consumed
        ));
        assert_eq!(
            lost_with_retry.admit_producer_retry_at(
                deadline + Duration::from_nanos(1),
                retained_retry.decision_sequence(),
                "recipe-lost-retry",
            ),
            Err(ProducerAttemptRejection::ExecutorLost)
        );
        assert_eq!(
            lost_with_retry.authorize_response_publication_at(
                deadline + Duration::from_nanos(1),
                RollingResponsePublication::generation_metadata(
                    RollingResponseObject::MasterPlaylist,
                    "presentation-lost-retry".to_owned(),
                ),
            ),
            Err(ResponsePublicationRejection::DecisionCommitted)
        );
        assert!(!lost_with_retry.commit_generation_metadata_at(
            deadline + Duration::from_nanos(1),
            "presentation-lost-retry",
            "generation-metadata-eof",
        ));
        let lost_retry_status =
            lost_with_retry.producer_operational_snapshot_at(deadline + Duration::from_nanos(1));
        assert_eq!(lost_retry_status.phase, "failed");
        assert_eq!(lost_retry_status.action_owner, "terminal_failure");
        assert_eq!(lost_retry_status.executor_state, "lost");
    }

    #[test]
    fn producer_decision_slot_is_immutable_redeliverable_and_bounded() {
        let now = Instant::now();
        let mut actor =
            RollingControlActor::new(now, "session-start", Arc::new(AtomicBool::new(false)));
        let decision = ProducerDecision::Retry {
            decision_sequence: 7,
            failed_attempt: 1,
            recipe: ValidatedRetryRecipe::for_test("cpu-safe", "fingerprint-a"),
            reason: ProducerDecisionReason::StartupDeadline,
        };
        assert!(actor.install_producer_decision_for_test(now, decision.clone()));
        assert!(!actor.install_producer_decision_for_test(
            now,
            ProducerDecision::Fail {
                decision_sequence: 8,
                failed_attempt: 1,
                reason: ProducerDecisionReason::ProcessExit,
                proposal: None,
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: CleanupPolicy::DiscardPrepublication,
                },
            }
        ));
        assert_eq!(
            actor.poll_producer_decision_at(0),
            ProducerDecisionPoll::Decision(Arc::new(decision.clone()))
        );
        assert_eq!(
            actor.poll_producer_decision_at(6),
            ProducerDecisionPoll::Decision(Arc::new(decision))
        );
        assert_eq!(
            actor.poll_producer_decision_at(7),
            ProducerDecisionPoll::Idle
        );
    }

    async fn wait_for_executor_observation(
        handle: &RollingControlHandle,
        expected_state: &str,
        expected_sequence: Option<u64>,
    ) {
        for _ in 0..1_024 {
            let (state, sequence) = handle.executor_observation_for_test();
            if state == expected_state
                && expected_sequence.is_none_or(|expected| sequence == expected)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        let (state, sequence) = handle.executor_observation_for_test();
        panic!(
            "executor never reached {expected_state:?}/{expected_sequence:?}; last={state:?}/{sequence}"
        );
    }

    #[test]
    fn terminal_transition_settles_the_retained_producer_decision() {
        let now = Instant::now();
        let retired = Arc::new(AtomicBool::new(false));
        let mut actor = RollingControlActor::new(now, "session-start", retired);
        assert!(actor.install_producer_decision_for_test(
            now,
            ProducerDecision::Fail {
                decision_sequence: 3,
                failed_attempt: 2,
                reason: ProducerDecisionReason::ProgressDeadline,
                proposal: None,
                cleanup: ProducerFailureCleanup {
                    kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                    cleanup_policy: CleanupPolicy::RetainPublished,
                },
            }
        ));
        assert_eq!(
            actor.terminate(RollingTerminalCause::End),
            RollingTerminalOutcome::Won(RollingTerminalCause::End)
        );
        assert_eq!(
            actor.poll_producer_decision_at(0),
            ProducerDecisionPoll::Terminal(RollingTerminalCause::End)
        );
        assert!(actor.pending_decision.is_none());
        assert!(actor.decision_committed_at.is_none());
    }

    #[tokio::test]
    async fn decision_poll_replays_without_a_fake_application_acknowledgement() {
        let handle = RollingControlHandle::spawn("session-start");
        let decision = ProducerDecision::Fail {
            decision_sequence: 4,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ReaderFailed,
            proposal: None,
            cleanup: ProducerFailureCleanup {
                kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                cleanup_policy: CleanupPolicy::DiscardPrepublication,
            },
        };
        assert!(
            handle
                .install_producer_decision_for_test(decision.clone())
                .await
        );
        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Decision(Arc::new(decision.clone())))
        );
        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
        );
        assert_eq!(
            handle.poll_producer_decision(4).await,
            Ok(ProducerDecisionPoll::Idle)
        );
        wait_for_executor_observation(&handle, "idle", Some(4)).await;
    }

    #[tokio::test]
    async fn retained_notify_and_full_inbox_wakes_are_bounded() {
        let notify = tokio::sync::Notify::new();
        notify.notify_one();
        notify.notified().await;

        let (sender, mut inbox) = RollingSessionExecutorInbox::new();
        assert!(sender.try_send(()).is_ok());
        assert!(matches!(
            sender.try_send(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(()))
        ));
        assert_eq!(
            inbox.receiver.recv().await,
            Some(()),
            "one coalesced wake remains available"
        );
    }

    #[test]
    fn terminal_and_lost_executor_observations_are_first_winner_absorbing() {
        let terminal_first = RollingExecutorObservation::default();
        assert!(terminal_first.begin_observing());
        terminal_first.settle_terminal();
        terminal_first.settle_lost();
        assert_eq!(terminal_first.state(), "terminal");
        assert!(!terminal_first.begin_observing());
        assert!(!terminal_first.finish_observing());

        let lost_first = RollingExecutorObservation::default();
        assert!(lost_first.begin_observing());
        lost_first.settle_lost();
        lost_first.settle_terminal();
        assert_eq!(lost_first.state(), "lost");
        assert!(!lost_first.begin_observing());
        assert!(!lost_first.finish_observing());
    }

    #[tokio::test]
    async fn cancelled_decision_poll_does_not_consume_the_actor_slot() {
        let handle = RollingControlHandle::spawn("session-start");
        let decision = ProducerDecision::Retry {
            decision_sequence: 12,
            failed_attempt: 1,
            recipe: ValidatedRetryRecipe::for_test("cpu-safe", "fingerprint-cancel"),
            reason: ProducerDecisionReason::StartupDeadline,
        };
        assert!(
            handle
                .install_producer_decision_for_test(decision.clone())
                .await
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        handle
            .enqueue_command(RollingControlCommand::PollProducerDecision {
                after_sequence: 0,
                reply,
            })
            .await
            .expect("poll enqueue");
        drop(response);
        tokio::task::yield_now().await;

        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
        );
    }

    #[tokio::test]
    async fn decision_transport_is_registered_before_begin_attempt_and_terminal_wake_is_retained() {
        let handle = RollingControlHandle::spawn("session-start");
        assert_eq!(handle.begin_producer_attempt().await, Ok(1));
        assert!(
            handle
                .install_producer_decision_for_test(ProducerDecision::Fail {
                    decision_sequence: 9,
                    failed_attempt: 1,
                    reason: ProducerDecisionReason::ExecutorLost,
                    proposal: None,
                    cleanup: ProducerFailureCleanup {
                        kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                        cleanup_policy: CleanupPolicy::DiscardPrepublication,
                    },
                })
                .await
        );
        assert_eq!(
            handle.end().await,
            Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
        );
        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Terminal(RollingTerminalCause::End))
        );
        wait_for_executor_observation(&handle, "terminal", None).await;
        handle.abort_actor_for_test();
        handle.wait_for_actor_exit_fence_for_test().await;
        handle.sender.closed().await;
        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Terminal(RollingTerminalCause::End)),
            "mailbox loss cannot relabel the actor's committed terminal cause"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timer_only_lease_expiry_wakes_the_passive_executor() {
        let handle = RollingControlHandle::spawn("session-start");

        handle.wait_for_actor_start_for_test().await;
        tokio::time::advance(ROLLING_LEGACY_LEASE_TIMEOUT + Duration::from_millis(1)).await;
        wait_for_executor_observation(&handle, "terminal", None).await;

        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Terminal(
                RollingTerminalCause::LeaseExpired
            ))
        );
    }

    #[tokio::test]
    async fn unexpected_executor_exit_is_visible_while_the_actor_remains_live() {
        let handle = RollingControlHandle::spawn("session-start");

        handle.abort_executor_for_test();
        wait_for_executor_observation(&handle, "lost", None).await;

        let decision = ProducerDecision::Fail {
            decision_sequence: 15,
            failed_attempt: 1,
            reason: ProducerDecisionReason::ExecutorLost,
            proposal: None,
            cleanup: ProducerFailureCleanup {
                kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                cleanup_policy: CleanupPolicy::DiscardPrepublication,
            },
        };
        assert!(
            handle
                .install_producer_decision_for_test(decision.clone())
                .await
        );
        assert_eq!(
            handle.poll_producer_decision(0).await,
            Ok(ProducerDecisionPoll::Decision(Arc::new(decision)))
        );
        assert_eq!(handle.executor_observation_for_test().0, "lost");
        assert_eq!(
            handle.end().await,
            Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
        );
        assert_eq!(
            handle.executor_observation_for_test().0,
            "lost",
            "committed terminal state must preserve earlier executor-loss evidence"
        );
    }

    #[tokio::test]
    async fn unexpected_actor_exit_is_not_reported_as_a_committed_terminal_cause() {
        let handle = RollingControlHandle::spawn("session-start");

        handle.wait_for_actor_start_for_test().await;
        handle.abort_actor_for_test();
        handle.wait_for_actor_exit_fence_for_test().await;
        handle.sender.closed().await;
        wait_for_executor_observation(&handle, "lost", None).await;

        assert_eq!(handle.decision_transport.committed_terminal(), None);
        assert_eq!(handle.poll_producer_decision(0).await, Err(()));
    }

    #[tokio::test]
    async fn stale_poll_result_cannot_overwrite_terminal_executor_state() {
        let handle = RollingControlHandle::spawn("session-start");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_executor_observation_for_test(Arc::clone(&pause));
        assert!(
            handle
                .install_producer_decision_for_test(ProducerDecision::Fail {
                    decision_sequence: 17,
                    failed_attempt: 1,
                    reason: ProducerDecisionReason::ReaderFailed,
                    proposal: None,
                    cleanup: ProducerFailureCleanup {
                        kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                        cleanup_policy: CleanupPolicy::DiscardPrepublication,
                    },
                })
                .await
        );
        pause.wait().await;

        assert_eq!(
            handle.end().await,
            Ok(RollingTerminalOutcome::Won(RollingTerminalCause::End))
        );
        assert_eq!(handle.executor_observation_for_test().0, "terminal");
        pause.wait().await;
        wait_for_executor_observation(&handle, "terminal", Some(17)).await;
    }

    #[tokio::test]
    async fn actor_loss_during_executor_poll_is_reported_as_lost_not_terminal() {
        let handle = RollingControlHandle::spawn("session-start");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        handle.pause_executor_poll_for_test(Arc::clone(&pause));
        assert!(
            handle
                .install_producer_decision_for_test(ProducerDecision::Fail {
                    decision_sequence: 16,
                    failed_attempt: 1,
                    reason: ProducerDecisionReason::ExecutorLost,
                    proposal: None,
                    cleanup: ProducerFailureCleanup {
                        kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                        cleanup_policy: CleanupPolicy::DiscardPrepublication,
                    },
                })
                .await
        );
        pause.wait().await;

        handle.abort_actor_for_test();
        handle.wait_for_actor_exit_fence_for_test().await;
        handle.sender.closed().await;
        pause.wait().await;
        wait_for_executor_observation(&handle, "lost", None).await;

        assert_eq!(handle.decision_transport.committed_terminal(), None);
    }

    #[tokio::test]
    async fn dropping_the_last_handle_does_not_leave_the_actor_mailbox_self_owned() {
        let handle = RollingControlHandle::spawn("session-start");
        let retired = Arc::clone(&handle.retired);
        let transport = Arc::downgrade(&handle.decision_transport);

        drop(handle);

        assert!(
            transport.upgrade().is_none(),
            "the passive executor must not keep its command transport alive"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while !retired.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor must exit promptly after its final external sender is dropped");
    }
}
