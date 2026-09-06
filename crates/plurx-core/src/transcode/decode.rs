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
use crate::domain::{DolbyVisionFacts, MediaFile};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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

/// Proven input dynamic-range class. `None` on [`DecodeFacts`] remains
/// genuinely unknown; SDR is represented explicitly only when stream color
/// metadata proves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicRangeClass {
    Sdr,
    Hdr10,
    Hdr10Plus,
    Hlg,
    DolbyVision,
}

impl DynamicRangeClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sdr => "sdr",
            Self::Hdr10 => "hdr10",
            Self::Hdr10Plus => "hdr10plus",
            Self::Hlg => "hlg",
            Self::DolbyVision => "dolby_vision",
        }
    }

    fn is_hdr(self) -> bool {
        self != Self::Sdr
    }
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
    MeasuredPreference,
    CapabilityFallback,
}

/// Where decoded frames live before rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameDomain {
    SystemMemory,
    Qsv,
    Vaapi,
    Vulkan,
    OpenCl,
}

/// The validated handoff between the selected decoder and renderer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DecodeSurfaceContract {
    decode_domain: FrameDomain,
    decoder_download_format: Option<String>,
    renderer_domain: FrameDomain,
    renderer_upload_format: Option<String>,
    renderer_download_format: Option<String>,
    encoder_upload_domain: Option<FrameDomain>,
    encoder_upload_format: Option<String>,
    required_side_data: bool,
}

impl DecodeSurfaceContract {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        decode_domain: FrameDomain,
        decoder_download_format: Option<String>,
        renderer_domain: FrameDomain,
        renderer_upload_format: Option<String>,
        renderer_download_format: Option<String>,
        encoder_upload_domain: Option<FrameDomain>,
        encoder_upload_format: Option<String>,
        required_side_data: bool,
    ) -> Result<Self, PlanError> {
        for format in [
            decoder_download_format.as_deref(),
            renderer_upload_format.as_deref(),
            renderer_download_format.as_deref(),
            encoder_upload_format.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if format.is_empty()
                || format.len() > MAX_FACT_TOKEN_BYTES
                || !format
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(PlanError::InvalidFact("surface pixel format"));
            }
        }
        if encoder_upload_domain.is_some() != encoder_upload_format.is_some() {
            return Err(PlanError::InvalidFact("encoder surface upload"));
        }
        Ok(Self {
            decode_domain,
            decoder_download_format,
            renderer_domain,
            renderer_upload_format,
            renderer_download_format,
            encoder_upload_domain,
            encoder_upload_format,
            required_side_data,
        })
    }

    pub fn for_plan(
        backend: DecodeBackend,
        pipeline: Pipeline,
        facts: &DecodeFacts,
        encoder: Encoder,
        subtitle_rendering: SubtitleRendering,
    ) -> Self {
        surface_contract(backend, pipeline, facts, encoder, subtitle_rendering)
    }

    pub fn decode_domain(&self) -> FrameDomain {
        self.decode_domain
    }

    pub fn decoder_download_format(&self) -> Option<&str> {
        self.decoder_download_format.as_deref()
    }

    pub fn renderer_domain(&self) -> FrameDomain {
        self.renderer_domain
    }

    pub fn renderer_upload_format(&self) -> Option<&str> {
        self.renderer_upload_format.as_deref()
    }

    pub fn renderer_download_format(&self) -> Option<&str> {
        self.renderer_download_format.as_deref()
    }

    pub fn encoder_upload_domain(&self) -> Option<FrameDomain> {
        self.encoder_upload_domain
    }

    pub fn encoder_upload_format(&self) -> Option<&str> {
        self.encoder_upload_format.as_deref()
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

/// Already-scanned source metadata that selective on-demand FFprobe output
/// cannot be trusted to reproduce on every supported build. Its digest enters
/// both the fact cache key and the resolved fact digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeCatalogMetadata {
    dynamic_range: Option<DynamicRangeClass>,
    hdr_format: Option<String>,
    dolby_vision: DolbyVisionFacts,
    digest: String,
}

impl DecodeCatalogMetadata {
    pub fn from_media_file(file: &MediaFile) -> Result<Self, PlanError> {
        Self::new(
            file.hdr.as_deref(),
            file.hdr_format.as_deref(),
            file.dolby_vision,
        )
    }

    pub fn new(
        dynamic_range: Option<&str>,
        hdr_format: Option<&str>,
        dolby_vision: DolbyVisionFacts,
    ) -> Result<Self, PlanError> {
        let mut dynamic_range = dynamic_range.map(parse_catalog_dynamic_range).transpose()?;
        let hdr_format = validated_optional_text(hdr_format, "catalog hdr format")?;
        if hdr_format
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("hdr10+"))
        {
            dynamic_range = Some(DynamicRangeClass::Hdr10Plus);
        }
        if !dolby_vision.is_empty() {
            if !matches!(dolby_vision.profile, Some(4 | 5 | 7 | 8 | 9 | 10))
                || !dolby_vision
                    .level
                    .is_none_or(|level| (0..=13).contains(&level))
                || !dolby_vision
                    .bl_compat_id
                    .is_none_or(|id| (0..=6).contains(&id))
            {
                return Err(PlanError::InvalidFact("catalog dolby vision"));
            }
            if dynamic_range != Some(DynamicRangeClass::DolbyVision) {
                return Err(PlanError::ConflictingMetadata("dolby vision"));
            }
            let labelled_base = hdr_format.as_deref().and_then(|format| {
                if format.contains("HDR10-compatible") {
                    Some(DynamicRangeClass::Hdr10)
                } else if format.contains("HLG-compatible") {
                    Some(DynamicRangeClass::Hlg)
                } else {
                    None
                }
            });
            if dolby_vision.profile == Some(5) {
                if !dolby_vision.bl_compat_id.is_none_or(|id| id == 0) || labelled_base.is_some() {
                    return Err(PlanError::ConflictingMetadata("dolby compatibility"));
                }
            } else if let Some(compatibility_id) = dolby_vision.bl_compat_id {
                let typed_base = match compatibility_id {
                    1 | 6 => Some(DynamicRangeClass::Hdr10),
                    4 => Some(DynamicRangeClass::Hlg),
                    _ => None,
                };
                if labelled_base.is_some() && labelled_base != typed_base {
                    return Err(PlanError::ConflictingMetadata("dolby compatibility"));
                }
            }
        }
        let encoded = serde_json::to_vec(&(dynamic_range, hdr_format.as_deref(), dolby_vision))
            .map_err(|_| PlanError::InvalidFact("catalog serialization"))?;
        Ok(Self {
            dynamic_range,
            hdr_format,
            dolby_vision,
            digest: hex::encode(Sha256::digest(encoded)),
        })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

fn parse_catalog_dynamic_range(value: &str) -> Result<DynamicRangeClass, PlanError> {
    let value = value.trim();
    if value.len() > MAX_FACT_TOKEN_BYTES || value.chars().any(char::is_control) {
        return Err(PlanError::InvalidFact("catalog hdr"));
    }
    match value.to_ascii_lowercase().as_str() {
        "hdr10" => Ok(DynamicRangeClass::Hdr10),
        "hdr10plus" | "hdr10+" => Ok(DynamicRangeClass::Hdr10Plus),
        "hlg" => Ok(DynamicRangeClass::Hlg),
        "dolby_vision" => Ok(DynamicRangeClass::DolbyVision),
        _ => Err(PlanError::InvalidFact("catalog hdr")),
    }
}

fn validated_optional_text(
    value: Option<&str>,
    field: &'static str,
) -> Result<Option<String>, PlanError> {
    const MAX_METADATA_TEXT_BYTES: usize = 512;
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_METADATA_TEXT_BYTES || value.chars().any(char::is_control) {
        return Err(PlanError::InvalidFact(field));
    }
    Ok(Some(value.to_owned()))
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
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    dynamic_range: Option<DynamicRangeClass>,
    hdr_format: Option<String>,
    dolby_vision: DolbyVisionFacts,
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
    color_range: &'a Option<String>,
    color_space: &'a Option<String>,
    color_transfer: &'a Option<String>,
    color_primaries: &'a Option<String>,
    dynamic_range: Option<DynamicRangeClass>,
    hdr_format: &'a Option<String>,
    dolby_vision: DolbyVisionFacts,
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
        Self::from_ffprobe_json_inner(json, source_identity, None, None)
    }

    /// Parse the first playable video stream and merge the source's retained
    /// typed HDR/Dolby facts. Contradictions refuse planning rather than
    /// letting a build-specific omission downgrade a known source.
    pub fn from_ffprobe_json_with_catalog(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        catalog: &DecodeCatalogMetadata,
    ) -> Result<Self, PlanError> {
        Self::from_ffprobe_json_inner(json, source_identity, None, Some(catalog))
    }

    /// Parse one explicitly selected absolute video stream.
    pub fn from_ffprobe_json_at(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        absolute_index: u32,
    ) -> Result<Self, PlanError> {
        Self::from_ffprobe_json_inner(json, source_identity, Some(absolute_index), None)
    }

    /// Parse one explicitly selected absolute video stream and merge catalog
    /// metadata already bound to that same stream. Explicit selection follows
    /// FFmpeg's stream specifier semantics exactly, including attached video.
    pub fn from_ffprobe_json_at_with_catalog(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        absolute_index: u32,
        catalog: &DecodeCatalogMetadata,
    ) -> Result<Self, PlanError> {
        Self::from_ffprobe_json_inner(json, source_identity, Some(absolute_index), Some(catalog))
    }

    fn from_ffprobe_json_inner(
        json: &Value,
        source_identity: DecodeSourceIdentity,
        requested_index: Option<u32>,
        catalog: Option<&DecodeCatalogMetadata>,
    ) -> Result<Self, PlanError> {
        let streams = json
            .get("streams")
            .and_then(Value::as_array)
            .ok_or(PlanError::MissingVideoStream)?;
        let selected = streams.iter().find(|stream| {
            if stream.get("codec_type").and_then(Value::as_str) != Some("video") {
                return false;
            }
            // The default fact-selection contract chooses the first playable
            // stream. An explicit absolute index has already been resolved
            // from the command's stream specifier and must not silently skip
            // attached pictures, which FFmpeg counts as video streams.
            if requested_index.is_none() && attached_picture(stream) {
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
        let color_range = bounded_token(selected, "color_range")?;
        let color_space = bounded_token(selected, "color_space")?;
        let color_transfer = bounded_token(selected, "color_transfer")?;
        let color_primaries = bounded_token(selected, "color_primaries")?;
        let probed_dynamic_range = parse_dynamic_range(selected);
        let dynamic_range = merge_dynamic_range(probed_dynamic_range, catalog)?;
        let hdr_format = catalog.and_then(|metadata| metadata.hdr_format.clone());
        let dolby_vision =
            catalog.map_or_else(DolbyVisionFacts::default, |metadata| metadata.dolby_vision);
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
            color_range: &color_range,
            color_space: &color_space,
            color_transfer: &color_transfer,
            color_primaries: &color_primaries,
            dynamic_range,
            hdr_format: &hdr_format,
            dolby_vision,
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
            color_range,
            color_space,
            color_transfer,
            color_primaries,
            dynamic_range,
            hdr_format,
            dolby_vision,
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
        self.dynamic_range.map(DynamicRangeClass::name)
    }

    pub fn dynamic_range_class(&self) -> Option<DynamicRangeClass> {
        self.dynamic_range
    }

    pub fn is_hdr(&self) -> bool {
        self.dynamic_range.is_some_and(DynamicRangeClass::is_hdr)
    }

    fn routing_dynamic_range(&self) -> Option<&'static str> {
        match self.dynamic_range {
            Some(DynamicRangeClass::DolbyVision) if self.dolby_vision.profile == Some(5) => {
                Some(DynamicRangeClass::DolbyVision.name())
            }
            Some(DynamicRangeClass::DolbyVision) => match self.dolby_vision.bl_compat_id {
                Some(1 | 6) => Some(DynamicRangeClass::Hdr10.name()),
                Some(4) => Some(DynamicRangeClass::Hlg.name()),
                Some(_) => Some(DynamicRangeClass::DolbyVision.name()),
                None if self.dolby_vision.is_empty()
                    && self
                        .hdr_format
                        .as_deref()
                        .is_some_and(|format| format.contains("HDR10-compatible")) =>
                {
                    Some(DynamicRangeClass::Hdr10.name())
                }
                None if self.dolby_vision.is_empty()
                    && self
                        .hdr_format
                        .as_deref()
                        .is_some_and(|format| format.contains("HLG-compatible")) =>
                {
                    Some(DynamicRangeClass::Hlg.name())
                }
                _ => Some(DynamicRangeClass::DolbyVision.name()),
            },
            Some(dynamic_range) if dynamic_range.is_hdr() => Some(dynamic_range.name()),
            _ => None,
        }
    }

    fn requires_dolby_rpu_processing(&self) -> bool {
        self.dynamic_range == Some(DynamicRangeClass::DolbyVision)
            && self.routing_dynamic_range() == Some(DynamicRangeClass::DolbyVision.name())
    }

    pub fn color_range(&self) -> Option<&str> {
        self.color_range.as_deref()
    }

    pub fn color_space(&self) -> Option<&str> {
        self.color_space.as_deref()
    }

    pub fn color_transfer(&self) -> Option<&str> {
        self.color_transfer.as_deref()
    }

    pub fn color_primaries(&self) -> Option<&str> {
        self.color_primaries.as_deref()
    }

    pub fn hdr_format(&self) -> Option<&str> {
        self.hdr_format.as_deref()
    }

    pub fn dolby_vision(&self) -> DolbyVisionFacts {
        self.dolby_vision
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
        // The presentation contract guarantees an even result without
        // upscaling. A one-pixel dimension has no positive even
        // representation and therefore remains unknown rather than becoming
        // a falsely claimed two-pixel output.
        .filter(|value| *value >= 2)
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
    if pixel_format.starts_with("p010") {
        return Some(10);
    }
    if pixel_format.starts_with("p012") {
        return Some(12);
    }
    if let Some(planar) = pixel_format.rsplit_once('p').map(|(_, suffix)| suffix) {
        let digits = planar
            .bytes()
            .take_while(u8::is_ascii_digit)
            .collect::<Vec<_>>();
        if !digits.is_empty() {
            return std::str::from_utf8(&digits)
                .ok()?
                .parse::<u8>()
                .ok()
                .filter(|depth| (1..=32).contains(depth));
        }
    }
    if (pixel_format.starts_with("yuv") && pixel_format.contains('p'))
        || matches!(pixel_format, "nv12" | "nv21")
    {
        return Some(8);
    }
    None
}

fn chroma_from_pixel_format(pixel_format: &str) -> Option<String> {
    if pixel_format.starts_with("nv12")
        || pixel_format.starts_with("nv21")
        || pixel_format.starts_with("p010")
        || pixel_format.starts_with("p012")
    {
        return Some("420".to_owned());
    }
    ["420", "422", "444"]
        .into_iter()
        .find(|sampling| pixel_format.contains(sampling))
        .map(str::to_owned)
}

fn parse_dynamic_range(stream: &Value) -> Option<DynamicRangeClass> {
    let side_data = stream
        .get("side_data_list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("side_data_type").and_then(Value::as_str))
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if side_data
        .iter()
        .any(|value| value.contains("dovi") || value.contains("dolby vision"))
    {
        return Some(DynamicRangeClass::DolbyVision);
    }
    if side_data
        .iter()
        .any(|value| value.contains("smpte2094-40") || value.contains("dynamic hdr plus"))
    {
        return Some(DynamicRangeClass::Hdr10Plus);
    }
    match stream
        .get("color_transfer")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("smpte2084") => Some(DynamicRangeClass::Hdr10),
        Some("arib-std-b67") => Some(DynamicRangeClass::Hlg),
        Some("bt709" | "iec61966-2-1") => Some(DynamicRangeClass::Sdr),
        _ => None,
    }
}

fn merge_dynamic_range(
    probed: Option<DynamicRangeClass>,
    catalog: Option<&DecodeCatalogMetadata>,
) -> Result<Option<DynamicRangeClass>, PlanError> {
    let catalog_range = catalog.and_then(|metadata| metadata.dynamic_range);
    match (probed, catalog_range) {
        (Some(probed), Some(catalog)) if probed == catalog => Ok(Some(probed)),
        // A selective probe commonly retains only the PQ transfer and omits
        // the side data that distinguishes HDR10+, or Dolby Vision. The
        // catalog is a compatible refinement of that common HDR10 base.
        (
            Some(DynamicRangeClass::Hdr10),
            Some(refined @ (DynamicRangeClass::Hdr10Plus | DynamicRangeClass::DolbyVision)),
        ) => Ok(Some(refined)),
        (Some(DynamicRangeClass::Hlg), Some(DynamicRangeClass::DolbyVision))
            if catalog.is_some_and(|metadata| match metadata.dolby_vision.bl_compat_id {
                Some(id) => id == 4,
                None => metadata
                    .hdr_format
                    .as_deref()
                    .is_some_and(|format| format.contains("HLG-compatible")),
            }) =>
        {
            Ok(Some(DynamicRangeClass::DolbyVision))
        }
        // Conversely, a richer probe result must not be downgraded by a
        // coarser catalog row.
        (
            Some(refined @ (DynamicRangeClass::Hdr10Plus | DynamicRangeClass::DolbyVision)),
            Some(DynamicRangeClass::Hdr10),
        ) => Ok(Some(refined)),
        (Some(_), Some(_)) => Err(PlanError::ConflictingMetadata("dynamic range")),
        (Some(probed), None) => Ok(Some(probed)),
        (None, catalog) => Ok(catalog),
    }
}

/// Exact identity of the build and device environment that produced a
/// capability snapshot. It is deliberately node-local and never contains a
/// device path or physical identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeCapabilitySnapshotIdentity {
    ffmpeg_build_digest: String,
    device_class: String,
    driver_environment_digest: Option<String>,
}

impl DecodeCapabilitySnapshotIdentity {
    pub fn new(
        ffmpeg_build_digest: String,
        device_class: String,
        driver_environment_digest: Option<String>,
    ) -> Result<Self, PlanError> {
        validate_sha256(&ffmpeg_build_digest)
            .map_err(|_| PlanError::InvalidCapabilityIdentity("ffmpeg build digest"))?;
        let device_class = validated_capability_token(device_class, "device class")?;
        if !device_class
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
        {
            return Err(PlanError::InvalidCapabilityIdentity("device class"));
        }
        if let Some(digest) = driver_environment_digest.as_deref() {
            validate_sha256(digest)
                .map_err(|_| PlanError::InvalidCapabilityIdentity("driver environment digest"))?;
        }
        Ok(Self {
            ffmpeg_build_digest,
            device_class,
            driver_environment_digest,
        })
    }

    pub fn ffmpeg_build_digest(&self) -> &str {
        &self.ffmpeg_build_digest
    }

    pub fn device_class(&self) -> &str {
        &self.device_class
    }

    pub fn driver_environment_digest(&self) -> Option<&str> {
        self.driver_environment_digest.as_deref()
    }
}

fn validate_sha256(value: &str) -> Result<(), ()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(())
    }
}

/// One build/device qualification row. A qualified row has no wildcards: it
/// describes the exact tested input class, maximum geometry/pixel rate, and
/// the full surface-transfer contract exercised by the fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeCapability {
    pub backend: DecodeBackend,
    pub codec: String,
    pub profile: Option<String>,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<u8>,
    pub dynamic_range: Option<DynamicRangeClass>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_pixel_rate: Option<u64>,
    pub surface: Option<DecodeSurfaceContract>,
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
    identity: DecodeCapabilitySnapshotIdentity,
    capabilities: Vec<DecodeCapability>,
    software_decoders: BTreeMap<String, String>,
}

impl DecodeCapabilities {
    pub fn new(
        identity: DecodeCapabilitySnapshotIdentity,
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
                bit_depth: capability.bit_depth,
                dynamic_range: capability.dynamic_range,
                max_width: capability.max_width,
                max_height: capability.max_height,
                max_pixel_rate: capability.max_pixel_rate,
                surface: capability.surface,
                status: capability.status,
            };
            if capability.status == CapabilityStatus::Qualified
                && (capability.profile.is_none()
                    || capability.pixel_format.is_none()
                    || capability.bit_depth.is_none()
                    || capability.dynamic_range.is_none()
                    || capability.max_width.is_none()
                    || capability.max_height.is_none()
                    || capability.max_pixel_rate.is_none()
                    || capability.surface.is_none())
            {
                return Err(PlanError::IncompleteQualifiedCapability);
            }
            if capability
                .bit_depth
                .is_some_and(|depth| !(1..=32).contains(&depth))
                || capability.max_width == Some(0)
                || capability.max_height == Some(0)
                || capability.max_pixel_rate == Some(0)
            {
                return Err(PlanError::InvalidFact("capability envelope"));
            }
            let key = (
                capability.backend,
                capability.codec.clone(),
                capability.profile.clone(),
                capability.pixel_format.clone(),
                capability.bit_depth,
                capability.dynamic_range,
                capability.max_width,
                capability.max_height,
                capability.max_pixel_rate,
                capability.surface.clone(),
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
            identity,
            capabilities: validated,
            software_decoders: mapped,
        })
    }

    pub fn identity(&self) -> &DecodeCapabilitySnapshotIdentity {
        &self.identity
    }

    fn software_decoder(&self, codec: &str) -> Option<&str> {
        self.software_decoders.get(codec).map(String::as_str)
    }

    fn status(
        &self,
        backend: DecodeBackend,
        facts: &DecodeFacts,
        surface: &DecodeSurfaceContract,
    ) -> CapabilityStatus {
        let Some(codec) = facts.codec() else {
            return CapabilityStatus::Unavailable;
        };
        let matching = self
            .capabilities
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
                    && entry
                        .bit_depth
                        .is_none_or(|depth| facts.bit_depth() == Some(depth))
                    && entry
                        .dynamic_range
                        .is_none_or(|range| facts.dynamic_range_class() == Some(range))
                    && entry
                        .max_width
                        .is_none_or(|width| facts.width().is_some_and(|actual| actual <= width))
                    && entry
                        .max_height
                        .is_none_or(|height| facts.height().is_some_and(|actual| actual <= height))
                    && entry.max_pixel_rate.is_none_or(|limit| {
                        source_pixel_rate(facts).is_some_and(|actual| actual <= limit)
                    })
                    && entry
                        .surface
                        .as_ref()
                        .is_none_or(|expected| expected == surface)
            })
            .collect::<Vec<_>>();
        let most_specific_rejection = matching
            .iter()
            .filter(|entry| entry.status == CapabilityStatus::Rejected)
            .map(|entry| capability_specificity(entry))
            .max();
        let most_specific_qualification = matching
            .iter()
            .filter(|entry| entry.status == CapabilityStatus::Qualified)
            .map(|entry| capability_specificity(entry))
            .max();
        if let Some(rejected) = most_specific_rejection {
            if most_specific_qualification.is_none_or(|qualified| qualified <= rejected) {
                return CapabilityStatus::Rejected;
            }
        }
        if most_specific_qualification.is_some() {
            return CapabilityStatus::Qualified;
        }
        matching
            .into_iter()
            .max_by_key(|entry| capability_specificity(entry))
            .map(|entry| entry.status)
            .unwrap_or(CapabilityStatus::Unavailable)
    }
}

fn capability_specificity(capability: &DecodeCapability) -> usize {
    [
        capability.profile.is_some(),
        capability.pixel_format.is_some(),
        capability.bit_depth.is_some(),
        capability.dynamic_range.is_some(),
        capability.max_width.is_some(),
        capability.max_height.is_some(),
        capability.max_pixel_rate.is_some(),
        capability.surface.is_some(),
    ]
    .into_iter()
    .map(usize::from)
    .sum()
}

fn capability_dimensions_overlap(left: &DecodeCapability, right: &DecodeCapability) -> bool {
    (left.profile.is_none()
        || right.profile.is_none()
        || left.profile.as_deref() == right.profile.as_deref())
        && (left.pixel_format.is_none()
            || right.pixel_format.is_none()
            || left.pixel_format.as_deref() == right.pixel_format.as_deref())
        && (left.bit_depth.is_none()
            || right.bit_depth.is_none()
            || left.bit_depth == right.bit_depth)
        && (left.dynamic_range.is_none()
            || right.dynamic_range.is_none()
            || left.dynamic_range == right.dynamic_range)
        && (left.surface.is_none() || right.surface.is_none() || left.surface == right.surface)
}

fn source_pixel_rate(facts: &DecodeFacts) -> Option<u64> {
    let width = u128::from(facts.width()?);
    let height = u128::from(facts.height()?);
    let rate = facts.frame_rate().value()?;
    let numerator = width
        .checked_mul(height)?
        .checked_mul(u128::from(rate.numerator()))?;
    let denominator = u128::from(rate.denominator());
    u64::try_from(numerator.div_ceil(denominator)).ok()
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
        let compatibility_value = compatibility_value
            .filter(|value| {
                value.len() <= MAX_FACT_TOKEN_BYTES && !value.chars().any(char::is_control)
            })
            .map(str::to_owned);
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
    requested_max_height: u32,
    effective_width: Option<u32>,
    effective_height: Option<u32>,
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

    pub fn requested_max_height(&self) -> u32 {
        self.requested_max_height
    }

    pub fn effective_width(&self) -> Option<u32> {
        self.effective_width
    }

    pub fn effective_height(&self) -> Option<u32> {
        self.effective_height
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
    ConflictingMetadata(&'static str),
    InvalidSoftwareDecoder,
    InvalidCapabilityIdentity(&'static str),
    IncompleteQualifiedCapability,
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
            Self::ConflictingMetadata(field) => {
                write!(formatter, "decode metadata disagrees about {field}")
            }
            Self::InvalidSoftwareDecoder => formatter.write_str("software decoder name is invalid"),
            Self::InvalidCapabilityIdentity(field) => {
                write!(formatter, "decode capability {field} is invalid")
            }
            Self::IncompleteQualifiedCapability => formatter
                .write_str("qualified decode capability is missing tested envelope evidence"),
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
    let requested_max_height = u32::try_from(options.target_height)
        .map_err(|_| PlanError::InvalidMediaOption("target_height"))?;
    if !options.pipeline.pairs_with(request.encoder)
        || !options.pipeline.handles(facts.routing_dynamic_range())
        || (facts.requires_dolby_rpu_processing()
            && !matches!(
                options.pipeline,
                Pipeline::DoviTonemapx | Pipeline::DoviPassthrough
            ))
        || request
            .encoder
            .video_codec_for(options.pipeline.output_grade())
            .is_none()
    {
        return Err(PlanError::IncompatibleRenderer);
    }
    let subtitle_rendering = match options.subtitle_burn.as_ref() {
        None => SubtitleRendering::None,
        Some(subtitle) if subtitle.bitmap => SubtitleRendering::BitmapBurn,
        Some(_) => SubtitleRendering::TextBurn,
    };

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
        let surface = surface_contract(
            DecodeBackend::Software,
            options.pipeline,
            facts,
            request.encoder,
            subtitle_rendering,
        );
        (
            DecodeBackend::Software,
            software_evidence(capabilities, facts, &surface, policy)?,
            reason,
        )
    } else {
        let preferred_surface = surface_contract(
            preferred,
            options.pipeline,
            facts,
            request.encoder,
            subtitle_rendering,
        );
        match capabilities.status(preferred, facts, &preferred_surface) {
            CapabilityStatus::Qualified => (preferred, DecodeEvidence::Qualified, reason),
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
                let surface = surface_contract(
                    DecodeBackend::Software,
                    options.pipeline,
                    facts,
                    request.encoder,
                    subtitle_rendering,
                );
                (
                    DecodeBackend::Software,
                    software_evidence(capabilities, facts, &surface, policy)?,
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
    let surface = surface_contract(
        backend,
        options.pipeline,
        facts,
        request.encoder,
        subtitle_rendering,
    );
    let effective_geometry = effective_output_geometry(facts, requested_max_height);
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
        requested_max_height,
        effective_width: effective_geometry.map(|geometry| geometry.0),
        effective_height: effective_geometry.map(|geometry| geometry.1),
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

fn effective_output_geometry(facts: &DecodeFacts, requested_max_height: u32) -> Option<(u32, u32)> {
    let source_width = u64::from(facts.width?);
    let source_height = u64::from(facts.height?);
    let height = u64::from(requested_max_height).min(source_height).max(2) & !1;
    let width =
        ((source_width.checked_mul(height)? + source_height / 2) / source_height).max(2) & !1;
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
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
    surface: &DecodeSurfaceContract,
    policy: &DecodePolicySnapshot,
) -> Result<DecodeEvidence, PlanError> {
    let codec = facts.codec().ok_or(PlanError::MissingCodec)?;
    capabilities
        .software_decoder(codec)
        .ok_or(PlanError::SoftwareDecoderUnavailable)?;
    match capabilities.status(DecodeBackend::Software, facts, surface) {
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
        && (facts.is_hdr() || facts.height().is_some_and(|height| height >= 2160));
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
    let ten_bit = facts.bit_depth().is_some_and(|depth| depth >= 10) || facts.is_hdr();
    let decode_domain = match backend {
        DecodeBackend::Qsv => FrameDomain::Qsv,
        DecodeBackend::Vaapi => FrameDomain::Vaapi,
        DecodeBackend::Software | DecodeBackend::VideoToolbox | DecodeBackend::Cuda => {
            FrameDomain::SystemMemory
        }
    };
    let vendor_native = matches!(
        (decode_domain, pipeline),
        (FrameDomain::Qsv, Pipeline::VppQsv) | (FrameDomain::Vaapi, Pipeline::TonemapVaapi)
    );
    let decoder_download_format =
        if matches!(decode_domain, FrameDomain::Qsv | FrameDomain::Vaapi) && !vendor_native {
            Some(if ten_bit { "p010le" } else { "nv12" }.to_owned())
        } else {
            None
        };
    let renderer_domain = match pipeline {
        Pipeline::VppQsv => FrameDomain::Qsv,
        Pipeline::TonemapVaapi => FrameDomain::Vaapi,
        Pipeline::Libplacebo => FrameDomain::Vulkan,
        Pipeline::TonemapOpencl if facts.is_hdr() => FrameDomain::OpenCl,
        Pipeline::TonemapOpencl
        | Pipeline::DoviTonemapx
        | Pipeline::DoviPassthrough
        | Pipeline::Hdr10Passthrough
        | Pipeline::Cpu => FrameDomain::SystemMemory,
    };
    let renderer_upload_format = match renderer_domain {
        FrameDomain::Vulkan => decoder_download_format
            .clone()
            .or_else(|| facts.pixel_format.clone()),
        FrameDomain::OpenCl => Some("p010le".to_owned()),
        FrameDomain::Qsv | FrameDomain::Vaapi | FrameDomain::SystemMemory => None,
    };
    let renderer_download_format = match pipeline {
        Pipeline::Libplacebo => Some("nv12".to_owned()),
        Pipeline::TonemapOpencl if facts.is_hdr() => Some("nv12".to_owned()),
        Pipeline::VppQsv | Pipeline::TonemapVaapi
            if subtitle_rendering != SubtitleRendering::None =>
        {
            Some("nv12".to_owned())
        }
        Pipeline::VppQsv
        | Pipeline::TonemapVaapi
        | Pipeline::TonemapOpencl
        | Pipeline::DoviTonemapx
        | Pipeline::DoviPassthrough
        | Pipeline::Hdr10Passthrough
        | Pipeline::Cpu => None,
    };
    let rendered_domain = if renderer_download_format.is_some() {
        FrameDomain::SystemMemory
    } else {
        renderer_domain
    };
    let encoder_upload_domain = match encoder {
        Encoder::Qsv if rendered_domain != FrameDomain::Qsv => Some(FrameDomain::Qsv),
        Encoder::Vaapi if rendered_domain != FrameDomain::Vaapi => Some(FrameDomain::Vaapi),
        Encoder::Software
        | Encoder::Nvenc
        | Encoder::Qsv
        | Encoder::Vaapi
        | Encoder::VideoToolbox => None,
    };
    let encoder_upload_format = match encoder_upload_domain {
        Some(FrameDomain::Qsv) if pipeline.output_grade() == OutputGrade::Hdr10 => {
            Some("p010le".to_owned())
        }
        Some(FrameDomain::Qsv | FrameDomain::Vaapi) => Some("nv12".to_owned()),
        Some(FrameDomain::SystemMemory | FrameDomain::Vulkan | FrameDomain::OpenCl) | None => None,
    };
    DecodeSurfaceContract {
        decode_domain,
        decoder_download_format,
        renderer_domain,
        renderer_upload_format,
        renderer_download_format,
        encoder_upload_domain,
        encoder_upload_format,
        required_side_data: pipeline.requires_software_decode(),
    }
}
