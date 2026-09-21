//! Pure Live TV delivery planning.
//!
//! The tuner observer supplies source facts, the active player supplies a
//! transport envelope, and this module freezes one plan. FFmpeg construction
//! consumes the answer; it does not make another codec, size, or packaging
//! decision later.

use plurx_core::domain::ScanType;
use plurx_core::playback::caps::{DeviceCaps, Transfer};
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 32;
const MAX_TOKEN_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveRational {
    pub(crate) num: u32,
    pub(crate) den: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveHlsFormat {
    pub(crate) container: String,
    pub(crate) video: String,
    pub(crate) audio: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveVideoLimit {
    pub(crate) codec: String,
    #[serde(default)]
    pub(crate) profile: Option<String>,
    pub(crate) max_width: u16,
    pub(crate) max_height: u16,
    pub(crate) max_frame_rate: LiveRational,
    #[serde(default)]
    pub(crate) interlaced: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveAudioLimit {
    pub(crate) codec: String,
    pub(crate) max_channels: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveCompatibilityHint {
    #[serde(default)]
    pub(crate) failed_video: bool,
    #[serde(default)]
    pub(crate) failed_audio: bool,
    #[serde(default)]
    pub(crate) failed_container: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LivePlaybackRequest {
    pub(crate) v: u8,
    /// The existing playback capability document, retained as JSON so request
    /// identity remains exactly comparable for idempotent owner starts.
    pub(crate) caps: serde_json::Value,
    #[serde(default)]
    pub(crate) hls_formats: Vec<LiveHlsFormat>,
    #[serde(default)]
    pub(crate) video_limits: Vec<LiveVideoLimit>,
    #[serde(default)]
    pub(crate) audio_limits: Vec<LiveAudioLimit>,
    #[serde(default)]
    pub(crate) max_height: Option<u16>,
    #[serde(default)]
    pub(crate) max_bitrate_bps: Option<u64>,
    #[serde(default)]
    pub(crate) compatibility: Option<LiveCompatibilityHint>,
}

impl LivePlaybackRequest {
    pub(crate) fn validate(&self) -> Result<DeviceCaps, String> {
        if self.v != 1 {
            return Err("live playback request version must be 1".into());
        }
        if self.hls_formats.len() > MAX_ENTRIES
            || self.video_limits.len() > MAX_ENTRIES
            || self.audio_limits.len() > MAX_ENTRIES
        {
            return Err("live playback capability arrays may contain at most 32 entries".into());
        }
        if self.max_height == Some(0) || self.max_bitrate_bps == Some(0) {
            return Err("live playback ceilings must be positive when present".into());
        }
        for format in &self.hls_formats {
            validate_token(&format.container)?;
            validate_token(&format.video)?;
            validate_token(&format.audio)?;
        }
        for limit in &self.video_limits {
            validate_token(&limit.codec)?;
            if let Some(profile) = &limit.profile {
                validate_token(profile)?;
            }
            if limit.max_width == 0
                || limit.max_height == 0
                || limit.max_frame_rate.num == 0
                || limit.max_frame_rate.den == 0
            {
                return Err(
                    "live video limits must contain positive dimensions and frame rate".into(),
                );
            }
        }
        for limit in &self.audio_limits {
            validate_token(&limit.codec)?;
            if limit.max_channels == 0 || limit.max_channels > 32 {
                return Err("live audio channel limits must be between 1 and 32".into());
            }
        }
        if self
            .compatibility
            .as_ref()
            .is_some_and(|hint| !hint.failed_video && !hint.failed_audio && !hint.failed_container)
        {
            return Err("a live compatibility hint must identify a failed route component".into());
        }
        let caps: DeviceCaps = serde_json::from_value(self.caps.clone())
            .map_err(|_| "live playback caps are malformed".to_owned())?;
        if caps.v != DeviceCaps::VERSION {
            return Err("live playback caps version must be 2".into());
        }
        if caps.video.len() > MAX_ENTRIES
            || caps.audio.len() > MAX_ENTRIES
            || caps.containers.len() > MAX_ENTRIES
            || caps.transports.len() > MAX_ENTRIES
        {
            return Err("playback caps arrays may contain at most 32 entries".into());
        }
        for token in caps
            .video
            .iter()
            .flat_map(|entry| std::iter::once(&entry.codec).chain(entry.profiles.iter()))
            .chain(caps.audio.iter())
            .chain(caps.containers.iter())
            .chain(caps.transports.iter())
        {
            validate_token(token)?;
        }
        Ok(caps)
    }
}

fn validate_token(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TOKEN_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(
            "live playback codec, profile, and transport names must be 1-32 ASCII token bytes"
                .into(),
        );
    }
    Ok(())
}

fn normalized(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveSourceFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_level: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pixel_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) field_order: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_rate: Option<LiveRational>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sample_aspect_ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_primaries: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_transfer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hdr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_layout: Option<String>,
}

impl LiveSourceFacts {
    fn interlaced(&self) -> bool {
        matches!(
            ScanType::from_field_order(self.field_order.as_deref()),
            ScanType::Interlaced(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveTrackAction {
    Copy,
    Encode,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LivePackaging {
    Mpegts,
    Fmp4,
}

impl LivePackaging {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mpegts => "mpegts",
            Self::Fmp4 => "fmp4",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveDeliveryReason {
    pub(crate) code: String,
    pub(crate) explanation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveDeliveryOutput {
    pub(crate) container: String,
    pub(crate) video_codec: String,
    pub(crate) audio_codec: String,
    pub(crate) width: u16,
    pub(crate) height: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_rate: Option<LiveRational>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hdr: Option<String>,
    pub(crate) audio_channels: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveDeliveryPlan {
    pub(crate) source: LiveSourceFacts,
    pub(crate) output: LiveDeliveryOutput,
    pub(crate) video_action: LiveTrackAction,
    pub(crate) audio_action: LiveTrackAction,
    pub(crate) packaging: LivePackaging,
    pub(crate) reasons: Vec<LiveDeliveryReason>,
    pub(crate) deinterlace: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deinterlace_output: Option<LiveDeinterlaceOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_bitrate_bps: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LiveQualityPolicy {
    pub(crate) max_height: Option<u16>,
    pub(crate) max_bitrate_bps: Option<u64>,
    pub(crate) deinterlace_output: LiveDeinterlaceOutput,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveDeinterlaceOutput {
    #[default]
    Field,
    Frame,
}

impl LiveDeinterlaceOutput {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Frame => "frame",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "field" => Some(Self::Field),
            "frame" => Some(Self::Frame),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LiveExecutionSupport {
    pub(crate) video_encode: bool,
    pub(crate) audio_encode: bool,
    pub(crate) tone_map: bool,
}

fn reason(code: &str, explanation: &str) -> LiveDeliveryReason {
    LiveDeliveryReason {
        code: code.to_owned(),
        explanation: explanation.to_owned(),
    }
}

fn rate_within(source: Option<LiveRational>, limit: LiveRational) -> bool {
    source.is_none_or(|source| {
        u64::from(source.num) * u64::from(limit.den) <= u64::from(limit.num) * u64::from(source.den)
    })
}

fn doubled_rate(rate: LiveRational) -> Option<LiveRational> {
    let numerator = u64::from(rate.num).checked_mul(2)?;
    let denominator = u64::from(rate.den);
    let divisor = gcd(numerator, denominator);
    Some(LiveRational {
        num: u32::try_from(numerator / divisor).ok()?,
        den: u32::try_from(denominator / divisor).ok()?,
    })
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.max(1)
}

fn video_limit_supports(source: &LiveSourceFacts, limit: &LiveVideoLimit) -> bool {
    let Some(codec) = source.video_codec.as_deref() else {
        return false;
    };
    let Some(width) = source.width else {
        return false;
    };
    let Some(height) = source.height else {
        return false;
    };
    normalized(&limit.codec) == normalized(codec)
        && limit.profile.as_deref().is_none_or(|claimed| {
            source
                .video_profile
                .as_deref()
                .is_some_and(|profile| normalized(claimed) == normalized(profile))
        })
        && width <= limit.max_width
        && height <= limit.max_height
        && rate_within(source.frame_rate, limit.max_frame_rate)
        && (!source.interlaced() || limit.interlaced)
}

fn device_video_supports(source: &LiveSourceFacts, caps: &DeviceCaps) -> bool {
    let Some(codec) = source.video_codec.as_deref() else {
        return false;
    };
    let Some(profile) = source.video_profile.as_deref() else {
        return false;
    };
    let Some(height) = source.height else {
        return false;
    };
    caps.video.iter().any(|entry| {
        normalized(&entry.codec) == normalized(codec)
            && (entry.profiles.is_empty()
                || entry
                    .profiles
                    .iter()
                    .any(|claimed| normalized(claimed) == normalized(profile)))
            && entry
                .max_height
                .is_none_or(|maximum| i64::from(height) <= maximum)
            && match source.hdr.as_deref() {
                None | Some("sdr") => true,
                Some("hlg") => entry.present.contains(&Transfer::Hlg),
                Some(_) => entry.present.contains(&Transfer::Pq),
            }
    })
}

fn audio_limit_supports(source: &LiveSourceFacts, limit: &LiveAudioLimit) -> bool {
    source.audio_codec.as_deref().is_some_and(|codec| {
        normalized(&limit.codec) == normalized(codec)
            && source.audio_sample_rate.is_some_and(|rate| rate > 0)
            && source
                .audio_channels
                .is_some_and(|channels| channels > 0 && channels <= limit.max_channels)
    })
}

fn format_supports(
    format: &LiveHlsFormat,
    packaging: LivePackaging,
    video: &str,
    audio: &str,
) -> bool {
    normalized(&format.container) == packaging.as_str()
        && normalized(&format.video) == normalized(video)
        && normalized(&format.audio) == normalized(audio)
}

fn claimed_packaging(
    request: &LivePlaybackRequest,
    preferred: LivePackaging,
    video: &str,
    audio: &str,
) -> Option<LivePackaging> {
    let alternate = match preferred {
        LivePackaging::Mpegts => LivePackaging::Fmp4,
        LivePackaging::Fmp4 => LivePackaging::Mpegts,
    };
    [preferred, alternate].into_iter().find(|packaging| {
        request
            .hls_formats
            .iter()
            .any(|format| format_supports(format, *packaging, video, audio))
    })
}

pub(crate) fn resolve_live_delivery(
    source: &LiveSourceFacts,
    request: Option<&LivePlaybackRequest>,
    policy: &LiveQualityPolicy,
    available: &LiveExecutionSupport,
) -> Result<LiveDeliveryPlan, String> {
    let width = source
        .width
        .ok_or_else(|| "source_probe_incomplete: source width is unknown".to_owned())?;
    let height = source
        .height
        .ok_or_else(|| "source_probe_incomplete: source height is unknown".to_owned())?;
    let source_video = source
        .video_codec
        .as_deref()
        .ok_or_else(|| "source_probe_incomplete: source video codec is unknown".to_owned())?;
    let source_audio = source
        .audio_codec
        .as_deref()
        .ok_or_else(|| "source_probe_incomplete: source audio codec is unknown".to_owned())?;
    // A probe can identify AC-4 before the first random-access frame supplies
    // its layout. Zero means unknown, not a request for `-ac 0`.
    let source_channels = source
        .audio_channels
        .filter(|channels| *channels > 0)
        .unwrap_or(2);

    let caps = request.map(LivePlaybackRequest::validate).transpose()?;
    let compatibility = request.and_then(|request| request.compatibility.as_ref());
    let explicit_height = [
        policy.max_height,
        request.and_then(|request| request.max_height),
    ]
    .into_iter()
    .flatten()
    .min();
    let max_bitrate_bps = [
        policy.max_bitrate_bps,
        request.and_then(|request| request.max_bitrate_bps),
    ]
    .into_iter()
    .flatten()
    .min();

    let mut reasons = Vec::new();
    let copy_height_allowed = explicit_height.is_none_or(|maximum| height <= maximum);
    if !copy_height_allowed {
        reasons.push(reason(
            "explicit_resolution_ceiling",
            "The requested or saved maximum resolution is below the source.",
        ));
    }

    let copy_packaging = if normalized(source_video) == "hevc" {
        LivePackaging::Fmp4
    } else {
        LivePackaging::Mpegts
    };
    let video_claimed = request.is_some_and(|request| {
        caps.as_ref()
            .is_some_and(|caps| device_video_supports(source, caps))
            && request
                .video_limits
                .iter()
                .any(|limit| video_limit_supports(source, limit))
    });
    let audio_claimed = request.is_some_and(|request| {
        caps.as_ref().is_some_and(|caps| {
            caps.audio
                .iter()
                .any(|codec| normalized(codec) == normalized(source_audio))
        }) && request
            .audio_limits
            .iter()
            .any(|limit| audio_limit_supports(source, limit))
    });
    let complete_copy_claim = request.is_some_and(|request| {
        request
            .hls_formats
            .iter()
            .any(|format| format_supports(format, copy_packaging, source_video, source_audio))
    });

    let video_copy = video_claimed
        && copy_height_allowed
        && !compatibility.is_some_and(|hint| hint.failed_video || hint.failed_container);
    let audio_copy = audio_claimed
        && !compatibility.is_some_and(|hint| hint.failed_audio || hint.failed_container);

    let mut video_action = if video_copy {
        LiveTrackAction::Copy
    } else {
        LiveTrackAction::Encode
    };
    let mut audio_action = if audio_copy {
        LiveTrackAction::Copy
    } else {
        LiveTrackAction::Encode
    };

    if request.is_none() {
        reasons.push(reason(
            "client_capability_unknown",
            "This client did not describe its live HLS playback path, so the server selected a conservative route.",
        ));
    } else {
        if video_action == LiveTrackAction::Encode && copy_height_allowed {
            reasons.push(reason(
                "video_incompatible",
                "The active player did not claim the complete source video route.",
            ));
        }
        if audio_action == LiveTrackAction::Encode {
            reasons.push(reason(
                "audio_incompatible",
                "The active output route did not claim the source audio codec and channel layout.",
            ));
        }
        if compatibility.is_some() {
            reasons.push(reason(
                "compatibility_fallback",
                "The player reported that the previous delivery route failed.",
            ));
        }
    }

    if video_action == LiveTrackAction::Copy
        && audio_action == LiveTrackAction::Copy
        && !complete_copy_claim
    {
        audio_action = LiveTrackAction::Encode;
        reasons.push(reason(
            "container_incompatible",
            "The player did not claim the complete source video/audio packaging combination.",
        ));
    }

    if source.interlaced()
        && !request.is_some_and(|request| {
            request
                .video_limits
                .iter()
                .any(|limit| video_limit_supports(source, limit) && limit.interlaced)
        })
    {
        video_action = LiveTrackAction::Encode;
        reasons.push(reason(
            "unsupported_interlacing",
            "The player did not claim a usable deinterlacing path for this source.",
        ));
    }

    if source.hdr.as_deref().is_some_and(|value| value != "sdr")
        && video_action == LiveTrackAction::Encode
        && !available.tone_map
    {
        return Err(
            "hdr_conversion_unsupported: no verified tone-mapping route is available".into(),
        );
    }
    if video_action == LiveTrackAction::Encode && !available.video_encode {
        return Err("video_conversion_unavailable: this route requires a video encoder".into());
    }
    if audio_action == LiveTrackAction::Encode && !available.audio_encode {
        return Err("audio_conversion_unavailable: this route requires an audio encoder".into());
    }

    let video_codec = if video_action == LiveTrackAction::Copy {
        normalized(source_video)
    } else {
        "h264".to_owned()
    };
    let mut audio_codec = if audio_action == LiveTrackAction::Copy {
        normalized(source_audio)
    } else {
        "aac".to_owned()
    };
    let preferred_packaging = if video_action == LiveTrackAction::Copy && video_codec == "hevc" {
        LivePackaging::Fmp4
    } else {
        LivePackaging::Mpegts
    };
    let mut packaging = if let Some(request) = request {
        if let Some(packaging) =
            claimed_packaging(request, preferred_packaging, &video_codec, &audio_codec)
        {
            packaging
        } else if audio_action == LiveTrackAction::Copy {
            if !available.audio_encode {
                return Err(
                    "audio_conversion_unavailable: the final client route requires AAC audio"
                        .into(),
                );
            }
            let Some(packaging) =
                claimed_packaging(request, preferred_packaging, &video_codec, "aac")
            else {
                return Err(
                    "client_route_unsupported: the player did not claim the resolved HLS codec and packaging combination"
                        .into(),
                );
            };
            audio_action = LiveTrackAction::Encode;
            audio_codec = "aac".to_owned();
            reasons.push(reason(
                "container_incompatible",
                "The source audio cannot be carried in a complete HLS route claimed by this player, so only audio is converted.",
            ));
            packaging
        } else {
            return Err(
                "client_route_unsupported: the player did not claim the resolved HLS codec and packaging combination"
                    .into(),
            );
        }
    } else {
        preferred_packaging
    };

    // With short live segments, delayed AC-3 packets can arrive after hlsenc
    // writes the fMP4 init file. FFmpeg can exit successfully yet leave an
    // unreadable stsd entry ("Cannot write moov atom before AC3 packets").
    // A larger input probe does not fix that output-side race. Preserve video
    // and encode audio, provided the player actually claims the AAC route.
    if packaging == LivePackaging::Fmp4
        && audio_action == LiveTrackAction::Copy
        && audio_codec == "ac3"
    {
        if !available.audio_encode {
            return Err(
                "audio_conversion_unavailable: live AC-3 in fMP4 requires AAC conversion".into(),
            );
        }
        packaging = request
            .and_then(|request| {
                claimed_packaging(request, preferred_packaging, &video_codec, "aac")
            })
            .ok_or_else(|| {
                "client_route_unsupported: live AC-3 in fMP4 requires a claimed AAC route"
                    .to_owned()
            })?;
        audio_action = LiveTrackAction::Encode;
        audio_codec = "aac".to_owned();
        reasons.push(reason(
            "audio_muxer_incompatible",
            "AC-3 can arrive after the live fMP4 initialization metadata is written, so audio is converted to AAC.",
        ));
    }

    let client_video_ceiling = request.and_then(|request| {
        request
            .video_limits
            .iter()
            .filter(|limit| normalized(&limit.codec) == video_codec)
            .map(|limit| limit.max_height)
            .max()
    });
    let output_height = [Some(height), explicit_height, client_video_ceiling]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(height);
    let output_width = if output_height < height {
        let scaled = u32::from(width) * u32::from(output_height) / u32::from(height);
        u16::try_from(scaled & !1).unwrap_or(width)
    } else {
        width
    };
    let audio_channels = if audio_action == LiveTrackAction::Copy {
        source_channels
    } else {
        request
            .and_then(|request| {
                request
                    .audio_limits
                    .iter()
                    .filter(|limit| normalized(&limit.codec) == "aac")
                    .map(|limit| limit.max_channels)
                    .max()
            })
            .unwrap_or(2)
            .min(source_channels)
            // Native AAC rejects immersive layouts such as AC-4's 7.1.4.
            // Use at most 5.1 while respecting a smaller source/client limit.
            .min(6)
    };

    if max_bitrate_bps.is_some() {
        reasons.push(reason(
            "explicit_bitrate_ceiling",
            "An explicit saved or requested bitrate ceiling applies to this delivery.",
        ));
    }
    if video_action == LiveTrackAction::Copy && audio_action == LiveTrackAction::Encode {
        reasons.push(reason(
            "video_preserved",
            "Only audio conversion is required; the compressed source video is copied.",
        ));
    }

    let deinterlace = source.interlaced() && video_action == LiveTrackAction::Encode;
    let output_frame_rate =
        if deinterlace && policy.deinterlace_output == LiveDeinterlaceOutput::Field {
            source.frame_rate.and_then(doubled_rate)
        } else {
            source.frame_rate
        };

    Ok(LiveDeliveryPlan {
        source: source.clone(),
        output: LiveDeliveryOutput {
            container: packaging.as_str().to_owned(),
            video_codec,
            audio_codec,
            width: output_width,
            height: output_height,
            bit_depth: (video_action == LiveTrackAction::Copy)
                .then_some(source.bit_depth)
                .flatten(),
            frame_rate: output_frame_rate,
            hdr: (video_action == LiveTrackAction::Copy)
                .then_some(source.hdr.clone())
                .flatten(),
            audio_channels,
        },
        video_action,
        audio_action,
        packaging,
        reasons,
        deinterlace,
        deinterlace_output: deinterlace.then_some(policy.deinterlace_output),
        max_bitrate_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> LiveSourceFacts {
        LiveSourceFacts {
            video_codec: Some("hevc".into()),
            video_profile: Some("main10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            field_order: Some("progressive".into()),
            frame_rate: Some(LiveRational {
                num: 60000,
                den: 1001,
            }),
            hdr: Some("pq".into()),
            audio_codec: Some("ac3".into()),
            audio_sample_rate: Some(48000),
            audio_channels: Some(6),
            ..LiveSourceFacts::default()
        }
    }

    fn request(audio: &str) -> LivePlaybackRequest {
        LivePlaybackRequest {
            v: 1,
            caps: serde_json::json!({
                "v": 2,
                "video": [{"codec":"hevc","profiles":["main10"],"max_height":2160,"present":["pq"]}],
                "audio": [audio],
                "containers": ["fmp4"],
                "transports": ["hls"]
            }),
            hls_formats: vec![LiveHlsFormat {
                container: "fmp4".into(),
                video: "hevc".into(),
                audio: audio.into(),
            }],
            video_limits: vec![LiveVideoLimit {
                codec: "hevc".into(),
                profile: Some("main10".into()),
                max_width: 3840,
                max_height: 2160,
                max_frame_rate: LiveRational {
                    num: 60000,
                    den: 1001,
                },
                interlaced: false,
            }],
            audio_limits: vec![LiveAudioLimit {
                codec: audio.into(),
                max_channels: 6,
            }],
            max_height: None,
            max_bitrate_bps: None,
            compatibility: None,
        }
    }

    #[test]
    fn compatible_main10_copies_both_tracks_in_fmp4() {
        let mut source = source();
        source.audio_codec = Some("aac".into());
        let plan = resolve_live_delivery(
            &source,
            Some(&request("aac")),
            &LiveQualityPolicy::default(),
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("copy route");
        assert_eq!(plan.video_action, LiveTrackAction::Copy);
        assert_eq!(plan.audio_action, LiveTrackAction::Copy);
        assert_eq!(plan.packaging, LivePackaging::Fmp4);
        assert_eq!(plan.output.height, 2160);
    }

    #[test]
    fn source_scan_type_is_conservative_for_unknown_and_future_tokens() {
        let mut source = source();
        for field_order in [
            None,
            Some("unknown"),
            Some("future-order"),
            Some("progressive"),
        ] {
            source.field_order = field_order.map(str::to_owned);
            assert!(
                !source.interlaced(),
                "unexpected interlace for {field_order:?}"
            );
        }
        for field_order in ["tt", "bb", "tb", "bt"] {
            source.field_order = Some(field_order.to_owned());
            assert!(source.interlaced(), "missing interlace for {field_order}");
        }
    }

    #[test]
    fn audio_mismatch_preserves_video() {
        let plan = resolve_live_delivery(
            &source(),
            Some(&request("aac")),
            &LiveQualityPolicy::default(),
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("audio-only conversion");
        assert_eq!(plan.video_action, LiveTrackAction::Copy);
        assert_eq!(plan.audio_action, LiveTrackAction::Encode);
    }

    #[test]
    fn encoded_video_never_invents_an_unclaimed_audio_tuple() {
        let mut playback = request("ac3");
        playback.caps = serde_json::json!({
            "v": 2,
            "video": [{"codec":"h264","profiles":[],"max_height":2160,"present":[]}],
            "audio": ["aac", "ac3"],
            "containers": ["mpegts"],
            "transports": ["hls"]
        });
        playback.hls_formats = vec![LiveHlsFormat {
            container: "mpegts".into(),
            video: "h264".into(),
            audio: "aac".into(),
        }];
        playback.video_limits = vec![LiveVideoLimit {
            codec: "h264".into(),
            profile: None,
            max_width: 3840,
            max_height: 2160,
            max_frame_rate: LiveRational { num: 60, den: 1 },
            interlaced: false,
        }];
        playback.audio_limits.push(LiveAudioLimit {
            codec: "aac".into(),
            max_channels: 2,
        });

        let plan = resolve_live_delivery(
            &source(),
            Some(&playback),
            &LiveQualityPolicy::default(),
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: true,
            },
        )
        .expect("claimed H.264/AAC route");
        assert_eq!(plan.video_action, LiveTrackAction::Encode);
        assert_eq!(plan.audio_action, LiveTrackAction::Encode);
        assert_eq!(plan.output.audio_codec, "aac");
        assert_eq!(plan.packaging, LivePackaging::Mpegts);
    }

    #[test]
    fn ceiling_never_upscales() {
        let mut lower = source();
        lower.audio_codec = Some("aac".into());
        lower.width = Some(1280);
        lower.height = Some(720);
        let plan = resolve_live_delivery(
            &lower,
            Some(&request("aac")),
            &LiveQualityPolicy {
                max_height: Some(2160),
                max_bitrate_bps: None,
                ..LiveQualityPolicy::default()
            },
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("bounded route");
        assert_eq!(plan.output.height, 720);
    }

    #[test]
    fn interlaced_encode_reports_the_selected_output_cadence() {
        let mut input = source();
        input.width = Some(1920);
        input.height = Some(1080);
        input.field_order = Some("tt".into());
        input.hdr = None;
        input.frame_rate = Some(LiveRational {
            num: 30_000,
            den: 1_001,
        });
        let field = resolve_live_delivery(
            &input,
            None,
            &LiveQualityPolicy::default(),
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("field-rate plan");
        assert!(field.deinterlace);
        assert_eq!(
            field.output.frame_rate,
            Some(LiveRational {
                num: 60_000,
                den: 1_001
            })
        );

        let frame = resolve_live_delivery(
            &input,
            None,
            &LiveQualityPolicy {
                deinterlace_output: LiveDeinterlaceOutput::Frame,
                ..LiveQualityPolicy::default()
            },
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("frame-rate plan");
        assert_eq!(
            frame.output.frame_rate,
            Some(LiveRational {
                num: 30_000,
                den: 1_001
            })
        );
    }
}
