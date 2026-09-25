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
    /// Every audio stream the probe observed, in demuxer order. The flat
    /// `audio_*` fields above describe the track the plan selected (the first
    /// one until a plan is resolved). Empty when the probe predates this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) audio_tracks: Vec<LiveSourceAudioTrack>,
}

/// One audio stream of the tuner programme. `index` is its ordinal among the
/// audio streams, which is what an FFmpeg `0:a:<index>` map addresses.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LiveSourceAudioTrack {
    pub(crate) index: u8,
    /// The container's own stream id (the PID in MPEG-TS), which the live
    /// open maps by (`0:i:<id>`) so a stream the runtime demuxer has not yet
    /// classified cannot shift the ordinal onto another track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    /// Described video, hearing-impaired or commentary audio per the
    /// container's disposition flags: never the main programme audio.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) described: bool,
}

impl LiveSourceAudioTrack {
    /// The FFmpeg input stream specifier for this track.
    pub(crate) fn map_specifier(&self) -> String {
        match self.id {
            Some(id) => format!("0:i:{id:#x}"),
            None => format!("0:a:{}", self.index),
        }
    }
}

impl LiveSourceFacts {
    /// The audio tracks a plan may choose from: the probed list, or the flat
    /// fields as a single track when the probe carried no list.
    fn candidate_audio_tracks(&self) -> Vec<LiveSourceAudioTrack> {
        if !self.audio_tracks.is_empty() {
            return self.audio_tracks.clone();
        }
        vec![LiveSourceAudioTrack {
            index: 0,
            id: None,
            codec: self.audio_codec.clone(),
            sample_rate: self.audio_sample_rate,
            channels: self.audio_channels,
            layout: self.audio_layout.clone(),
            language: None,
            described: false,
        }]
    }

    /// The facts with the flat `audio_*` fields describing `track`.
    fn with_audio_track(&self, track: &LiveSourceAudioTrack) -> Self {
        Self {
            audio_codec: track.codec.clone(),
            audio_sample_rate: track.sample_rate,
            audio_channels: track.channels,
            audio_layout: track.layout.clone(),
            ..self.clone()
        }
    }

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
    /// Ordinal of the selected source audio stream (`0:a:<audio_track>`).
    #[serde(default)]
    pub(crate) audio_track: u8,
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

fn scaled_output_size(width: u16, height: u16, maximum_height: u16) -> (u16, u16) {
    let output_height = height.min(maximum_height);
    let output_width = if output_height < height {
        let scaled = u32::from(width) * u32::from(output_height) / u32::from(height);
        u16::try_from(scaled & !1).unwrap_or(width)
    } else {
        width
    };
    (output_width, output_height)
}

fn encoded_output_for_request(
    request: &LivePlaybackRequest,
    codec: &str,
    width: u16,
    height: u16,
    explicit_height: Option<u16>,
    output_frame_rate: Option<LiveRational>,
) -> Option<(u16, u16)> {
    request
        .video_limits
        .iter()
        .filter(|limit| normalized(&limit.codec) == codec)
        .filter_map(|limit| {
            let width_limited_height = if width > limit.max_width {
                u16::try_from(u32::from(height) * u32::from(limit.max_width) / u32::from(width))
                    .ok()?
            } else {
                height
            };
            let maximum_height = [
                explicit_height.unwrap_or(height),
                limit.max_height,
                width_limited_height,
            ]
            .into_iter()
            .min()?;
            let output = scaled_output_size(width, height, maximum_height);
            (output.0 <= limit.max_width && rate_within(output_frame_rate, limit.max_frame_rate))
                .then_some(output)
        })
        // Select one complete limit. Taking the maximum of each field across
        // different limits can invent a 1080p60 capability from a 1080p30 row
        // and a 720p60 row.
        .max_by_key(|(width, height)| u32::from(*width) * u32::from(*height))
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

fn audio_limit_supports(track: &LiveSourceAudioTrack, limit: &LiveAudioLimit) -> bool {
    track.codec.as_deref().is_some_and(|codec| {
        normalized(&limit.codec) == normalized(codec)
            && track.sample_rate.is_some_and(|rate| rate > 0)
            && track
                .channels
                .is_some_and(|channels| channels > 0 && channels <= limit.max_channels)
    })
}

/// Whether the active player claims `track` for copy on the resolved video
/// route: the codec in its capability document, a channel limit that admits
/// the track, and an HLS packaging that carries the (video, audio) pair.
fn audio_track_copy_claimed(
    track: &LiveSourceAudioTrack,
    request: &LivePlaybackRequest,
    caps: &DeviceCaps,
    preferred_packaging: LivePackaging,
    video_codec: &str,
) -> bool {
    let Some(codec) = track.codec.as_deref() else {
        return false;
    };
    if !caps
        .audio
        .iter()
        .any(|claimed| normalized(claimed) == normalized(codec))
        || !request
            .audio_limits
            .iter()
            .any(|limit| audio_limit_supports(track, limit))
    {
        return false;
    }
    match claimed_packaging(request, preferred_packaging, video_codec, codec) {
        None => false,
        // AC-3 in live fMP4 is converted (the init-file race), so it only
        // counts as a copy when MPEG-TS carries the pair — the same condition
        // the container switch applies.
        Some(LivePackaging::Fmp4) if normalized(codec) == "ac3" => {
            claimed_packaging(request, LivePackaging::Mpegts, video_codec, codec)
                == Some(LivePackaging::Mpegts)
        }
        Some(_) => true,
    }
}

/// Codecs whose live decode is fragile enough that another track of the same
/// programme is preferred as an encode source: the AC-4 decoder emits nothing
/// until a global random-access frame arrives, which a broadcast may withhold
/// for seconds.
fn fragile_decode(codec: Option<&str>) -> bool {
    codec.is_some_and(|codec| normalized(codec) == "ac4")
}

/// A language tag worth comparing: `und` and empty are the same as no tag.
fn tagged_language(track: &LiveSourceAudioTrack) -> Option<String> {
    track
        .language
        .as_deref()
        .map(normalized)
        .filter(|language| !language.is_empty() && language != "und")
}

/// Choose the audio track for this delivery. Described, hearing-impaired and
/// commentary tracks are never preferred to the main audio. Then direct play
/// first: a track the player takes untouched beats every track that needs
/// conversion. Among equals, prefer the robust decode, then the fuller
/// layout, then broadcast order. Only tracks in the programme's primary
/// language are candidates — the first track's tag, or, when the first track
/// carries none, only the untagged tracks — so the choice never changes what
/// language is heard.
fn select_audio_track(
    tracks: &[LiveSourceAudioTrack],
    request: Option<&LivePlaybackRequest>,
    caps: Option<&DeviceCaps>,
    preferred_packaging: LivePackaging,
    video_codec: &str,
    audio_copy_refused: bool,
) -> LiveSourceAudioTrack {
    let primary_language = tracks.first().and_then(tagged_language);
    let same_language =
        |track: &LiveSourceAudioTrack| match (&primary_language, tagged_language(track)) {
            (Some(primary), Some(language)) => *primary == language,
            (None, None) => true,
            _ => false,
        };
    tracks
        .iter()
        .filter(|track| same_language(track))
        .max_by_key(|track| {
            let copyable = !audio_copy_refused
                && request.zip(caps).is_some_and(|(request, caps)| {
                    audio_track_copy_claimed(track, request, caps, preferred_packaging, video_codec)
                });
            (
                !track.described,
                copyable,
                !fragile_decode(track.codec.as_deref()),
                track.channels.unwrap_or(0),
                std::cmp::Reverse(track.index),
            )
        })
        .or_else(|| tracks.first())
        .cloned()
        .unwrap_or_default()
}

/// The channel layouts an encoded track may take under `ceiling` channels, as
/// an FFmpeg `aformat=channel_layouts=` list. FFmpeg picks the listed layout
/// closest to the decoded one, so a 7.1.4 broadcast becomes 5.1 while a
/// stereo one stays stereo — and a layout the probe never learned (an AC-4
/// track before its first random-access frame) is resolved when the first
/// frame arrives instead of being forced to stereo up front.
///
/// Only layouts with an ADTS `channel_configuration` are listed. `5.1(side)`
/// — the layout every ATSC AC-3 5.1 track decodes to — has none, so the AAC
/// encoder signals it as configuration 0 plus a PCE, which hls.js and the
/// browsers' MSE read as an audio track with no channels and never start
/// (measured 2026-09-24: Safari and Chrome black with 0 decoded frames on
/// 157.1). `5.1` is configuration 6; the side pair maps onto it 1:1.
pub(crate) fn encoded_channel_layouts(ceiling: u8) -> String {
    let mut layouts = Vec::new();
    if ceiling >= 6 {
        layouts.push("5.1");
    }
    if ceiling >= 2 {
        layouts.push("stereo");
    }
    layouts.push("mono");
    layouts.join("|")
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
    let video_copy_candidate = video_claimed
        && copy_height_allowed
        && !compatibility.is_some_and(|hint| hint.failed_video || hint.failed_container)
        && (!source.interlaced()
            || request.is_some_and(|request| {
                request
                    .video_limits
                    .iter()
                    .any(|limit| video_limit_supports(source, limit) && limit.interlaced)
            }));
    // The audio track is chosen against the video route it will ride with:
    // a copied HEVC picture prefers fMP4, an encode is H.264 in MPEG-TS.
    let tracks = source.candidate_audio_tracks();
    let copied_video_codec = normalized(source_video);
    let track = select_audio_track(
        &tracks,
        request,
        caps.as_ref(),
        if video_copy_candidate && copied_video_codec == "hevc" {
            LivePackaging::Fmp4
        } else {
            LivePackaging::Mpegts
        },
        if video_copy_candidate {
            copied_video_codec.as_str()
        } else {
            "h264"
        },
        compatibility.is_some_and(|hint| hint.failed_audio || hint.failed_container),
    );
    let source = &source.with_audio_track(&track);
    let source_audio = source
        .audio_codec
        .as_deref()
        .ok_or_else(|| "source_probe_incomplete: source audio codec is unknown".to_owned())?;
    // A probe can identify AC-4 before the first random-access frame supplies
    // its layout. Zero means unknown, not a request for `-ac 0`.
    let known_channels = source.audio_channels.filter(|channels| *channels > 0);
    let source_channels = known_channels.unwrap_or(2);
    let audio_claimed = request.is_some_and(|request| {
        caps.as_ref().is_some_and(|caps| {
            caps.audio
                .iter()
                .any(|codec| normalized(codec) == normalized(source_audio))
        }) && request
            .audio_limits
            .iter()
            .any(|limit| audio_limit_supports(&track, limit))
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
        && request.is_some_and(|request| {
            claimed_packaging(request, LivePackaging::Mpegts, &video_codec, &audio_codec)
                == Some(LivePackaging::Mpegts)
        })
    {
        // MPEG-TS has no init file to race, so a player that takes this
        // video/audio pair in MPEG-TS keeps the compressed audio untouched.
        packaging = LivePackaging::Mpegts;
        reasons.push(reason(
            "container_switched",
            "AC-3 is carried in MPEG-TS instead of fMP4 so the source audio can be copied.",
        ));
    }
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

    let deinterlace = source.interlaced() && video_action == LiveTrackAction::Encode;
    // Two cadences, and they are not the same number. `plan.source.frame_rate`
    // stays exactly as the source reported it — that is what a television
    // client reads when it picks a display mode. `output.frame_rate` is what
    // this encode will actually emit, so it has to agree with the bwdif mode
    // the filter chain builds from `policy.deinterlace_output`: `send_field`
    // emits one frame per field (doubled), `send_frame` one per frame pair
    // (unchanged). Declaring the field rate for a `send_frame` chain would
    // also mis-pick the client video limit chosen from it just below.
    let output_frame_rate =
        if deinterlace && policy.deinterlace_output == LiveDeinterlaceOutput::Field {
            source.frame_rate.and_then(doubled_rate)
        } else {
            source.frame_rate
        };
    let (output_width, output_height) = if video_action == LiveTrackAction::Encode {
        if let Some(request) = request {
            encoded_output_for_request(
                request,
                &video_codec,
                width,
                height,
                explicit_height,
                output_frame_rate,
            )
            .ok_or_else(|| {
                "client_route_unsupported: no single final video limit admits the encoded dimensions and frame rate"
                    .to_owned()
            })?
        } else {
            scaled_output_size(width, height, explicit_height.unwrap_or(height))
        }
    } else {
        scaled_output_size(width, height, explicit_height.unwrap_or(height))
    };
    let audio_channels = if audio_action == LiveTrackAction::Copy {
        source_channels
    } else {
        // Native AAC rejects immersive layouts such as AC-4's 7.1.4, so the
        // ceiling is 5.1, the player's AAC limit, and the source layout when
        // the probe learned it. An unknown layout keeps the ceiling: the
        // encode negotiates the real layout from the first decoded frame
        // (`encoded_channel_layouts`), so this is an upper bound, not a
        // promise of that many channels.
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
            .min(known_channels.unwrap_or(u8::MAX))
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
        audio_track: track.index,
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

    /// D-01 hands a television the cadence to switch its panel to, so the plan
    /// has to carry both numbers at once: the source cadence it reports, and
    /// the cadence this encode will emit. They differ whenever bwdif runs in
    /// `send_field`, so neither one may be derived from the other at the
    /// reader.
    #[test]
    fn deinterlaced_output_reports_field_rate_for_display_matching() {
        let mut interlaced = source();
        interlaced.field_order = Some("tt".into());
        interlaced.frame_rate = Some(LiveRational {
            num: 30000,
            den: 1001,
        });
        let mut playback = request("ac3");
        playback.caps = serde_json::json!({
            "v": 2,
            "video": [{"codec":"h264","profiles":[],"max_height":2160,"present":[]}],
            "audio": ["aac"],
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
        playback.audio_limits = vec![LiveAudioLimit {
            codec: "aac".into(),
            max_channels: 2,
        }];
        let plan = resolve_live_delivery(
            &interlaced,
            Some(&playback),
            &LiveQualityPolicy::default(),
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: true,
            },
        )
        .expect("interlaced input is converted");

        assert!(plan.deinterlace);
        assert_eq!(
            plan.output.frame_rate,
            Some(LiveRational {
                num: 60000,
                den: 1001,
            }),
        );
        // The source cadence is reported unchanged beside it.
        assert_eq!(
            plan.source.frame_rate,
            Some(LiveRational {
                num: 30000,
                den: 1001,
            }),
        );

        // Under `send_frame` the encode emits the source cadence, and the plan
        // still reports the same source cadence beside it.
        let frame = resolve_live_delivery(
            &interlaced,
            Some(&playback),
            &LiveQualityPolicy {
                deinterlace_output: LiveDeinterlaceOutput::Frame,
                ..LiveQualityPolicy::default()
            },
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: true,
            },
        )
        .expect("interlaced input is converted");
        assert!(frame.deinterlace);
        assert_eq!(
            frame.output.frame_rate,
            Some(LiveRational {
                num: 30000,
                den: 1001,
            }),
        );
        assert_eq!(frame.source.frame_rate, frame.output.frame_rate);
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

    #[test]
    fn interlaced_encode_selects_one_h264_limit_for_dimensions_and_final_cadence() {
        let mut input = source();
        input.width = Some(1920);
        input.height = Some(1080);
        input.field_order = Some("tt".into());
        input.hdr = None;
        input.frame_rate = Some(LiveRational {
            num: 30_000,
            den: 1_001,
        });

        let mut playback = request("aac");
        playback.caps = serde_json::json!({
            "v": 2,
            "video": [{"codec":"h264","profiles":[],"max_height":2160,"present":[]}],
            "audio": ["aac"],
            "containers": ["mpegts"],
            "transports": ["hls"]
        });
        playback.hls_formats = vec![LiveHlsFormat {
            container: "mpegts".into(),
            video: "h264".into(),
            audio: "aac".into(),
        }];
        playback.video_limits = vec![
            LiveVideoLimit {
                codec: "h264".into(),
                profile: None,
                max_width: 3840,
                max_height: 2160,
                max_frame_rate: LiveRational { num: 30, den: 1 },
                interlaced: false,
            },
            LiveVideoLimit {
                codec: "h264".into(),
                profile: None,
                max_width: 1280,
                max_height: 720,
                max_frame_rate: LiveRational { num: 60, den: 1 },
                interlaced: false,
            },
        ];
        let support = LiveExecutionSupport {
            video_encode: true,
            audio_encode: true,
            tone_map: false,
        };

        let field = resolve_live_delivery(
            &input,
            Some(&playback),
            &LiveQualityPolicy::default(),
            &support,
        )
        .expect("the 720p60 H.264 limit admits field-rate output");
        assert_eq!((field.output.width, field.output.height), (1280, 720));
        assert_eq!(
            field.output.frame_rate,
            Some(LiveRational {
                num: 60_000,
                den: 1_001
            })
        );

        let frame = resolve_live_delivery(
            &input,
            Some(&playback),
            &LiveQualityPolicy {
                deinterlace_output: LiveDeinterlaceOutput::Frame,
                ..LiveQualityPolicy::default()
            },
            &support,
        )
        .expect("the 1080p30 H.264 limit admits frame-rate output");
        assert_eq!((frame.output.width, frame.output.height), (1920, 1080));
        assert_eq!(
            frame.output.frame_rate,
            Some(LiveRational {
                num: 30_000,
                den: 1_001
            })
        );

        playback.video_limits.pop();
        let error = resolve_live_delivery(
            &input,
            Some(&playback),
            &LiveQualityPolicy::default(),
            &support,
        )
        .expect_err("1080p30 must not be treated as 1080p59.94");
        assert!(error.contains("no single final video limit"), "{error}");
    }
}
