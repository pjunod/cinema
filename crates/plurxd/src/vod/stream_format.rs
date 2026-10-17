//! Facts from the immutable attached recipe, not current node preferences.
use super::*;

#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamFormat {
    pub rendition_id: String,
    pub video_codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_action: &'static str,
    pub dimensions_basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_aspect_ratio: Option<SampleAspectRatio>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_label: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_build: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SampleAspectRatio {
    pub numerator: u32,
    pub denominator: u32,
}

impl StreamFormat {
    pub(super) fn for_rendition(rendition: &Rendition) -> Option<Self> {
        if let Some(encoding) = rendition.recipe.encoding.as_ref() {
            // A shared soundtrack has no video descriptor of its own.
            if encoding.shared_audio.is_some() {
                return None;
            }
            let plan = &encoding.plan;
            let contract = plan.output_contract();
            let pipeline = plan.options().pipeline;
            let pipeline_label = if pipeline == plurx_core::transcode::Pipeline::Cpu
                && !(plan.input_is_hdr()
                    && contract.output_grade() == plurx_core::transcode::OutputGrade::Sdr)
            {
                "CPU video processing"
            } else {
                pipeline.label()
            };
            return Some(Self {
                rendition_id: rendition.key.clone(),
                video_codec: contract.output_codec().to_owned(),
                width: contract.effective_width(),
                height: contract.effective_height(),
                video_action: "encode",
                dimensions_basis: "recipe",
                sample_aspect_ratio: contract.normalized_geometry().map(|geometry| {
                    SampleAspectRatio {
                        numerator: geometry.sample_aspect_ratio.numerator(),
                        denominator: geometry.sample_aspect_ratio.denominator(),
                    }
                }),
                encoder: Some(contract.output_encoder().to_owned()),
                pipeline: Some(pipeline.name()),
                pipeline_label: Some(pipeline_label),
                dynamic_range: Some(contract.output_dynamic_range().to_owned()),
                ffmpeg_build: Some(encoding.ffmpeg_build.clone()),
            });
        }
        let source = &rendition.recipe.file;
        let codec = source
            .video_codec
            .as_ref()
            .filter(|codec| !codec.is_empty())?;
        Some(Self {
            rendition_id: rendition.key.clone(),
            video_codec: codec.clone(),
            width: source
                .width
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0),
            height: source
                .height
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0),
            video_action: "copy",
            dimensions_basis: "source_copy",
            sample_aspect_ratio: None,
            encoder: Some("copy".into()),
            pipeline: None,
            pipeline_label: Some("Stream copy"),
            // Copy can change DV signaling; source HDR labels cannot answer
            // for its delivered grade. Existing negotiation retains that fact.
            dynamic_range: None,
            ffmpeg_build: None,
        })
    }
}

pub(super) fn tone_map_peak(rendition: &Rendition) -> Option<(u32, &'static str)> {
    let plan = &rendition.recipe.encoding.as_ref()?.plan;
    if !plan.input_is_hdr()
        || plan.output_contract().output_grade() != plurx_core::transcode::OutputGrade::Sdr
    {
        return None;
    }
    let options = plan.options();
    let explicit = if options.pipeline == plurx_core::transcode::Pipeline::Cpu {
        options.tone_map == plurx_core::transcode::ToneMap::Zscale
    } else {
        options
            .pipeline
            .tone_map_peak_option(plan.routing_dynamic_range(), options.tone_map_peak_nits)
            .is_some()
    };
    explicit.then_some((
        options.tone_map_peak_nits,
        options.tone_map_peak_source.name(),
    ))
}
