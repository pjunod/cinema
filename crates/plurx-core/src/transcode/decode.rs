//! Pure decoder and renderer planning.
//!
//! This module deliberately does not spawn FFmpeg or read process environment.
//! Callers collect one bound [`DecodeFacts`] snapshot, one capability snapshot,
//! and one policy snapshot, then resolve exactly once. Command construction and
//! recipe identity consume the resulting [`ResolvedTranscode`] in the next
//! migration step; keeping resolution pure here makes that migration testable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{EffectiveRateControl, Encoder, OutputGrade, Pipeline, SubtitleBurn, ToneMap};

const MAX_FACT_TOKEN_BYTES: usize = 128;
const MAX_DECODER_NAME_BYTES: usize = 128;

/// Stable revision of the explicit legacy-preference extraction.
pub const LEGACY_DECODE_POLICY_REVISION: u32 = 1;

/// The input decoder family, independent of the output encoder family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeBackend {
    Software,
    VideoToolbox,
    Cuda,
    Qsv,
    Vaapi,
}

impl DecodeBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::VideoToolbox => "videotoolbox",
            Self::Cuda => "cuda",
            Self::Qsv => "qsv",
            Self::Vaapi => "vaapi",
        }
    }
}

/// Whether selection was supported by a qualified input probe or inherited
/// from the pre-plan routing policy during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeEvidence {
    Qualified,
    LegacyUnverified,
}

/// Capability inventory must not collapse advertised and exercised support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Unavailable,
    Advertised,
    Qualified,
    Rejected,
}

/// Why one complete pipeline was selected. Display text is derived elsewhere;
/// these stable values are safe for identity and bounded diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeReason {
    RendererRequirement,
    CompatibilityExclusion,
    OperatorSoftwareOverride,
    ContinuationRestriction,
    LegacyPreference,
    QualifiedPreference,
    CapabilityFallback,
}

/// Where decoded frames live before rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameDomain {
    SystemMemory,
    Qsv,
    Vaapi,
}

/// The validated handoff between the selected decoder and renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeSurfaceContract {
    frame_domain: FrameDomain,
    download_format: Option<String>,
    upload_domain: Option<FrameDomain>,
    upload_format: Option<String>,
    required_side_data: bool,
}

impl DecodeSurfaceContract {
    pub fn frame_domain(&self) -> FrameDomain {
        self.frame_domain
    }

    pub fn download_format(&self) -> Option<&str> {
        self.download_format.as_deref()
    }

    pub fn upload_domain(&self) -> Option<FrameDomain> {
        self.upload_domain
    }

    pub fn upload_format(&self) -> Option<&str> {
        self.upload_format.as_deref()
    }

    pub fn requires_side_data(&self) -> bool {
        self.required_side_data
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamSelectionProvenance {
    FirstPlayableVideo,
    ExplicitAbsoluteIndex,
}

/// A reduced, strictly positive rational.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rational {
    numerator: u32,
    denominator: u32,
}

impl Rational {
    pub fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        let divisor = gcd(numerator, denominator);
        Some(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    pub fn numerator(self) -> u32 {
        self.numerator
    }

    pub fn denominator(self) -> u32 {
        self.denominator
    }
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameRateProvenance {
    Average,
    Nominal,
    Variable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrameRate {
    value: Option<Rational>,
    provenance: FrameRateProvenance,
}

impl FrameRate {
    pub fn value(self) -> Option<Rational> {
        self.value
    }

    pub fn provenance(self) -> FrameRateProvenance {
        self.provenance
    }
}

/// Identity of the already-bound source used for both fact collection and
/// later execution. The caller computes this from its held-source contract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DecodeSourceIdentity(String);

impl DecodeSourceIdentity {
    pub fn from_sha256(value: impl Into<String>) -> Result<Self, PlanError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PlanError::InvalidSourceIdentity);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Immutable input facts parsed from selective FFprobe JSON for one selected
/// absolute stream. Fields stay unknown when FFprobe cannot prove them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeFacts {
    input_video_stream: u32,
    selection_provenance: StreamSelectionProvenance,
    codec: Option<String>,
    profile: Option<String>,
    pixel_format: Option<String>,
    chroma_sampling: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: FrameRate,
    bit_depth: Option<u8>,
    dynamic_range: Option<String>,
    source_identity: DecodeSourceIdentity,
    facts_digest: String,
}

#[derive(Serialize)]
struct FactsDigest<'a> {
    input_video_stream: u32,
    selection_provenance: StreamSelectionProvenance,
    codec: &'a Option<String>,
    profile: &'a Option<String>,
    pixel_format: &'a Option<String>,
    chroma_sampling: &'a Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: FrameRate,
    bit_depth: Option<u8>,
    dynamic_range: &'a Option<String>,
    source_identity: &'a DecodeSourceIdentity,
}

impl DecodeFacts {
    /// Parse the first non-attached video stream, preserving its absolute
    /// FFprobe index. Callers that selected a different stream pass its index
    /// to [`Self::from_ffprobe_json_at`].
    pub fn from_ffprobe_json(
        json: &Value,
        source_identity: DecodeSourceIdentity,
    ) -> Result<Self, PlanError> {
        Self::from_ffprobe_json_inner(json, source_identity, None)
    }

    /// Parse one explicitly selected absolute video stream.
    pub fn from_ffprobe_json_at(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        absolute_index: u32,
    ) -> Result<Self, PlanError> {
        Self::from_ffprobe_json_inner(json, source_identity, Some(absolute_index))
    }

    fn from_ffprobe_json_inner(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        requested_index: Option<u32>,
    ) -> Result<Self, PlanError> {
        let streams = json
            .get("streams")
            .and_then(Value::as_array)
            .ok_or(PlanError::MissingVideoStream)?;
        let selected = streams.iter().find(|stream| {
            if stream.get("codec_type").and_then(Value::as_str) != Some("video")
                || attached_picture(stream)
            {
                return false;
            }
            requested_index.is_none_or(|wanted| stream_index(stream) == Some(wanted))
        });
        let selected = selected.ok_or(PlanError::MissingVideoStream)?;
        let input_video_stream = stream_index(selected).ok_or(PlanError::InvalidStreamIndex)?;
        let selection_provenance = if requested_index.is_some() {
            StreamSelectionProvenance::ExplicitAbsoluteIndex
        } else {
            StreamSelectionProvenance::FirstPlayableVideo
        };
        let codec = bounded_token(selected, "codec_name")?;
        let profile = bounded_token(selected, "profile")?;
        let pixel_format = bounded_token(selected, "pix_fmt")?;
        let chroma_sampling = pixel_format.as_deref().and_then(chroma_from_pixel_format);
        let width = positive_u32(selected, "width");
        let height = positive_u32(selected, "height");
        let frame_rate = parse_frame_rate(selected);
        let bit_depth = parse_bit_depth(selected, pixel_format.as_deref());
        let dynamic_range = parse_dynamic_range(selected);
        let digest_input = FactsDigest {
            input_video_stream,
            selection_provenance,
            codec: &codec,
            profile: &profile,
            pixel_format: &pixel_format,
            chroma_sampling: &chroma_sampling,
            width,
            height,
            frame_rate,
            bit_depth,
            dynamic_range: &dynamic_range,
            source_identity: &source_identity,
        };
        let encoded = serde_json::to_vec(&digest_input)
            .map_err(|_| PlanError::InvalidFact("serialization"))?;
        let facts_digest = hex::encode(Sha256::digest(encoded));
        Ok(Self {
            input_video_stream,
            selection_provenance,
            codec,
            profile,
            pixel_format,
            chroma_sampling,
            width,
            height,
            frame_rate,
            bit_depth,
            dynamic_range,
            source_identity,
            facts_digest,
        })
    }

    pub fn input_video_stream(&self) -> u32 {
        self.input_video_stream
    }

    pub fn selection_provenance(&self) -> StreamSelectionProvenance {
        self.selection_provenance
    }

    pub fn codec(&self) -> Option<&str> {
        self.codec.as_deref()
    }

    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    pub fn pixel_format(&self) -> Option<&str> {
        self.pixel_format.as_deref()
    }

    pub fn chroma_sampling(&self) -> Option<&str> {
        self.chroma_sampling.as_deref()
    }

    pub fn width(&self) -> Option<u32> {
        self.width
    }

    pub fn height(&self) -> Option<u32> {
        self.height
    }

    pub fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    pub fn bit_depth(&self) -> Option<u8> {
        self.bit_depth
    }

    pub fn dynamic_range(&self) -> Option<&str> {
        self.dynamic_range.as_deref()
    }

    pub fn source_identity(&self) -> &DecodeSourceIdentity {
        &self.source_identity
    }

    pub fn facts_digest(&self) -> &str {
        &self.facts_digest
    }
}

fn attached_picture(stream: &Value) -> bool {
    stream
        .get("disposition")
        .and_then(|value| value.get("attached_pic"))
        .and_then(Value::as_i64)
        .is_some_and(|value| value != 0)
}

fn stream_index(stream: &Value) -> Option<u32> {
    u32::try_from(stream.get("index")?.as_i64()?).ok()
}

fn bounded_token(stream: &Value, key: &'static str) -> Result<Option<String>, PlanError> {
    let Some(value) = stream.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_FACT_TOKEN_BYTES || value.chars().any(char::is_control) {
        return Err(PlanError::InvalidFact(key));
    }
    Ok(Some(value.to_ascii_lowercase()))
}

fn positive_u32(stream: &Value, key: &str) -> Option<u32> {
    u32::try_from(stream.get(key)?.as_i64()?)
        .ok()
        .filter(|value| *value > 0)
}

fn parse_rational(value: Option<&str>) -> Option<Rational> {
    let (numerator, denominator) = value?.split_once('/')?;
    Rational::new(numerator.parse().ok()?, denominator.parse().ok()?)
}

fn parse_frame_rate(stream: &Value) -> FrameRate {
    let average = parse_rational(stream.get("avg_frame_rate").and_then(Value::as_str));
    let nominal = parse_rational(stream.get("r_frame_rate").and_then(Value::as_str));
    match (average, nominal) {
        (Some(average), Some(nominal)) if average != nominal => FrameRate {
            value: None,
            provenance: FrameRateProvenance::Variable,
        },
        (Some(average), _) => FrameRate {
            value: Some(average),
            provenance: FrameRateProvenance::Average,
        },
        (None, Some(nominal)) => FrameRate {
            value: Some(nominal),
            provenance: FrameRateProvenance::Nominal,
        },
        (None, None) => FrameRate {
            value: None,
            provenance: FrameRateProvenance::Unknown,
        },
    }
}

fn parse_bit_depth(stream: &Value, pixel_format: Option<&str>) -> Option<u8> {
    if let Some(value) = stream
        .get("bits_per_raw_sample")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|value| (1..=32).contains(value))
    {
        return Some(value);
    }
    let pixel_format = pixel_format?;
    if pixel_format.contains("12le")
        || pixel_format.contains("12be")
        || pixel_format.contains("p012")
    {
        Some(12)
    } else if pixel_format.contains("10le")
        || pixel_format.contains("10be")
        || pixel_format.contains("p010")
    {
        Some(10)
    } else if pixel_format.starts_with("yuv") || pixel_format.starts_with("nv12") {
        Some(8)
    } else {
        None
    }
}

fn chroma_from_pixel_format(pixel_format: &str) -> Option<String> {
    ["420", "422", "444"]
        .into_iter()
        .find(|sampling| pixel_format.contains(sampling))
        .map(str::to_owned)
}

fn parse_dynamic_range(stream: &Value) -> Option<String> {
    let has_dolby = stream
        .get("side_data_list")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("side_data_type")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.contains("DOVI") || value.contains("Dolby Vision"))
            })
        });
    if has_dolby {
        return Some("dolby_vision".to_owned());
    }
    match stream.get("color_transfer").and_then(Value::as_str) {
        Some("smpte2084") => Some("hdr10".to_owned()),
        Some("arib-std-b67") => Some("hlg".to_owned()),
        _ => None,
    }
}

/// One build/device qualification row. Optional profile and pixel-format
/// fields are explicit class wildcards, not missing evidence on the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeCapability {
    pub backend: DecodeBackend,
    pub codec: String,
    pub profile: Option<String>,
    pub pixel_format: Option<String>,
    pub status: CapabilityStatus,
}

/// An explicit mapping from a codec class to an inventoried software decoder
/// implementation. A codec family is never treated as an implementation name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareDecoder {
    pub codec: String,
    pub implementation: String,
}

/// Node-local capability snapshot for the exact FFmpeg build and device class.
#[derive(Debug, Clone)]
pub struct DecodeCapabilities {
    capabilities: Vec<DecodeCapability>,
    software_decoders: BTreeMap<String, String>,
}

impl DecodeCapabilities {
    pub fn new(
        capabilities: Vec<DecodeCapability>,
        software_decoders: Vec<SoftwareDecoder>,
    ) -> Result<Self, PlanError> {
        let mut mapped = BTreeMap::new();
        for decoder in software_decoders {
            let codec = validated_capability_token(decoder.codec, "software codec")?;
            let implementation = validated_decoder_name(decoder.implementation)?;
            if mapped.insert(codec, implementation).is_some() {
                return Err(PlanError::DuplicateCapability);
            }
        }
        let mut keys = BTreeSet::new();
        let mut validated = Vec::with_capacity(capabilities.len());
        for capability in capabilities {
            let capability = DecodeCapability {
                backend: capability.backend,
                codec: validated_capability_token(capability.codec, "capability codec")?,
                profile: capability
                    .profile
                    .map(|value| validated_capability_token(value, "capability profile"))
                    .transpose()?,
                pixel_format: capability
                    .pixel_format
                    .map(|value| validated_capability_token(value, "capability pixel format"))
                    .transpose()?,
                status: capability.status,
            };
            let key = (
                capability.backend,
                capability.codec.clone(),
                capability.profile.clone(),
                capability.pixel_format.clone(),
            );
            if !keys.insert(key) {
                return Err(PlanError::DuplicateCapability);
            }
            if validated.iter().any(|existing: &DecodeCapability| {
                existing.backend == capability.backend
                    && existing.codec == capability.codec
                    && capability_specificity(existing) == capability_specificity(&capability)
                    && capability_dimensions_overlap(existing, &capability)
            }) {
                return Err(PlanError::AmbiguousCapability);
            }
            validated.push(capability);
        }
        Ok(Self {
            capabilities: validated,
            software_decoders: mapped,
        })
    }

    fn software_decoder(&self, codec: &str) -> Option<&str> {
        self.software_decoders.get(codec).map(String::as_str)
    }

    fn status(&self, backend: DecodeBackend, facts: &DecodeFacts) -> CapabilityStatus {
        let Some(codec) = facts.codec() else {
            return CapabilityStatus::Unavailable;
        };
        self.capabilities
            .iter()
            .filter(|entry| {
                entry.backend == backend
                    && entry.codec == codec
                    && entry
                        .profile
                        .as_deref()
                        .is_none_or(|profile| facts.profile() == Some(profile))
                    && entry
                        .pixel_format
                        .as_deref()
                        .is_none_or(|pixel_format| facts.pixel_format() == Some(pixel_format))
            })
            .max_by_key(|entry| {
                usize::from(entry.profile.is_some()) + usize::from(entry.pixel_format.is_some())
            })
            .map(|entry| entry.status)
            .unwrap_or(CapabilityStatus::Unavailable)
    }
}

fn capability_specificity(capability: &DecodeCapability) -> usize {
    usize::from(capability.profile.is_some()) + usize::from(capability.pixel_format.is_some())
}

fn capability_dimensions_overlap(left: &DecodeCapability, right: &DecodeCapability) -> bool {
    (left.profile.is_none()
        || right.profile.is_none()
        || left.profile.as_deref() == right.profile.as_deref())
        && (left.pixel_format.is_none()
            || right.pixel_format.is_none()
            || left.pixel_format.as_deref() == right.pixel_format.as_deref())
}

fn validated_capability_token(value: String, field: &'static str) -> Result<String, PlanError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_FACT_TOKEN_BYTES || value.chars().any(char::is_control)
    {
        return Err(PlanError::InvalidFact(field));
    }
    Ok(value.to_ascii_lowercase())
}

fn validated_decoder_name(value: String) -> Result<String, PlanError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_DECODER_NAME_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(PlanError::InvalidSoftwareDecoder);
    }
    Ok(value.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePlanPolicy {
    Legacy,
    Enforce,
}

/// One preparation-time policy read. This type never reads the environment;
/// callers pass the compatibility value they observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodePolicySnapshot {
    plan_policy: DecodePlanPolicy,
    force_software_decode: bool,
    compatibility_value: Option<String>,
    policy_revision: u32,
}

impl DecodePolicySnapshot {
    pub fn new(plan_policy: DecodePlanPolicy, compatibility_value: Option<&str>) -> Self {
        let compatibility_value =
            compatibility_value.map(|value| value.trim().to_ascii_lowercase());
        let force_software_decode = compatibility_value
            .as_deref()
            .is_some_and(|value| matches!(value, "off" | "0" | "false" | "no"));
        Self {
            plan_policy,
            force_software_decode,
            compatibility_value,
            policy_revision: LEGACY_DECODE_POLICY_REVISION,
        }
    }

    pub fn plan_policy(&self) -> DecodePlanPolicy {
        self.plan_policy
    }

    pub fn force_software_decode(&self) -> bool {
        self.force_software_decode
    }

    pub fn compatibility_value(&self) -> Option<&str> {
        self.compatibility_value.as_deref()
    }

    pub fn policy_revision(&self) -> u32 {
        self.policy_revision
    }
}

/// Attempt-scoped or durable continuation restrictions. Construction rejects
/// contradictory requirements instead of relying on selection order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRestrictions {
    excluded: BTreeSet<DecodeBackend>,
    required: Option<DecodeBackend>,
}

impl AttemptRestrictions {
    pub fn none() -> Self {
        Self {
            excluded: BTreeSet::new(),
            required: None,
        }
    }

    pub fn excluding(backends: impl IntoIterator<Item = DecodeBackend>) -> Self {
        Self {
            excluded: backends.into_iter().collect(),
            required: None,
        }
    }

    pub fn requiring(backend: DecodeBackend) -> Self {
        Self {
            excluded: BTreeSet::new(),
            required: Some(backend),
        }
    }

    fn permits(&self, backend: DecodeBackend) -> bool {
        !self.excluded.contains(&backend)
            && self.required.is_none_or(|required| required == backend)
    }
}

/// Semantic subset of today's transcode options. Execution coordinates,
/// paths, pacing, and thread reservations intentionally do not enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeMediaOptions {
    pub target_height: i64,
    pub video_bitrate_kbps: u32,
    pub effective_rate_control: EffectiveRateControl,
    pub audio_channels: u32,
    pub audio_bitrate_kbps: u32,
    pub audio_index: Option<i64>,
    pub audio_offset_ms: i64,
    pub tone_map: ToneMap,
    pub pipeline: Pipeline,
    pub subtitle_burn: Option<SubtitleBurn>,
}

/// Requested semantic pipeline before decode resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeRequest {
    encoder: Encoder,
    options: TranscodeMediaOptions,
}

impl TranscodeRequest {
    pub fn new(encoder: Encoder, options: TranscodeMediaOptions) -> Self {
        Self { encoder, options }
    }

    pub fn encoder(&self) -> Encoder {
        self.encoder
    }

    pub fn options(&self) -> &TranscodeMediaOptions {
        &self.options
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleRendering {
    None,
    TextBurn,
    BitmapBurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputWidthRule {
    PreserveAspectEven,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresentationContract {
    output_grade: OutputGrade,
    output_codec: String,
    output_encoder: String,
    output_profile: Option<String>,
    output_pixel_format: String,
    output_dynamic_range: String,
    output_transfer: String,
    output_matrix: String,
    output_primaries: String,
    width_rule: OutputWidthRule,
    target_height: i64,
    audio_channels: u32,
    audio_bitrate_kbps: u32,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
    subtitle_rendering: SubtitleRendering,
    audio_offset_ms: i64,
}

impl PresentationContract {
    pub fn output_grade(&self) -> OutputGrade {
        self.output_grade
    }

    pub fn output_codec(&self) -> &str {
        &self.output_codec
    }

    pub fn output_encoder(&self) -> &str {
        &self.output_encoder
    }

    pub fn output_profile(&self) -> Option<&str> {
        self.output_profile.as_deref()
    }

    pub fn output_pixel_format(&self) -> &str {
        &self.output_pixel_format
    }

    pub fn output_dynamic_range(&self) -> &str {
        &self.output_dynamic_range
    }

    pub fn output_transfer(&self) -> &str {
        &self.output_transfer
    }

    pub fn output_matrix(&self) -> &str {
        &self.output_matrix
    }

    pub fn output_primaries(&self) -> &str {
        &self.output_primaries
    }

    pub fn width_rule(&self) -> OutputWidthRule {
        self.width_rule
    }

    pub fn target_height(&self) -> i64 {
        self.target_height
    }

    pub fn subtitle_rendering(&self) -> SubtitleRendering {
        self.subtitle_rendering
    }

    pub fn audio_offset_ms(&self) -> i64 {
        self.audio_offset_ms
    }
}

/// Decoder decision validated as part of one complete pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedDecode {
    backend: DecodeBackend,
    software_decoder: Option<String>,
    input_video_stream: u32,
    surface: DecodeSurfaceContract,
    reason: DecodeReason,
    evidence: DecodeEvidence,
    policy_revision: u32,
}

impl ResolvedDecode {
    pub fn backend(&self) -> DecodeBackend {
        self.backend
    }

    pub fn software_decoder(&self) -> Option<&str> {
        self.software_decoder.as_deref()
    }

    pub fn input_video_stream(&self) -> u32 {
        self.input_video_stream
    }

    pub fn surface(&self) -> &DecodeSurfaceContract {
        &self.surface
    }

    pub fn reason(&self) -> DecodeReason {
        self.reason
    }

    pub fn evidence(&self) -> DecodeEvidence {
        self.evidence
    }

    pub fn policy_revision(&self) -> u32 {
        self.policy_revision
    }
}

/// One validated semantic plan. Fields are private so callers cannot create a
/// decoder/renderer/encoder combination that skipped validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTranscode {
    decode: ResolvedDecode,
    encoder: Encoder,
    options: TranscodeMediaOptions,
    source_facts_digest: String,
    output_contract: PresentationContract,
}

impl ResolvedTranscode {
    pub fn decode(&self) -> &ResolvedDecode {
        &self.decode
    }

    pub fn encoder(&self) -> Encoder {
        self.encoder
    }

    pub fn options(&self) -> &TranscodeMediaOptions {
        &self.options
    }

    pub fn source_facts_digest(&self) -> &str {
        &self.source_facts_digest
    }

    pub fn output_contract(&self) -> &PresentationContract {
        &self.output_contract
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    MissingVideoStream,
    InvalidStreamIndex,
    InvalidSourceIdentity,
    InvalidFact(&'static str),
    InvalidSoftwareDecoder,
    DuplicateCapability,
    AmbiguousCapability,
    InvalidMediaOption(&'static str),
    MissingCodec,
    SoftwareDecoderUnavailable,
    IncompatibleRenderer,
    IncompatibleRestriction,
    CapabilityUnavailable(DecodeBackend),
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVideoStream => formatter.write_str("no selected playable video stream"),
            Self::InvalidStreamIndex => {
                formatter.write_str("selected video stream index is invalid")
            }
            Self::InvalidSourceIdentity => formatter.write_str("bound source identity is invalid"),
            Self::InvalidFact(field) => write!(formatter, "decode fact {field} is invalid"),
            Self::InvalidSoftwareDecoder => formatter.write_str("software decoder name is invalid"),
            Self::DuplicateCapability => {
                formatter.write_str("decode capability snapshot contains a duplicate class")
            }
            Self::AmbiguousCapability => {
                formatter.write_str("decode capability snapshot contains overlapping classes")
            }
            Self::InvalidMediaOption(field) => {
                write!(formatter, "transcode media option {field} is invalid")
            }
            Self::MissingCodec => formatter.write_str("selected video codec is unknown"),
            Self::SoftwareDecoderUnavailable => {
                formatter.write_str("no inventoried software decoder covers the selected codec")
            }
            Self::IncompatibleRenderer => {
                formatter.write_str("renderer cannot preserve the requested presentation")
            }
            Self::IncompatibleRestriction => {
                formatter.write_str("attempt restriction excludes every compatible decoder")
            }
            Self::CapabilityUnavailable(backend) => {
                write!(formatter, "{} decode is not qualified", backend.name())
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// Resolve a complete decoder/renderer/encoder plan without environment reads
/// or mutable global state.
pub fn resolve_transcode(
    request: &TranscodeRequest,
    facts: &DecodeFacts,
    capabilities: &DecodeCapabilities,
    policy: &DecodePolicySnapshot,
    restrictions: &AttemptRestrictions,
) -> Result<ResolvedTranscode, PlanError> {
    let codec = facts.codec().ok_or(PlanError::MissingCodec)?;
    let mut options = request.options.clone();
    validate_media_options(&options)?;
    if !options.pipeline.pairs_with(request.encoder)
        || !options.pipeline.handles(facts.dynamic_range())
        || request
            .encoder
            .video_codec_for(options.pipeline.output_grade())
            .is_none()
    {
        return Err(PlanError::IncompatibleRenderer);
    }

    let (mut preferred, mut reason) = preferred_backend(request.encoder, options.pipeline, facts);
    if let Some(required) = restrictions.required {
        preferred = required;
        reason = DecodeReason::ContinuationRestriction;
    } else if !restrictions.permits(preferred) {
        if restrictions.permits(DecodeBackend::Software) {
            preferred = DecodeBackend::Software;
            reason = DecodeReason::ContinuationRestriction;
        } else {
            return Err(PlanError::IncompatibleRestriction);
        }
    }
    if policy.force_software_decode() {
        if !restrictions.permits(DecodeBackend::Software) {
            return Err(PlanError::IncompatibleRestriction);
        }
        preferred = DecodeBackend::Software;
        reason = DecodeReason::OperatorSoftwareOverride;
    } else if request.encoder == Encoder::VideoToolbox && codec == "mpeg4" {
        if !restrictions.permits(DecodeBackend::Software) {
            return Err(PlanError::IncompatibleRestriction);
        }
        preferred = DecodeBackend::Software;
        reason = DecodeReason::CompatibilityExclusion;
    }

    if !pipeline_accepts_decode(options.pipeline, preferred) {
        if preferred == DecodeBackend::Software {
            options.pipeline = grade_preserving_software_renderer(options.pipeline)?;
        } else {
            return Err(PlanError::IncompatibleRenderer);
        }
    }

    let (backend, evidence, reason) = if preferred == DecodeBackend::Software {
        (
            DecodeBackend::Software,
            software_evidence(capabilities, facts, policy)?,
            reason,
        )
    } else {
        match capabilities.status(preferred, facts) {
            CapabilityStatus::Qualified => (
                preferred,
                DecodeEvidence::Qualified,
                if reason == DecodeReason::LegacyPreference {
                    DecodeReason::QualifiedPreference
                } else {
                    reason
                },
            ),
            CapabilityStatus::Advertised if policy.plan_policy() == DecodePlanPolicy::Legacy => {
                (preferred, DecodeEvidence::LegacyUnverified, reason)
            }
            CapabilityStatus::Unavailable if policy.plan_policy() == DecodePlanPolicy::Legacy => {
                // The old route inferred hardware decode from the encoder and
                // did not carry an independent input capability inventory.
                (preferred, DecodeEvidence::LegacyUnverified, reason)
            }
            CapabilityStatus::Rejected
            | CapabilityStatus::Unavailable
            | CapabilityStatus::Advertised => {
                if !restrictions.permits(DecodeBackend::Software) {
                    return Err(PlanError::CapabilityUnavailable(preferred));
                }
                if !options.pipeline.decode_args().is_empty() {
                    options.pipeline = grade_preserving_software_renderer(options.pipeline)?;
                }
                (
                    DecodeBackend::Software,
                    software_evidence(capabilities, facts, policy)?,
                    DecodeReason::CapabilityFallback,
                )
            }
        }
    };

    let software_decoder = if backend == DecodeBackend::Software {
        Some(
            capabilities
                .software_decoder(codec)
                .ok_or(PlanError::SoftwareDecoderUnavailable)?
                .to_owned(),
        )
    } else {
        None
    };
    let output_grade = options.pipeline.output_grade();
    let output_encoder = request
        .encoder
        .video_codec_for(output_grade)
        .ok_or(PlanError::IncompatibleRenderer)?;
    let subtitle_rendering = match options.subtitle_burn.as_ref() {
        None => SubtitleRendering::None,
        Some(subtitle) if subtitle.bitmap => SubtitleRendering::BitmapBurn,
        Some(_) => SubtitleRendering::TextBurn,
    };
    let surface = surface_contract(
        backend,
        options.pipeline,
        facts,
        request.encoder,
        subtitle_rendering,
    );
    let output_contract = PresentationContract {
        output_grade,
        output_codec: match output_grade {
            OutputGrade::Sdr => "h264",
            OutputGrade::Hdr10 => "hevc",
        }
        .to_owned(),
        output_encoder: output_encoder.to_owned(),
        output_profile: match output_grade {
            OutputGrade::Hdr10 => Some("main10".to_owned()),
            OutputGrade::Sdr if request.encoder == Encoder::Software => Some("high".to_owned()),
            OutputGrade::Sdr => None,
        },
        output_pixel_format: output_grade.pixel_format().to_owned(),
        output_dynamic_range: output_grade.delivered_dynamic_range().to_owned(),
        output_transfer: output_grade.transfer().to_owned(),
        output_matrix: output_grade.matrix().to_owned(),
        output_primaries: output_grade.primaries().to_owned(),
        width_rule: OutputWidthRule::PreserveAspectEven,
        target_height: options.target_height,
        audio_channels: options.audio_channels,
        audio_bitrate_kbps: options.audio_bitrate_kbps,
        audio_index: options.audio_index,
        subtitle_index: options
            .subtitle_burn
            .as_ref()
            .map(|subtitle| subtitle.subtitle_index),
        subtitle_rendering,
        audio_offset_ms: options.audio_offset_ms,
    };
    Ok(ResolvedTranscode {
        decode: ResolvedDecode {
            backend,
            software_decoder,
            input_video_stream: facts.input_video_stream,
            surface,
            reason,
            evidence,
            policy_revision: policy.policy_revision,
        },
        encoder: request.encoder,
        options,
        source_facts_digest: facts.facts_digest.clone(),
        output_contract,
    })
}

fn validate_media_options(options: &TranscodeMediaOptions) -> Result<(), PlanError> {
    if options.target_height < 2 {
        return Err(PlanError::InvalidMediaOption("target_height"));
    }
    if options.video_bitrate_kbps == 0 {
        return Err(PlanError::InvalidMediaOption("video_bitrate_kbps"));
    }
    if options.audio_channels == 0 {
        return Err(PlanError::InvalidMediaOption("audio_channels"));
    }
    if options.audio_bitrate_kbps == 0 {
        return Err(PlanError::InvalidMediaOption("audio_bitrate_kbps"));
    }
    if options.audio_index.is_some_and(|index| index < 0) {
        return Err(PlanError::InvalidMediaOption("audio_index"));
    }
    if options
        .subtitle_burn
        .as_ref()
        .is_some_and(|subtitle| subtitle.subtitle_index < 0)
    {
        return Err(PlanError::InvalidMediaOption("subtitle_index"));
    }
    if !(-15_000..=15_000).contains(&options.audio_offset_ms) {
        return Err(PlanError::InvalidMediaOption("audio_offset_ms"));
    }
    Ok(())
}

fn pipeline_accepts_decode(pipeline: Pipeline, backend: DecodeBackend) -> bool {
    match pipeline {
        Pipeline::VppQsv => backend == DecodeBackend::Qsv,
        Pipeline::TonemapVaapi => backend == DecodeBackend::Vaapi,
        Pipeline::DoviTonemapx | Pipeline::DoviPassthrough => backend == DecodeBackend::Software,
        Pipeline::Libplacebo
        | Pipeline::TonemapOpencl
        | Pipeline::Hdr10Passthrough
        | Pipeline::Cpu => true,
    }
}

fn software_evidence(
    capabilities: &DecodeCapabilities,
    facts: &DecodeFacts,
    policy: &DecodePolicySnapshot,
) -> Result<DecodeEvidence, PlanError> {
    let codec = facts.codec().ok_or(PlanError::MissingCodec)?;
    capabilities
        .software_decoder(codec)
        .ok_or(PlanError::SoftwareDecoderUnavailable)?;
    match capabilities.status(DecodeBackend::Software, facts) {
        CapabilityStatus::Qualified => Ok(DecodeEvidence::Qualified),
        CapabilityStatus::Unavailable | CapabilityStatus::Advertised
            if policy.plan_policy() == DecodePlanPolicy::Legacy =>
        {
            Ok(DecodeEvidence::LegacyUnverified)
        }
        CapabilityStatus::Unavailable
        | CapabilityStatus::Advertised
        | CapabilityStatus::Rejected => {
            Err(PlanError::CapabilityUnavailable(DecodeBackend::Software))
        }
    }
}

fn preferred_backend(
    encoder: Encoder,
    pipeline: Pipeline,
    facts: &DecodeFacts,
) -> (DecodeBackend, DecodeReason) {
    if pipeline.requires_software_decode() {
        return (DecodeBackend::Software, DecodeReason::RendererRequirement);
    }
    match pipeline {
        Pipeline::VppQsv => return (DecodeBackend::Qsv, DecodeReason::LegacyPreference),
        Pipeline::TonemapVaapi => {
            return (DecodeBackend::Vaapi, DecodeReason::LegacyPreference);
        }
        _ => {}
    }
    let heavy = matches!(facts.codec(), Some("hevc" | "h265" | "hevc10"))
        && (facts.dynamic_range().is_some() || facts.height().is_some_and(|height| height >= 2160));
    let backend = match encoder {
        Encoder::Software => DecodeBackend::Software,
        Encoder::Nvenc => DecodeBackend::Cuda,
        Encoder::VideoToolbox => DecodeBackend::VideoToolbox,
        Encoder::Qsv if heavy => DecodeBackend::Qsv,
        Encoder::Vaapi if heavy => DecodeBackend::Vaapi,
        Encoder::Qsv | Encoder::Vaapi => DecodeBackend::Software,
    };
    (backend, DecodeReason::LegacyPreference)
}

fn grade_preserving_software_renderer(pipeline: Pipeline) -> Result<Pipeline, PlanError> {
    let fallback = pipeline.fallback().ok_or(PlanError::IncompatibleRenderer)?;
    if fallback.output_grade() != pipeline.output_grade() {
        return Err(PlanError::IncompatibleRenderer);
    }
    Ok(fallback)
}

fn surface_contract(
    backend: DecodeBackend,
    pipeline: Pipeline,
    facts: &DecodeFacts,
    encoder: Encoder,
    subtitle_rendering: SubtitleRendering,
) -> DecodeSurfaceContract {
    let ten_bit =
        facts.bit_depth().is_some_and(|depth| depth >= 10) || facts.dynamic_range().is_some();
    let frame_domain = match backend {
        DecodeBackend::Qsv => FrameDomain::Qsv,
        DecodeBackend::Vaapi => FrameDomain::Vaapi,
        DecodeBackend::Software | DecodeBackend::VideoToolbox | DecodeBackend::Cuda => {
            FrameDomain::SystemMemory
        }
    };
    let remains_vendor_resident = matches!(
        (backend, pipeline),
        (DecodeBackend::Qsv, Pipeline::VppQsv) | (DecodeBackend::Vaapi, Pipeline::TonemapVaapi)
    ) && subtitle_rendering == SubtitleRendering::None;
    let download_format = if matches!(backend, DecodeBackend::Qsv | DecodeBackend::Vaapi)
        && !remains_vendor_resident
    {
        Some(if ten_bit { "p010le" } else { "nv12" }.to_owned())
    } else {
        None
    };
    let upload_domain = if remains_vendor_resident {
        None
    } else {
        match encoder {
            Encoder::Qsv => Some(FrameDomain::Qsv),
            Encoder::Vaapi => Some(FrameDomain::Vaapi),
            Encoder::Software | Encoder::Nvenc | Encoder::VideoToolbox => None,
        }
    };
    let upload_format = match upload_domain {
        Some(FrameDomain::Qsv) if pipeline.output_grade() == OutputGrade::Hdr10 => {
            Some("p010le".to_owned())
        }
        Some(FrameDomain::Qsv | FrameDomain::Vaapi) => Some("nv12".to_owned()),
        Some(FrameDomain::SystemMemory) | None => None,
    };
    DecodeSurfaceContract {
        frame_domain,
        download_format,
        upload_domain,
        upload_format,
        required_side_data: pipeline.requires_software_decode(),
    }
}
