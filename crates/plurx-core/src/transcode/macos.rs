//! Frozen macOS processing observations and implementation identity.
//! Runtime probes populate this context; the existing decoder resolver owns
//! selection. Availability is compatibility evidence, not a benchmark score.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{Pipeline, PlanError};

pub const MACOS_PROCESSING_GRAPH_REVISION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacosProcessingAvailability {
    Pending,
    Available,
    Unavailable,
}

/// Identity of the executable, dependencies and Apple processing environment.
/// No node name, probe time or benchmark result belongs in output identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacosProcessingIdentity {
    ffmpeg_sha256: String,
    ffprobe_sha256: String,
    patch_digest: String,
    linked_libraries_digest: String,
    os_build: String,
    architecture: String,
    hardware_class: String,
    graph_revision: u32,
}

impl MacosProcessingIdentity {
    pub fn new(
        ffmpeg_sha256: String,
        ffprobe_sha256: String,
        patch_digest: String,
        linked_libraries_digest: String,
        os_build: String,
        architecture: String,
        hardware_class: String,
    ) -> Result<Self, PlanError> {
        for value in [
            &ffmpeg_sha256,
            &ffprobe_sha256,
            &patch_digest,
            &linked_libraries_digest,
        ] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(PlanError::InvalidCapabilityIdentity(
                    "macOS implementation digest",
                ));
            }
        }
        for value in [&os_build, &hardware_class] {
            if value.is_empty()
                || value.len() > 128
                || !value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b' ' | b'_' | b'-' | b'.' | b',' | b'/' | b'(' | b')')
                })
            {
                return Err(PlanError::InvalidCapabilityIdentity("macOS environment"));
            }
        }
        if !matches!(architecture.as_str(), "arm64" | "x86_64") {
            return Err(PlanError::InvalidCapabilityIdentity("macOS architecture"));
        }
        Ok(Self {
            ffmpeg_sha256,
            ffprobe_sha256,
            patch_digest,
            linked_libraries_digest,
            os_build,
            architecture,
            hardware_class,
            graph_revision: MACOS_PROCESSING_GRAPH_REVISION,
        })
    }

    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(
            serde_json::to_vec(self).expect("identity serialization is infallible"),
        ))
    }

    pub fn ffmpeg_sha256(&self) -> &str {
        &self.ffmpeg_sha256
    }
    pub fn ffprobe_sha256(&self) -> &str {
        &self.ffprobe_sha256
    }
    pub fn patch_digest(&self) -> &str {
        &self.patch_digest
    }
    pub fn linked_libraries_digest(&self) -> &str {
        &self.linked_libraries_digest
    }
    pub fn os_build(&self) -> &str {
        &self.os_build
    }
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
    pub fn hardware_class(&self) -> &str {
        &self.hardware_class
    }
    pub fn graph_revision(&self) -> u32 {
        self.graph_revision
    }
}

/// Complete independently observed graph tuples. Components do not imply
/// support for another color, subtitle or cadence combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacosProcessingGraph {
    SdrScale,
    Hdr10Metal,
    HlgMetal,
    SdrTextBurn,
    Hdr10TextBurn,
    HlgTextBurn,
    SdrBitmapBurn,
    Hdr10BitmapBurn,
    HlgBitmapBurn,
    SdrBwdifFrame,
    SdrBwdifField,
    HevcSdr,
    HevcHdr10,
    HevcSdrHost,
    HevcHdr10Host,
    LiveSdrUploadScale,
    LiveSdrUploadBwdifFrame,
    LiveSdrUploadBwdifField,
    P5SoftwareCpu,
    P5VtTonemapx,
    P5VtMetal,
    P5SoftwareMetal,
}

impl MacosProcessingGraph {
    /// Exact software-decode upload initialization, shared by runtime probes
    /// and the existing LiveTV plan owner. It does not claim hardware decode.
    pub fn live_upload_init_args(self) -> Option<&'static [&'static str]> {
        match self {
            Self::LiveSdrUploadScale
            | Self::LiveSdrUploadBwdifFrame
            | Self::LiveSdrUploadBwdifField => Some(&[
                "-init_hw_device",
                "videotoolbox=plurx_live_vt",
                "-filter_hw_device",
                "plurx_live_vt",
            ]),
            _ => None,
        }
    }

    /// Project the already-selected exact LiveTV graph into its filter recipe.
    /// Eligibility and source/output binding remain in LiveTvTranscodePlan.
    pub fn live_upload_filter(self, width: u32, height: u32) -> Option<String> {
        if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return None;
        }
        let deinterlace = match self {
            Self::LiveSdrUploadScale => "",
            Self::LiveSdrUploadBwdifFrame => {
                ",bwdif_videotoolbox=mode=send_frame:parity=auto:deint=interlaced"
            }
            Self::LiveSdrUploadBwdifField => {
                ",bwdif_videotoolbox=mode=send_field:parity=auto:deint=interlaced"
            }
            _ => return None,
        };
        let scale =
            Pipeline::VtScaleSdr.filters(Some(i64::from(width)), i64::from(height), None)?;
        Some(format!("format=nv12,hwupload{deinterlace},{scale}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosProcessingContext {
    enabled: bool,
    hevc_output_enabled: bool,
    identity: MacosProcessingIdentity,
    sdr_scale: MacosProcessingAvailability,
    hdr10_metal: MacosProcessingAvailability,
    excluded_pipelines: BTreeSet<Pipeline>,
    graphs: BTreeMap<MacosProcessingGraph, MacosProcessingAvailability>,
}

impl MacosProcessingContext {
    pub fn new(
        enabled: bool,
        identity: MacosProcessingIdentity,
        sdr_scale: MacosProcessingAvailability,
        hdr10_metal: MacosProcessingAvailability,
    ) -> Self {
        Self {
            enabled,
            hevc_output_enabled: false,
            identity,
            sdr_scale,
            hdr10_metal,
            excluded_pipelines: BTreeSet::new(),
            graphs: BTreeMap::new(),
        }
    }

    /// Independent saved output preference; runtime compatibility never
    /// changes this value or the processing preference.
    #[must_use]
    pub fn with_hevc_output_enabled(mut self, enabled: bool) -> Self {
        self.hevc_output_enabled = enabled;
        self
    }

    pub fn hevc_output_enabled(&self) -> bool {
        self.hevc_output_enabled
    }

    /// Runtime evidence for one complete extension tuple. No offline
    /// qualification receipt or operator preference enters this observation.
    #[must_use]
    pub fn with_graph(
        mut self,
        graph: MacosProcessingGraph,
        availability: MacosProcessingAvailability,
    ) -> Self {
        self.graphs.insert(graph, availability);
        self
    }

    pub fn observed_graphs(
        &self,
    ) -> impl Iterator<Item = (MacosProcessingGraph, MacosProcessingAvailability)> + '_ {
        self.graphs
            .iter()
            .map(|(graph, availability)| (*graph, *availability))
    }

    pub fn graph(&self, graph: MacosProcessingGraph) -> MacosProcessingAvailability {
        match graph {
            MacosProcessingGraph::SdrScale => self.sdr_scale,
            MacosProcessingGraph::Hdr10Metal => self.hdr10_metal,
            _ => self
                .graphs
                .get(&graph)
                .copied()
                .unwrap_or(MacosProcessingAvailability::Unavailable),
        }
    }

    /// Existing recovery owners freeze a failed renderer exclusion here.
    /// The saved choice remains intact and no retry policy lives in core.
    #[must_use]
    pub fn excluding_pipeline(mut self, pipeline: Pipeline) -> Self {
        self.excluded_pipelines.insert(pipeline);
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn identity(&self) -> &MacosProcessingIdentity {
        &self.identity
    }
    pub fn sdr_scale(&self) -> MacosProcessingAvailability {
        self.sdr_scale
    }
    pub fn hdr10_metal(&self) -> MacosProcessingAvailability {
        self.hdr10_metal
    }
    pub fn permits(&self, pipeline: Pipeline) -> bool {
        !self.excluded_pipelines.contains(&pipeline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacosProcessingSelection {
    Disabled,
    Selected,
    ProbePending,
    RuntimeProbeFailed,
    IncompatibleInput,
    PresentationConstraint,
    DecoderOverride,
    RecoveryRestriction,
    CapabilityFallback,
}

impl MacosProcessingSelection {
    pub fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Selected => "selected",
            Self::ProbePending => "probe_pending",
            Self::RuntimeProbeFailed => "runtime_probe_failed",
            Self::IncompatibleInput => "incompatible_input",
            Self::PresentationConstraint => "presentation_constraint",
            Self::DecoderOverride => "decoder_override",
            Self::RecoveryRestriction => "recovery_restriction",
            Self::CapabilityFallback => "capability_fallback",
        }
    }
}
