//! Embedded Mac smoke inputs and node-local, immutable compatibility reports.
//!
//! The caller launches explicit reprobes after startup. This owner never stores
//! the operator preference, selects a production plan, or schedules retries.
//! Production graph spelling belongs to `Pipeline`, including processing order.

mod live;
mod p5;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use plurx_core::transcode::{
    MacosProcessingAvailability, MacosProcessingContext, MacosProcessingGraph,
    MacosProcessingIdentity,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MANIFEST: &[u8] = include_bytes!("../fixtures/macos-processing/manifest.json");
const SDR8: &[u8] = include_bytes!("../fixtures/macos-processing/sdr8.mp4");
const SDR10: &[u8] = include_bytes!("../fixtures/macos-processing/sdr10.mp4");
const HDR10: &[u8] = include_bytes!("../fixtures/macos-processing/hdr10.mp4");
const EXTENSIONS: &[u8] = include_bytes!("../fixtures/macos-processing/extensions.json");
const CUE_SUP: &[u8] = include_bytes!("../fixtures/macos-processing/cue.sup");
const BURN_ASS: &[u8] = include_bytes!("../fixtures/macos-processing/burn.ass");
const PGS_SDR: &[u8] = include_bytes!("../fixtures/macos-processing/pgs-sdr.mkv");
const PGS_SDR10: &[u8] = include_bytes!("../fixtures/macos-processing/pgs-sdr10.mkv");
const PGS_HDR10: &[u8] = include_bytes!("../fixtures/macos-processing/pgs-hdr10.mkv");
const HLG: &[u8] = include_bytes!("../fixtures/macos-processing/hlg.mp4");
const EXTENSION_MEDIA: &[(&str, &[u8])] = &[
    ("pgs_sdr", PGS_SDR),
    ("pgs_sdr10", PGS_SDR10),
    ("pgs_hdr10", PGS_HDR10),
    ("hlg", HLG),
];
const CORPUS_BUDGET: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeReason {
    Pending,
    Available,
    UnsupportedPlatform,
    InvalidEmbeddedCorpus,
    CacheUnavailable,
    PreparationTimedOut,
    Cancelled,
    IdentityUnavailable,
    UnsupportedDependencies,
    DependencyInventoryUnavailable,
    ImplementationChanged,
    MissingFilter,
    GraphFailed,
    GraphTimedOut,
    OutputContractFailed,
}

impl ProbeReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "probe_pending",
            Self::Available => "available",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::InvalidEmbeddedCorpus => "invalid_embedded_corpus",
            Self::CacheUnavailable => "fixture_cache_unavailable",
            Self::PreparationTimedOut => "fixture_preparation_timed_out",
            Self::Cancelled => "cancelled",
            Self::IdentityUnavailable => "implementation_identity_unavailable",
            Self::UnsupportedDependencies => "unsupported_implementation_dependencies",
            Self::DependencyInventoryUnavailable => {
                "implementation_dependency_inventory_unavailable"
            }
            Self::ImplementationChanged => "implementation_changed",
            Self::MissingFilter => "missing_filter",
            Self::GraphFailed => "runtime_graph_failed",
            Self::GraphTimedOut => "runtime_graph_timed_out",
            Self::OutputContractFailed => "output_contract_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GraphObservation {
    pub(crate) availability: MacosProcessingAvailability,
    pub(crate) reason: ProbeReason,
}

impl GraphObservation {
    fn pending() -> Self {
        Self {
            availability: MacosProcessingAvailability::Pending,
            reason: ProbeReason::Pending,
        }
    }

    fn from_result(result: Result<(), ProbeReason>) -> Self {
        match result {
            Ok(()) => Self {
                availability: MacosProcessingAvailability::Available,
                reason: ProbeReason::Available,
            },
            Err(reason) => Self {
                availability: MacosProcessingAvailability::Unavailable,
                reason,
            },
        }
    }

    fn diagnostics(&self) -> Value {
        let availability = match self.availability {
            MacosProcessingAvailability::Pending => "pending",
            MacosProcessingAvailability::Available => "available",
            MacosProcessingAvailability::Unavailable => "unavailable",
        };
        json!({"availability": availability, "reason": self.reason.as_str()})
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MacosVideoReport {
    pub(crate) generation: u64,
    pub(crate) sdr_scale: GraphObservation,
    pub(crate) hdr10_metal: GraphObservation,
    graphs: BTreeMap<MacosProcessingGraph, GraphObservation>,
    identity: Option<MacosProcessingIdentity>,
}

impl MacosVideoReport {
    fn pending(generation: u64) -> Self {
        Self {
            generation,
            sdr_scale: GraphObservation::pending(),
            hdr10_metal: GraphObservation::pending(),
            graphs: BTreeMap::new(),
            identity: None,
        }
    }

    fn unavailable(generation: u64, reason: ProbeReason) -> Self {
        Self {
            generation,
            sdr_scale: GraphObservation::from_result(Err(reason)),
            hdr10_metal: GraphObservation::from_result(Err(reason)),
            graphs: BTreeMap::new(),
            identity: None,
        }
    }

    /// Capture one report for one new plan; readiness never modifies `enabled`.
    pub(crate) fn context(&self, enabled: bool) -> Option<MacosProcessingContext> {
        self.identity.as_ref().map(|identity| {
            self.graphs.iter().fold(
                MacosProcessingContext::new(
                    enabled,
                    identity.clone(),
                    self.sdr_scale.availability,
                    self.hdr10_metal.availability,
                ),
                |context, (graph, observation)| {
                    context.with_graph(*graph, observation.availability)
                },
            )
        })
    }

    /// Bounded wire identifiers; no command paths, scores or hostnames.
    pub(crate) fn diagnostics(&self) -> Value {
        let identity = self.identity.as_ref().map(|identity| {
            json!({
                "digest": identity.digest(),
                "ffmpeg_sha256": identity.ffmpeg_sha256(),
                "ffprobe_sha256": identity.ffprobe_sha256(),
                "patch_digest": identity.patch_digest(),
                "linked_libraries_digest": identity.linked_libraries_digest(),
                "os_build": identity.os_build(),
                "architecture": identity.architecture(),
                "hardware_class": identity.hardware_class(),
                "graph_revision": identity.graph_revision(),
            })
        });
        json!({
            "generation": self.generation,
            "implementation": identity,
            "sdr_scale": self.sdr_scale.diagnostics(),
            "hdr10_metal": self.hdr10_metal.diagnostics(),
            "graphs": self.graphs.iter().map(|(graph, observation)| (serde_json::to_value(graph).expect("enum serializes").as_str().expect("enum string").to_owned(), observation.diagnostics())).collect::<serde_json::Map<String, Value>>(),
            "qualification": "external_advisory",
            "dependency_inventory_tool": "otool",
            "supported_dependency_scope": "apple_system_only",
        })
    }
}

pub(crate) struct MacosVideoProbe {
    runtime_cache: PathBuf,
    report: RwLock<Arc<MacosVideoReport>>,
    serial: tokio::sync::Mutex<()>,
}

impl MacosVideoProbe {
    pub(crate) fn new(runtime_cache: PathBuf) -> Self {
        let initial = if cfg!(target_os = "macos") {
            MacosVideoReport::pending(0)
        } else {
            MacosVideoReport::unavailable(0, ProbeReason::UnsupportedPlatform)
        };
        Self {
            runtime_cache,
            report: RwLock::new(Arc::new(initial)),
            serial: tokio::sync::Mutex::new(()),
        }
    }

    /// Synchronous Arc clone for the existing synchronous movie-plan resolver.
    pub(crate) fn snapshot(&self) -> Arc<MacosVideoReport> {
        Arc::clone(
            &self
                .report
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Integration tests inject independent node observations while the
    /// manager remains the sole owner of the saved processing preference.
    #[cfg(test)]
    pub(crate) fn publish_context_for_test(&self, context: &MacosProcessingContext) {
        fn observation(availability: MacosProcessingAvailability) -> GraphObservation {
            match availability {
                MacosProcessingAvailability::Available => GraphObservation::from_result(Ok(())),
                MacosProcessingAvailability::Pending => GraphObservation::pending(),
                MacosProcessingAvailability::Unavailable => {
                    GraphObservation::from_result(Err(ProbeReason::GraphFailed))
                }
            }
        }
        self.publish(MacosVideoReport {
            generation: self.snapshot().generation.saturating_add(1),
            identity: Some(context.identity().clone()),
            sdr_scale: observation(context.sdr_scale()),
            hdr10_metal: observation(context.hdr10_metal()),
            graphs: context
                .observed_graphs()
                .map(|(graph, availability)| (graph, observation(availability)))
                .collect(),
        });
    }

    fn publish(&self, report: MacosVideoReport) -> Arc<MacosVideoReport> {
        let report = Arc::new(report);
        *self
            .report
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(&report);
        report
    }

    /// An overlapping request observes the in-flight generation immediately;
    /// there is no queue, detached retry or competing use of the GPU.
    pub(crate) async fn reprobe(&self, cancelled: &CancellationToken) -> Arc<MacosVideoReport> {
        let Ok(_serial) = self.serial.try_lock() else {
            return self.snapshot();
        };
        let generation = self.snapshot().generation.saturating_add(1);
        if cancelled.is_cancelled() {
            return self.publish(MacosVideoReport::unavailable(
                generation,
                ProbeReason::Cancelled,
            ));
        }
        self.publish(MacosVideoReport::pending(generation));
        let corpus = match embedded_corpus(MANIFEST, SDR8, SDR10, HDR10) {
            Ok(corpus) => corpus,
            Err(reason) => return self.publish(MacosVideoReport::unavailable(generation, reason)),
        };
        let final_report = run_generation(self, generation, corpus, cancelled).await;
        self.publish(final_report)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Corpus {
    schema_version: u32,
    corpus_version: u32,
    generator_recipe_version: u32,
    size_budget_bytes: usize,
    aggregate_media_bytes: usize,
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    id: String,
    path: String,
    sha256: String,
    byte_length: usize,
    class: String,
    role: String,
    available: bool,
    duration_seconds: f64,
    expected: InputFacts,
    #[serde(default)]
    input_pixels: Value,
    output_expectations: Vec<OutputExpectation>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExtensionCorpus {
    schema_version: u32,
    generator_recipe_version: u32,
    fixtures: Vec<ExtensionFixture>,
    auxiliary: Vec<AuxiliaryFixture>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExtensionFixture {
    id: String,
    path: String,
    sha256: String,
    byte_length: usize,
    source_class: String,
    operation: String,
    output_expectation: OutputExpectation,
}

#[derive(Debug, Clone, Deserialize)]
struct AuxiliaryFixture {
    path: String,
    sha256: String,
    byte_length: usize,
}

fn extension_corpus() -> Result<ExtensionCorpus, ProbeReason> {
    let invalid = ProbeReason::InvalidEmbeddedCorpus;
    let corpus: ExtensionCorpus = serde_json::from_slice(EXTENSIONS).map_err(|_| invalid)?;
    if corpus.schema_version != 1
        || corpus.generator_recipe_version != 1
        || corpus.fixtures.len() != EXTENSION_MEDIA.len()
        || corpus.auxiliary.len() != 2
        || live::MANIFEST.len()
            + live::MEDIA
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + p5::MANIFEST.len()
            + p5::MEDIA
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + EXTENSIONS.len()
            + BURN_ASS.len()
            + EXTENSION_MEDIA
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + MANIFEST.len()
            + SDR8.len()
            + SDR10.len()
            + HDR10.len()
            > CORPUS_BUDGET
    {
        return Err(invalid);
    }
    for (fixture, (id, bytes)) in corpus.fixtures.iter().zip(EXTENSION_MEDIA) {
        let (class, operation, path) = match *id {
            "pgs_sdr" => ("sdr", "bitmap_burn", "pgs-sdr.mkv"),
            "pgs_sdr10" => ("sdr", "bitmap_burn", "pgs-sdr10.mkv"),
            "pgs_hdr10" => ("hdr10", "bitmap_burn", "pgs-hdr10.mkv"),
            "hlg" => ("hlg", "plain", "hlg.mp4"),
            _ => return Err(invalid),
        };
        if fixture.id != *id
            || fixture.path != path
            || fixture.source_class != class
            || fixture.operation != operation
            || fixture.sha256 != digest(bytes)
            || fixture.byte_length != bytes.len()
            || bytes.is_empty()
        {
            return Err(invalid);
        }
        validate_contract(
            &fixture.output_expectation,
            class != "sdr",
            *id != "pgs_sdr10" && class == "sdr",
        )?;
    }
    let ass = &corpus.auxiliary[0];
    if ass.path != "burn.ass" || ass.sha256 != digest(BURN_ASS) || ass.byte_length != BURN_ASS.len()
    {
        return Err(invalid);
    }
    let cue = &corpus.auxiliary[1];
    if cue.path != "cue.sup" || cue.sha256 != digest(CUE_SUP) || cue.byte_length != CUE_SUP.len() {
        return Err(invalid);
    }
    Ok(corpus)
}

#[derive(Debug, Clone, Deserialize)]
struct InputFacts {
    codec_name: String,
    profile: String,
    pix_fmt: String,
    width: usize,
    height: usize,
    frame_count: usize,
    avg_frame_rate: String,
    sample_aspect_ratio: String,
    field_order: String,
    color_primaries: String,
    color_transfer: String,
    color_space: String,
    color_range: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OutputExpectation {
    graph_id: String,
    expected: OutputFacts,
    timestamps: Timestamps,
    pixels: PixelExpectation,
    forbidden_side_data: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OutputFacts {
    codec_name: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    codec_tag_string: Option<String>,
    width: usize,
    height: usize,
    frame_count: usize,
    avg_frame_rate: String,
    color_primaries: String,
    color_transfer: String,
    color_space: String,
    color_range: String,
    sample_aspect_ratio: String,
    field_order: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Timestamps {
    start_seconds: f64,
    step_seconds: f64,
    absolute_tolerance_seconds: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct PixelExpectation {
    format: String,
    sample_radius: usize,
    neutral_gray_max_chroma_distance_from_128: f64,
    patches: Vec<Patch>,
    expected_y: Option<Vec<f64>>,
    absolute_y_tolerance: Option<f64>,
    ordered_patch_indices: Option<Vec<usize>>,
    ordering_tolerance_y: Option<f64>,
    minimum_black_to_peak_y_difference: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct Patch {
    x: usize,
    y: usize,
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn embedded_corpus(
    manifest: &[u8],
    sdr8: &[u8],
    sdr10: &[u8],
    hdr10: &[u8],
) -> Result<Corpus, ProbeReason> {
    let invalid = ProbeReason::InvalidEmbeddedCorpus;
    extension_corpus()?;
    live::fixtures()?;
    if manifest.len() > 128 * 1024 {
        return Err(invalid);
    }
    let corpus: Corpus = serde_json::from_slice(manifest).map_err(|_| invalid)?;
    if corpus.schema_version != 1
        || corpus.corpus_version != 1
        || corpus.generator_recipe_version != 1
        || corpus.size_budget_bytes != CORPUS_BUDGET
        || corpus.fixtures.len() != 3
        || manifest
            .len()
            .saturating_add(sdr8.len())
            .saturating_add(sdr10.len())
            .saturating_add(hdr10.len())
            > CORPUS_BUDGET
        || corpus.aggregate_media_bytes != sdr8.len() + sdr10.len() + hdr10.len()
    {
        return Err(invalid);
    }
    for (fixture, (identity, bytes)) in
        corpus
            .fixtures
            .iter()
            .zip([("sdr8", sdr8), ("sdr10", sdr10), ("hdr10", hdr10)])
    {
        let hdr = identity == "hdr10";
        let bits8 = identity == "sdr8";
        let e = &fixture.expected;
        if fixture.id != identity
            || fixture.path != format!("{identity}.mp4")
            || fixture.sha256 != digest(bytes)
            || fixture.byte_length != bytes.len()
            || bytes.is_empty()
            || fixture.class != if hdr { "hdr10" } else { "sdr" }
            || fixture.role != "smoke"
            || !fixture.available
            || fixture.duration_seconds != 1.0
            || e.codec_name != "hevc"
            || e.profile != if bits8 { "Main" } else { "Main 10" }
            || e.pix_fmt != if bits8 { "yuv420p" } else { "yuv420p10le" }
            || e.width != 320
            || e.height != 180
            || e.frame_count != 12
            || e.avg_frame_rate != "12/1"
            || e.sample_aspect_ratio != "1:1"
            || e.field_order != "progressive"
            || e.color_range != "tv"
            || e.color_primaries != if hdr { "bt2020" } else { "bt709" }
            || e.color_transfer != if hdr { "smpte2084" } else { "bt709" }
            || e.color_space != if hdr { "bt2020nc" } else { "bt709" }
        {
            return Err(invalid);
        }
        let graph = if hdr {
            "vt_tonemap_metal"
        } else {
            "vt_scale_sdr"
        };
        let matches: Vec<_> = fixture
            .output_expectations
            .iter()
            .filter(|e| e.graph_id == graph)
            .collect();
        if matches.len() != 1 {
            return Err(invalid);
        }
        validate_contract(matches[0], hdr, bits8)?;
    }
    Ok(corpus)
}

fn validate_contract(
    contract: &OutputExpectation,
    hdr: bool,
    bits8: bool,
) -> Result<(), ProbeReason> {
    let e = &contract.expected;
    let t = &contract.timestamps;
    let p = &contract.pixels;
    let forbidden = [
        "DOVI configuration record",
        "Dolby Vision RPU Data",
        "Mastering display metadata",
        "Content light level metadata",
    ];
    if e.codec_name != "h264"
        || e.width != 160
        || e.height != 90
        || e.frame_count != 12
        || e.avg_frame_rate != "12/1"
        || e.sample_aspect_ratio != "1:1"
        || e.field_order != "progressive"
        || e.color_primaries != "bt709"
        || e.color_transfer != "bt709"
        || e.color_space != "bt709"
        || e.color_range != "tv"
        || t.start_seconds != 0.0
        || (t.step_seconds - 1.0 / 12.0).abs() > f64::EPSILON
        || t.absolute_tolerance_seconds != 0.00002
        || p.format != "decoded_yuv420p"
        || p.sample_radius != 2
        || p.neutral_gray_max_chroma_distance_from_128 != 12.0
        || p.patches.len() != 8
        || p.patches
            .iter()
            .enumerate()
            .any(|(i, patch)| patch.x != 10 + 20 * i || patch.y != 30)
        || forbidden.iter().any(|name| {
            !contract
                .forbidden_side_data
                .iter()
                .any(|actual| actual == name)
        })
    {
        return Err(ProbeReason::InvalidEmbeddedCorpus);
    }
    let correct_pixels = if hdr {
        p.expected_y.is_none()
            && p.ordered_patch_indices.as_deref() == Some(&[0, 1, 2, 3, 4, 5, 6])
            && p.ordering_tolerance_y == Some(4.0)
            && p.minimum_black_to_peak_y_difference == Some(24.0)
    } else {
        p.expected_y.as_deref()
            == Some(&[
                16.0,
                26.0,
                57.0,
                80.0,
                123.0,
                if bits8 { 171.0 } else { 170.0 },
                235.0,
                235.0,
            ])
            && p.absolute_y_tolerance == Some(12.0)
    };
    if !correct_pixels {
        return Err(ProbeReason::InvalidEmbeddedCorpus);
    }
    Ok(())
}

fn observe_output(
    document: &Value,
    raw: &[u8],
    contract: &OutputExpectation,
) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    let streams = document
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(fail)?;
    let frames = document
        .get("frames")
        .and_then(Value::as_array)
        .ok_or(fail)?;
    let e = &contract.expected;
    if streams.len() != 1
        || frames.len() != e.frame_count
        || raw.len() != e.width * e.height * 3 / 2 * e.frame_count
    {
        return Err(fail);
    }
    let stream = &streams[0];
    for (key, expected) in [
        ("codec_name", e.codec_name.as_str()),
        ("avg_frame_rate", e.avg_frame_rate.as_str()),
        ("sample_aspect_ratio", e.sample_aspect_ratio.as_str()),
        ("field_order", e.field_order.as_str()),
    ] {
        if stream.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(fail);
        }
    }
    for (key, expected) in [
        ("profile", e.profile.as_deref()),
        ("pix_fmt", e.pix_fmt.as_deref()),
        ("codec_tag_string", e.codec_tag_string.as_deref()),
    ] {
        if expected
            .is_some_and(|expected| stream.get(key).and_then(Value::as_str) != Some(expected))
        {
            return Err(fail);
        }
    }
    for frame in std::iter::once(stream).chain(frames.iter()) {
        if frame.get("width").and_then(Value::as_u64) != Some(e.width as u64)
            || frame.get("height").and_then(Value::as_u64) != Some(e.height as u64)
        {
            return Err(fail);
        }
        for (key, expected) in [
            ("color_primaries", e.color_primaries.as_str()),
            ("color_transfer", e.color_transfer.as_str()),
            ("color_space", e.color_space.as_str()),
            ("color_range", e.color_range.as_str()),
        ] {
            if frame.get(key).and_then(Value::as_str) != Some(expected) {
                return Err(fail);
            }
        }
        if frame
            .get("side_data_list")
            .and_then(Value::as_array)
            .is_some_and(|sides| {
                sides.iter().any(|side| {
                    let name = side
                        .get("side_data_type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let lower = name.to_ascii_lowercase();
                    contract
                        .forbidden_side_data
                        .iter()
                        .any(|forbidden| forbidden == name)
                        || lower.contains("dovi")
                        || lower.contains("dolby")
                })
            })
        {
            return Err(fail);
        }
    }
    for (index, frame) in frames.iter().enumerate() {
        if frame.get("interlaced_frame").and_then(Value::as_u64) != Some(0) {
            return Err(fail);
        }
        let pts: f64 = frame
            .get("best_effort_timestamp_time")
            .and_then(Value::as_str)
            .ok_or(fail)?
            .parse()
            .map_err(|_| fail)?;
        let target =
            contract.timestamps.start_seconds + index as f64 * contract.timestamps.step_seconds;
        if !pts.is_finite() || (pts - target).abs() > contract.timestamps.absolute_tolerance_seconds
        {
            return Err(fail);
        }
        let start = index * e.width * e.height * 3 / 2;
        let pixels = &contract.pixels;
        let mut gray = Vec::with_capacity(8);
        for patch in &pixels.patches {
            let radius = pixels.sample_radius;
            let mut sum = 0_u32;
            for y in patch.y - radius..=patch.y + radius {
                for x in patch.x - radius..=patch.x + radius {
                    sum += u32::from(raw[start + y * e.width + x]);
                }
            }
            gray.push(f64::from(sum) / ((2 * radius + 1).pow(2)) as f64);
            let chroma = patch.y / 2 * (e.width / 2) + patch.x / 2;
            for plane in 0..2 {
                let pos = start + e.width * e.height + plane * e.width * e.height / 4 + chroma;
                if (f64::from(raw[pos]) - 128.0).abs()
                    > pixels.neutral_gray_max_chroma_distance_from_128
                {
                    return Err(fail);
                }
            }
        }
        if let Some(expected) = &pixels.expected_y {
            let tolerance = pixels.absolute_y_tolerance.ok_or(fail)?;
            if gray
                .iter()
                .zip(expected)
                .any(|(actual, expected)| (actual - expected).abs() > tolerance)
            {
                return Err(fail);
            }
        } else {
            let order = pixels.ordered_patch_indices.as_ref().ok_or(fail)?;
            let tolerance = pixels.ordering_tolerance_y.ok_or(fail)?;
            if order
                .windows(2)
                .any(|pair| gray[pair[0]] > gray[pair[1]] + tolerance)
                || gray[*order.last().ok_or(fail)?] - gray[order[0]]
                    < pixels.minimum_black_to_peak_y_difference.ok_or(fail)?
            {
                return Err(fail);
            }
        }
    }
    Ok(())
}

const PREPARATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
const IDENTITY_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
const GRAPH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
const ALL_GRAPHS_BUDGET: std::time::Duration = std::time::Duration::from_secs(210);
const PROBE_WORK: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("Mac processing compatibility probe");

struct PreparedCorpus {
    directory: plurx_core::fs_secure::SecureDirectory,
    path: PathBuf,
}

struct PreparationFence(Arc<std::sync::atomic::AtomicBool>);

impl Drop for PreparationFence {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

async fn prepare_corpus(
    root: &std::path::Path,
    corpus: &Corpus,
    cancelled: &CancellationToken,
) -> Result<PreparedCorpus, ProbeReason> {
    let deadline = std::time::Instant::now() + PREPARATION_BUDGET;
    let prepare = async {
        let fence = PreparationFence(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        let parent = plurx_core::fs_secure::SecureDirectory::open(root)
            .await
            .map_err(|_| ProbeReason::CacheUnavailable)?;
        let owner = parent
            .create_child_directory("macos-processing")
            .await
            .map_err(|_| ProbeReason::CacheUnavailable)?;
        verify_private_directory(&owner).await?;
        let key = digest(&[MANIFEST, EXTENSIONS, live::MANIFEST, p5::MANIFEST].concat());
        let directory = owner
            .create_child_directory(&key)
            .await
            .map_err(|_| ProbeReason::CacheUnavailable)?;
        verify_private_directory(&directory).await?;
        let path = root.join("macos-processing").join(key);
        for (name, bytes) in std::iter::once(("manifest.json".to_owned(), MANIFEST))
            .chain(
                corpus
                    .fixtures
                    .iter()
                    .zip([SDR8, SDR10, HDR10])
                    .map(|(fixture, bytes)| (format!("{}.mp4", fixture.sha256), bytes)),
            )
            .chain(std::iter::once(("burn.ass".to_owned(), BURN_ASS)))
            .chain(
                EXTENSION_MEDIA
                    .iter()
                    .map(|(_, bytes)| (format!("{}.mp4", digest(bytes)), *bytes)),
            )
            .chain(
                live::MEDIA
                    .iter()
                    .map(|(_, bytes)| (format!("{}.mp4", digest(bytes)), *bytes)),
            )
            .chain(
                p5::MEDIA
                    .iter()
                    .map(|(_, bytes)| (format!("{}.mp4", digest(bytes)), *bytes)),
            )
        {
            if cancelled.is_cancelled() {
                return Err(ProbeReason::Cancelled);
            }
            let existing = directory
                .read_bounded_child(&name, bytes.len() as u64)
                .await;
            if existing
                .as_ref()
                .is_ok_and(|existing| existing.as_slice() == bytes)
            {
                continue;
            }
            let active = Arc::clone(&fence.0);
            let token = cancelled.clone();
            directory
                .atomic_write_child_cooperative(&name, bytes, move || {
                    active.load(std::sync::atomic::Ordering::Acquire)
                        && !token.is_cancelled()
                        && std::time::Instant::now() < deadline
                })
                .await
                .map_err(|_| {
                    if cancelled.is_cancelled() {
                        ProbeReason::Cancelled
                    } else if std::time::Instant::now() >= deadline {
                        ProbeReason::PreparationTimedOut
                    } else {
                        ProbeReason::CacheUnavailable
                    }
                })?;
            let written = directory
                .read_bounded_child(&name, bytes.len() as u64)
                .await
                .map_err(|_| ProbeReason::CacheUnavailable)?;
            if written.as_slice() != bytes {
                return Err(ProbeReason::CacheUnavailable);
            }
        }
        Ok(PreparedCorpus { directory, path })
    };
    tokio::select! {
        biased;
        () = cancelled.cancelled() => Err(ProbeReason::Cancelled),
        result = tokio::time::timeout(PREPARATION_BUDGET, prepare) =>
            result.unwrap_or(Err(ProbeReason::PreparationTimedOut)),
    }
}

#[cfg(unix)]
async fn verify_private_directory(
    directory: &plurx_core::fs_secure::SecureDirectory,
) -> Result<(), ProbeReason> {
    let directory = directory.clone();
    tokio::task::spawn_blocking(move || {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the cloned authority keeps this descriptor alive; fstat writes
        // exactly one stat and its return value is checked before initialization.
        if unsafe { libc::fstat(directory.raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(ProbeReason::CacheUnavailable);
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_uid != unsafe { libc::geteuid() } || stat.st_mode & 0o077 != 0 {
            return Err(ProbeReason::CacheUnavailable);
        }
        Ok(())
    })
    .await
    .map_err(|_| ProbeReason::CacheUnavailable)?
}

#[cfg(not(unix))]
async fn verify_private_directory(
    _directory: &plurx_core::fs_secure::SecureDirectory,
) -> Result<(), ProbeReason> {
    Err(ProbeReason::UnsupportedPlatform)
}

struct Implementation {
    ffmpeg: crate::ffmpeg::EncodedExecutable,
    ffprobe: crate::ffmpeg::EncodedExecutable,
    identity: MacosProcessingIdentity,
    filters: String,
}

// The admitted Jellyfin distribution statically supplies its non-Apple libraries.
// Both programs must prove that every dynamic install name belongs to macOS.
// This avoids pretending that a direct otool listing attests arbitrary @rpath
// or third-party transitive dependencies. The immutable Apple closure is bound
// by the actual OS build, not by a process-local fragment-index cache key.
fn apple_dependency_report(report: &[u8]) -> Result<String, ProbeReason> {
    let text = std::str::from_utf8(report).map_err(|_| ProbeReason::IdentityUnavailable)?;
    let mut lines = text.lines();
    if !lines.next().is_some_and(|line| line.ends_with(':')) {
        return Err(ProbeReason::IdentityUnavailable);
    }
    let mut dependencies = Vec::new();
    for line in lines {
        let line = line.trim();
        let (path, versions) = line
            .split_once(" (compatibility version ")
            .ok_or(ProbeReason::UnsupportedDependencies)?;
        if !(path.starts_with("/usr/lib/") || path.starts_with("/System/Library/"))
            || path
                .split('/')
                .skip(1)
                .any(|part| matches!(part, "" | "." | ".."))
            || !versions.ends_with(')')
            || !line
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(ProbeReason::UnsupportedDependencies);
        }
        dependencies.push(line);
    }
    if dependencies.is_empty() {
        return Err(ProbeReason::UnsupportedDependencies);
    }
    dependencies.sort_unstable();
    dependencies.dedup();
    Ok(dependencies.join("\n"))
}

fn apple_dependencies_digest(os_build: &str, ffmpeg: &str, ffprobe: &str) -> String {
    let mut linked = Sha256::new();
    linked.update(b"plurx/macos-processing/apple-dependencies/v1\0");
    linked.update((os_build.len() as u64).to_be_bytes());
    linked.update(os_build.as_bytes());
    for (role, report) in [("ffmpeg", ffmpeg), ("ffprobe", ffprobe)] {
        linked.update(role.as_bytes());
        linked.update((report.len() as u64).to_be_bytes());
        linked.update(report.as_bytes());
    }
    hex::encode(linked.finalize())
}

fn unmodified_dyld_environment() -> bool {
    !std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with("DYLD_"))
}

async fn identity_command(
    command: tokio::process::Command,
    deadline: tokio::time::Instant,
    cancelled: &CancellationToken,
    failure: ProbeReason,
) -> Result<Vec<u8>, ProbeReason> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(ProbeReason::IdentityUnavailable);
    }
    // Never wrap a live child in timeout_at: its bounded owner must kill/reap
    // before this generation releases its serial ownership.
    crate::ffmpeg::bounded_command_output_cancellable(
        command,
        remaining,
        1024 * 1024,
        "Mac implementation identity",
        Some(cancelled),
        PROBE_WORK,
    )
    .await
    .map(|output| output.stdout)
    .map_err(|_| {
        if cancelled.is_cancelled() {
            ProbeReason::Cancelled
        } else {
            if tokio::time::Instant::now() >= deadline {
                ProbeReason::IdentityUnavailable
            } else {
                failure
            }
        }
    })
}

async fn capture_implementation(
    cancelled: &CancellationToken,
) -> Result<Implementation, ProbeReason> {
    let deadline = tokio::time::Instant::now() + IDENTITY_BUDGET;
    if !unmodified_dyld_environment() {
        return Err(ProbeReason::UnsupportedDependencies);
    }
    let executable_capture = async {
        let ffmpeg = crate::ffmpeg::EncodedExecutable::capture()
            .await
            .map_err(|_| ProbeReason::IdentityUnavailable)?;
        let ffprobe =
            crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::bound_ffprobe_bin())
                .await
                .map_err(|_| ProbeReason::IdentityUnavailable)?;
        Ok::<_, ProbeReason>((ffmpeg, ffprobe))
    };
    // These file-only operations cannot publish media or retain child ownership.
    let (ffmpeg, ffprobe) = bounded_identity_io(deadline, cancelled, executable_capture).await?;
    let (os_build, hardware_class) = platform_facts()?;
    let mut reports = Vec::with_capacity(2);
    for executable in [&ffmpeg, &ffprobe] {
        let mut command = tokio::process::Command::new("/usr/bin/otool");
        command.arg("-L").arg(&executable.path);
        reports.push(apple_dependency_report(
            &identity_command(
                command,
                deadline,
                cancelled,
                ProbeReason::DependencyInventoryUnavailable,
            )
            .await?,
        )?);
    }
    let linked = apple_dependencies_digest(&os_build, &reports[0], &reports[1]);
    let identity = MacosProcessingIdentity::new(
        ffmpeg.digest.clone(),
        ffprobe.digest.clone(),
        ffmpeg.digest.clone(),
        linked,
        os_build,
        processing_architecture(std::env::consts::ARCH)?.to_owned(),
        hardware_class,
    )
    .map_err(|_| ProbeReason::IdentityUnavailable)?;
    let mut command = tokio::process::Command::new(&ffmpeg.path);
    command.args(["-hide_banner", "-filters"]);
    let filters = String::from_utf8(
        identity_command(
            command,
            deadline,
            cancelled,
            ProbeReason::IdentityUnavailable,
        )
        .await?,
    )
    .map_err(|_| ProbeReason::IdentityUnavailable)?;
    let implementation = Implementation {
        ffmpeg,
        ffprobe,
        identity,
        filters,
    };
    if !bounded_identity_io(deadline, cancelled, async {
        Ok(implementation_is_current(&implementation).await)
    })
    .await?
    {
        return Err(ProbeReason::ImplementationChanged);
    }
    Ok(implementation)
}

async fn bounded_identity_io<T>(
    deadline: tokio::time::Instant,
    cancelled: &CancellationToken,
    work: impl std::future::Future<Output = Result<T, ProbeReason>>,
) -> Result<T, ProbeReason> {
    tokio::select! {
        biased;
        () = cancelled.cancelled() => Err(ProbeReason::Cancelled),
        result = tokio::time::timeout_at(deadline, work) =>
            result.unwrap_or(Err(ProbeReason::IdentityUnavailable)),
    }
}

fn processing_architecture(architecture: &str) -> Result<&'static str, ProbeReason> {
    match architecture {
        "aarch64" | "arm64" => Ok("arm64"),
        "x86_64" => Ok("x86_64"),
        _ => Err(ProbeReason::IdentityUnavailable),
    }
}

#[cfg(target_os = "macos")]
fn platform_facts() -> Result<(String, String), ProbeReason> {
    fn read(name: &std::ffi::CStr) -> Result<String, ProbeReason> {
        let mut bytes = [0_u8; 128];
        let mut length = bytes.len();
        // SAFETY: the name is NUL terminated and the fixed buffer and its
        // capacity are passed together. sysctl is read-only (no new value).
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || length == 0 || length > bytes.len() {
            return Err(ProbeReason::IdentityUnavailable);
        }
        let end = bytes[..length]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(length);
        let value =
            std::str::from_utf8(&bytes[..end]).map_err(|_| ProbeReason::IdentityUnavailable)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(ProbeReason::IdentityUnavailable);
        }
        Ok(value.to_owned())
    }
    Ok((read(c"kern.osversion")?, read(c"hw.model")?))
}

#[cfg(not(target_os = "macos"))]
fn platform_facts() -> Result<(String, String), ProbeReason> {
    Err(ProbeReason::UnsupportedPlatform)
}

async fn implementation_is_current(implementation: &Implementation) -> bool {
    // Resolve the CURRENT configuration again. Merely statting the captured
    // canonical path misses a configured symlink moving A -> B while A survives.
    let Ok(ffmpeg) = crate::ffmpeg::EncodedExecutable::capture().await else {
        return false;
    };
    let Ok(ffprobe) =
        crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::bound_ffprobe_bin())
            .await
    else {
        return false;
    };
    same_executable(&ffmpeg, &implementation.ffmpeg)
        && same_executable(&ffprobe, &implementation.ffprobe)
        && ffmpeg.is_current().await
        && ffprobe.is_current().await
        && unmodified_dyld_environment()
        && platform_facts().is_ok_and(|(os_build, hardware_class)| {
            os_build == implementation.identity.os_build()
                && hardware_class == implementation.identity.hardware_class()
        })
}

fn same_executable(
    current: &crate::ffmpeg::EncodedExecutable,
    captured: &crate::ffmpeg::EncodedExecutable,
) -> bool {
    current.attestation_object() == captured.attestation_object()
        && current.digest == captured.digest
}

async fn run_generation(
    owner: &MacosVideoProbe,
    generation: u64,
    corpus: Corpus,
    cancelled: &CancellationToken,
) -> MacosVideoReport {
    if !cfg!(target_os = "macos") {
        return MacosVideoReport::unavailable(generation, ProbeReason::UnsupportedPlatform);
    }
    let prepared = match prepare_corpus(&owner.runtime_cache, &corpus, cancelled).await {
        Ok(prepared) => prepared,
        Err(reason) => return MacosVideoReport::unavailable(generation, reason),
    };
    let implementation = match capture_implementation(cancelled).await {
        Ok(implementation) => implementation,
        Err(reason) => return MacosVideoReport::unavailable(generation, reason),
    };
    let mut report = MacosVideoReport {
        generation,
        identity: Some(implementation.identity.clone()),
        sdr_scale: GraphObservation::pending(),
        hdr10_metal: GraphObservation::pending(),
        graphs: BTreeMap::new(),
    };
    // Real identity is now known: new plans can retain pending per-class
    // observations without inventing an implementation fingerprint.
    owner.publish(report.clone());
    let deadline = tokio::time::Instant::now() + ALL_GRAPHS_BUDGET;
    let mut sdr_result = Ok(());
    for fixture in &corpus.fixtures {
        let pipeline = if fixture.class != "sdr" {
            plurx_core::transcode::Pipeline::VtToneMapMetal
        } else {
            plurx_core::transcode::Pipeline::VtScaleSdr
        };
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(GRAPH_BUDGET);
        let result = if remaining.is_zero() {
            Err(ProbeReason::GraphTimedOut)
        } else {
            run_smoke(
                &prepared,
                fixture,
                pipeline,
                SmokeOperation::Plain,
                &implementation,
                cancelled,
                remaining,
            )
            .await
        };
        if fixture.class == "hdr10" {
            report.hdr10_metal = GraphObservation::from_result(result);
        } else if sdr_result.is_ok() {
            sdr_result = result;
        }
    }
    report.sdr_scale = GraphObservation::from_result(sdr_result);
    let extensions = match extension_corpus() {
        Ok(corpus) => corpus,
        Err(reason) => return MacosVideoReport::unavailable(generation, reason),
    };
    let mut work = Vec::new();
    if let Err(reason) = live::add_work(&corpus, &mut work) {
        return MacosVideoReport::unavailable(generation, reason);
    }
    if let Err(reason) = p5::add_work(&corpus, &mut work) {
        return MacosVideoReport::unavailable(generation, reason);
    }
    for fixture in &corpus.fixtures {
        let graph = if fixture.class == "hdr10" {
            MacosProcessingGraph::Hdr10TextBurn
        } else {
            MacosProcessingGraph::SdrTextBurn
        };
        work.push((fixture.clone(), SmokeOperation::Text, graph));
    }
    for fixture in &corpus.fixtures {
        let hdr = fixture.class == "hdr10";
        for (operation, graph) in [
            (
                SmokeOperation::HevcNative,
                if hdr {
                    MacosProcessingGraph::HevcHdr10
                } else {
                    MacosProcessingGraph::HevcSdr
                },
            ),
            (
                SmokeOperation::HevcHostSoftware,
                if hdr {
                    MacosProcessingGraph::HevcHdr10Host
                } else {
                    MacosProcessingGraph::HevcSdrHost
                },
            ),
            (
                SmokeOperation::HevcHostVideoToolbox,
                if hdr {
                    MacosProcessingGraph::HevcHdr10Host
                } else {
                    MacosProcessingGraph::HevcSdrHost
                },
            ),
        ] {
            work.push((fixture.clone(), operation, graph));
        }
    }
    for extension in extensions.fixtures {
        let base = &corpus.fixtures[if extension.source_class != "sdr" {
            2
        } else if extension.id == "pgs_sdr10" {
            1
        } else {
            0
        }];
        let mut fixture = base.clone();
        fixture.class = extension.source_class.clone();
        fixture.sha256 = extension.sha256;
        fixture.byte_length = extension.byte_length;
        fixture.output_expectations = vec![extension.output_expectation];
        if extension.operation == "plain" {
            work.push((
                fixture,
                SmokeOperation::Plain,
                MacosProcessingGraph::HlgMetal,
            ));
        } else if extension.operation == "bitmap_burn" {
            let graph = if extension.source_class == "hdr10" {
                MacosProcessingGraph::Hdr10BitmapBurn
            } else {
                MacosProcessingGraph::SdrBitmapBurn
            };
            work.push((fixture, SmokeOperation::Bitmap, graph));
        }
    }
    for (fixture, operation, graph) in work {
        let pipeline = if let SmokeOperation::StrictP5(pipeline, _) = operation {
            pipeline
        } else if fixture.class == "hdr10"
            && matches!(
                operation,
                SmokeOperation::HevcNative
                    | SmokeOperation::HevcHostSoftware
                    | SmokeOperation::HevcHostVideoToolbox
            )
        {
            plurx_core::transcode::Pipeline::VtScaleHdr10
        } else if fixture.class != "sdr" {
            plurx_core::transcode::Pipeline::VtToneMapMetal
        } else {
            plurx_core::transcode::Pipeline::VtScaleSdr
        };
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(GRAPH_BUDGET);
        let result = if remaining.is_zero() {
            Err(ProbeReason::GraphTimedOut)
        } else {
            run_smoke(
                &prepared,
                &fixture,
                pipeline,
                operation,
                &implementation,
                cancelled,
                remaining,
            )
            .await
        };
        // One failed source invalidates this complete graph class; later
        // successful bit-depth/parity controls cannot erase that failure.
        let observation = report
            .graphs
            .entry(graph)
            .or_insert_with(|| GraphObservation::from_result(Ok(())));
        if observation.availability == MacosProcessingAvailability::Available {
            *observation = GraphObservation::from_result(result);
        }
    }

    let current = bounded_graph_io(deadline, cancelled, async {
        Ok(implementation_is_current(&implementation).await)
    })
    .await;
    let failure = match current {
        Ok(true) => None,
        Ok(false) => Some(ProbeReason::ImplementationChanged),
        Err(reason) => Some(reason),
    };
    if let Some(reason) = failure {
        report.sdr_scale = GraphObservation::from_result(Err(reason));
        report.hdr10_metal = GraphObservation::from_result(Err(reason));
        for observation in report.graphs.values_mut() {
            *observation = GraphObservation::from_result(Err(reason));
        }
    }
    report
}

fn held_file_argument(
    command: &mut tokio::process::Command,
    file: &std::fs::File,
    path: &std::path::Path,
) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        let _ = path;
        crate::ffmpeg::inherit_file_descriptors(command, &[(file, 3)]);
        "/dev/fd/3".into()
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        path.as_os_str().to_owned()
    }
}

async fn verified_source(
    prepared: &PreparedCorpus,
    fixture: &Fixture,
) -> Result<std::fs::File, ProbeReason> {
    verified_content(
        prepared,
        &format!("{}.mp4", fixture.sha256),
        &fixture.sha256,
        fixture.byte_length,
    )
    .await
}

async fn verified_content(
    prepared: &PreparedCorpus,
    name: &str,
    expected_digest: &str,
    byte_length: usize,
) -> Result<std::fs::File, ProbeReason> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = prepared
        .directory
        .open_read_child(name)
        .await
        .map_err(|_| ProbeReason::CacheUnavailable)?;
    let mut bytes = Vec::with_capacity(byte_length);
    (&mut file)
        .take(byte_length as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ProbeReason::CacheUnavailable)?;
    if bytes.len() != byte_length || digest(&bytes) != expected_digest {
        return Err(ProbeReason::CacheUnavailable);
    }
    file.rewind()
        .await
        .map_err(|_| ProbeReason::CacheUnavailable)?;
    Ok(file.into_std().await)
}

async fn bounded_probe_command(
    command: tokio::process::Command,
    deadline: tokio::time::Instant,
    max_bytes: u64,
    cancelled: &CancellationToken,
) -> Result<Vec<u8>, ProbeReason> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(ProbeReason::GraphTimedOut);
    }
    crate::ffmpeg::bounded_command_output_cancellable(
        command,
        remaining,
        max_bytes,
        "Mac compatibility graph",
        Some(cancelled),
        PROBE_WORK,
    )
    .await
    .map(|output| output.stdout)
    .map_err(|_| {
        if cancelled.is_cancelled() {
            ProbeReason::Cancelled
        } else if tokio::time::Instant::now() >= deadline {
            ProbeReason::GraphTimedOut
        } else {
            ProbeReason::GraphFailed
        }
    })
}

// This wrapper is only for filesystem/stat futures. Subprocess futures always
// settle through bounded_probe_command, which retains kill-and-reap ownership.
async fn bounded_graph_io<T>(
    deadline: tokio::time::Instant,
    cancelled: &CancellationToken,
    work: impl std::future::Future<Output = Result<T, ProbeReason>>,
) -> Result<T, ProbeReason> {
    tokio::select! {
        biased;
        () = cancelled.cancelled() => Err(ProbeReason::Cancelled),
        result = tokio::time::timeout_at(deadline, work) => result.unwrap_or(Err(ProbeReason::GraphTimedOut)),
    }
}

// Cancellation/deadline is a request to stop this writer, never permission to
// drop its blocking JoinHandle. In particular it may have passed its final
// cooperative check already. Settle first, then the caller can remove any
// committed output without racing a late rename from a detached writer.
async fn settle_graph_write(
    deadline: tokio::time::Instant,
    cancelled: &CancellationToken,
    write: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<(), ProbeReason> {
    tokio::pin!(write);
    let stopped = tokio::select! {
        biased;
        () = cancelled.cancelled() => ProbeReason::Cancelled,
        () = tokio::time::sleep_until(deadline) => ProbeReason::GraphTimedOut,
        result = &mut write => return result.map_err(|_| ProbeReason::CacheUnavailable),
    };
    let _ = write.await;
    Err(stopped)
}

async fn settle_probe_publication(
    directory: &plurx_core::fs_secure::SecureDirectory,
    name: &str,
    deadline: tokio::time::Instant,
    cancelled: &CancellationToken,
    write: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<(), ProbeReason> {
    let result = settle_graph_write(deadline, cancelled, write).await;
    if result.is_err() {
        // The writer is settled, so even a rename after its last cooperative
        // check cannot race this unlink. A filesystem syscall already in flight
        // cannot be interrupted: settlement may extend past the probe budget.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            directory.unlink_child(name),
        )
        .await;
    }
    result
}

fn observe_burn(raw: &[u8], operation: SmokeOperation) -> Result<(), ProbeReason> {
    let stride = 160 * 90 * 3 / 2;
    if raw.len() != stride * 12 {
        return Err(ProbeReason::OutputContractFailed);
    }
    if operation == SmokeOperation::Bitmap {
        for index in 0..12 {
            let base = index * stride;
            let observed = [
                raw[base + 75 * 160 + 130],
                raw[base + 160 * 90 + 37 * 80 + 65],
                raw[base + 160 * 90 * 5 / 4 + 37 * 80 + 65],
            ];
            // PGS palette conversion is owned by the incumbent decoder. The
            // original half-alpha cue appears only in [0.25,0.75), over black.
            let expected: [u8; 3] = if (3..9).contains(&index) {
                [39, 115, 185]
            } else {
                [16, 128, 128]
            };
            if observed
                .into_iter()
                .zip(expected)
                .any(|(actual, expected)| actual.abs_diff(expected) > 10)
            {
                return Err(ProbeReason::OutputContractFailed);
            }
        }
    } else if operation == SmokeOperation::Text {
        // The cue overlays bright colored artwork: its brightest sample need
        // not exceed the background after lossy encoding. Compare the stationary
        // cue region with its own subtitle-free frame instead. Rows below this
        // region contain the fixture's independently moving white marker.
        let change = |index: usize| {
            (70..78)
                .flat_map(|y| (60..100).map(move |x| (y, x)))
                .map(|(y, x)| {
                    f64::from(raw[index * stride + y * 160 + x].abs_diff(raw[y * 160 + x]))
                })
                .sum::<f64>()
                / 320.0
        };
        if [5, 6, 7].into_iter().any(|index| change(index) < 8.0)
            || [1, 2, 10, 11].into_iter().any(|index| change(index) > 3.0)
        {
            return Err(ProbeReason::OutputContractFailed);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokeOperation {
    Plain,
    Text,
    Bitmap,
    BwdifFrame,
    BwdifField,
    HevcNative,
    HevcHostSoftware,
    HevcHostVideoToolbox,
    LiveUpload(MacosProcessingGraph),
    StrictP5(
        plurx_core::transcode::Pipeline,
        plurx_core::transcode::Encoder,
    ),
}

#[allow(clippy::too_many_arguments)]
async fn run_smoke(
    prepared: &PreparedCorpus,
    fixture: &Fixture,
    pipeline: plurx_core::transcode::Pipeline,
    operation: SmokeOperation,
    implementation: &Implementation,
    cancelled: &CancellationToken,
    budget: std::time::Duration,
) -> Result<(), ProbeReason> {
    use plurx_core::transcode::{EffectiveRateControl, Encoder};
    let deadline = tokio::time::Instant::now() + budget;
    if cancelled.is_cancelled() {
        return Err(ProbeReason::Cancelled);
    }
    if let SmokeOperation::StrictP5(pipeline, encoder) = operation {
        return p5::run(
            prepared,
            fixture,
            pipeline,
            encoder,
            implementation,
            cancelled,
            deadline,
        )
        .await;
    }
    let live_graph = if let SmokeOperation::LiveUpload(graph) = operation {
        Some(graph)
    } else {
        None
    };
    let field = operation == SmokeOperation::BwdifField
        || live_graph == Some(MacosProcessingGraph::LiveSdrUploadBwdifField);
    let hdr = fixture.class != "sdr";
    let hevc = matches!(
        operation,
        SmokeOperation::HevcNative
            | SmokeOperation::HevcHostSoftware
            | SmokeOperation::HevcHostVideoToolbox
    );
    let host = matches!(
        operation,
        SmokeOperation::HevcHostSoftware | SmokeOperation::HevcHostVideoToolbox
    );
    let mut required: Vec<&str> = if hdr {
        vec!["scale_vt", "tonemap_videotoolbox"]
    } else {
        vec!["scale_vt"]
    };
    if hevc {
        required = if host {
            vec!["scale", "format"]
        } else {
            vec!["scale_vt"]
        };
    }
    match operation {
        SmokeOperation::Text => required.extend(["hwdownload", "subtitles"]),
        SmokeOperation::Bitmap => required.extend(["hwdownload", "overlay", "scale"]),
        SmokeOperation::BwdifFrame | SmokeOperation::BwdifField => {
            required.push("bwdif_videotoolbox")
        }
        SmokeOperation::LiveUpload(graph) => {
            required.extend(["format", "hwupload"]);
            if graph != MacosProcessingGraph::LiveSdrUploadScale {
                required.push("bwdif_videotoolbox");
            }
        }
        SmokeOperation::StrictP5(_, _)
        | SmokeOperation::Plain
        | SmokeOperation::HevcNative
        | SmokeOperation::HevcHostSoftware
        | SmokeOperation::HevcHostVideoToolbox => {}
    }
    if !crate::pipeprobe::declares_filters(&implementation.filters, &required) {
        return Err(ProbeReason::MissingFilter);
    }
    if !bounded_graph_io(deadline, cancelled, async {
        Ok(implementation_is_current(implementation).await)
    })
    .await?
    {
        return Err(ProbeReason::ImplementationChanged);
    }
    let mut contract = fixture
        .output_expectations
        .iter()
        .find(|e| {
            e.graph_id
                == if fixture.class == "hlg" {
                    "vt_tonemap_hlg_metal"
                } else if hdr {
                    "vt_tonemap_metal"
                } else {
                    "vt_scale_sdr"
                }
        })
        .ok_or(ProbeReason::InvalidEmbeddedCorpus)?
        .clone();
    if hevc {
        contract.expected.codec_name = "hevc".to_owned();
        contract.expected.profile = Some(if hdr { "Main 10" } else { "Main" }.to_owned());
        contract.expected.pix_fmt = Some(if hdr { "yuv420p10le" } else { "yuv420p" }.to_owned());
        contract.expected.codec_tag_string = Some("hvc1".to_owned());
        if hdr {
            contract.expected.color_primaries = fixture.expected.color_primaries.clone();
            contract.expected.color_transfer = fixture.expected.color_transfer.clone();
            contract.expected.color_space = fixture.expected.color_space.clone();
            contract.forbidden_side_data.retain(|name| {
                !matches!(
                    name.as_str(),
                    "Mastering display metadata" | "Content light level metadata"
                )
            });
            contract.pixels.expected_y = Some(
                fixture.input_pixels["gray_patches"]
                    .as_array()
                    .ok_or(ProbeReason::InvalidEmbeddedCorpus)?
                    .iter()
                    .map(|patch| {
                        patch["y_code"]
                            .as_f64()
                            .map(|y| y / 4.0)
                            .ok_or(ProbeReason::InvalidEmbeddedCorpus)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            contract.pixels.absolute_y_tolerance = Some(6.0);
        }
    }
    let mut source =
        bounded_graph_io(deadline, cancelled, verified_source(prepared, fixture)).await?;
    let source_metadata = if hevc && hdr {
        let mut probe = tokio::process::Command::new(&implementation.ffprobe.path);
        let argument = held_file_argument(
            &mut probe,
            &source,
            &prepared.path.join(format!("{}.mp4", fixture.sha256)),
        );
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_streams",
                "-show_frames",
                "-of",
                "json",
            ])
            .arg(argument);
        let document: Value = serde_json::from_slice(
            &bounded_probe_command(probe, deadline, 512 * 1024, cancelled).await?,
        )
        .map_err(|_| ProbeReason::OutputContractFailed)?;
        let sides = document["frames"][0]["side_data_list"]
            .as_array()
            .ok_or(ProbeReason::OutputContractFailed)?;
        let metadata: Vec<Value> = sides
            .iter()
            .filter(|side| {
                matches!(
                    side["side_data_type"].as_str(),
                    Some("Mastering display metadata" | "Content light level metadata")
                )
            })
            .cloned()
            .collect();
        if metadata.len() != 2 {
            return Err(ProbeReason::OutputContractFailed);
        }
        Some(metadata)
    } else {
        None
    };
    // A descriptor inherited by ffprobe may share the open-file offset.
    // Restore the held source before constructing the encoder child.
    std::io::Seek::rewind(&mut source).map_err(|_| ProbeReason::CacheUnavailable)?;
    let source_path = prepared.path.join(format!("{}.mp4", fixture.sha256));
    let mut encode = tokio::process::Command::new(&implementation.ffmpeg.path);
    let ass = if operation == SmokeOperation::Text {
        Some(
            bounded_graph_io(
                deadline,
                cancelled,
                verified_content(prepared, "burn.ass", &digest(BURN_ASS), BURN_ASS.len()),
            )
            .await?,
        )
    } else {
        None
    };
    #[cfg(unix)]
    let source_arg = {
        let mut files = vec![(&source, 3)];
        if let Some(ass) = &ass {
            files.push((ass, 4));
        }
        crate::ffmpeg::inherit_file_descriptors(&mut encode, &files);
        std::ffi::OsString::from("/dev/fd/3")
    };
    #[cfg(not(unix))]
    let source_arg = source_path.as_os_str().to_owned();
    #[cfg(unix)]
    let _ = source_path;
    encode.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    if let Some(graph) = live_graph {
        encode.args(
            graph
                .live_upload_init_args()
                .ok_or(ProbeReason::GraphFailed)?,
        );
        encode.args(["-hwaccel", "none"]);
    } else if operation == SmokeOperation::HevcHostSoftware {
        encode.args(["-hwaccel", "none"]);
    } else if operation == SmokeOperation::HevcHostVideoToolbox {
        encode.args(["-hwaccel", "videotoolbox"]);
    } else {
        encode.args(pipeline.decode_args());
    }
    encode.arg("-i").arg(source_arg).args(["-an", "-sn", "-dn"]);
    if operation != SmokeOperation::Bitmap {
        encode.args(["-map", "0:v:0"]);
    }
    let mut filter = pipeline
        .filters(Some(160), 90, hdr.then_some(fixture.class.as_str()))
        .ok_or(ProbeReason::GraphFailed)?;
    if host {
        filter = format!(
            "scale=160:90,format={}",
            if hdr { "yuv420p10le" } else { "yuv420p" }
        );
    }
    match operation {
        SmokeOperation::LiveUpload(graph) => {
            filter = graph
                .live_upload_filter(160, 90)
                .ok_or(ProbeReason::GraphFailed)?;
            encode.args(["-vf", &filter]);
        }
        SmokeOperation::Text => {
            filter.push_str(",hwdownload,format=nv12,subtitles='/dev/fd/4'");
            encode.args(["-vf", &filter]);
        }
        SmokeOperation::Bitmap => {
            let composite = format!("[0:v]{filter},hwdownload,format=nv12[vburn];[0:s:0]scale=160:90[sburn];[vburn][sburn]overlay=eof_action=pass[o]");
            // Replace the simple video map with the existing bitmap compositor's output.
            encode.args(["-filter_complex", &composite, "-map", "[o]"]);
        }
        SmokeOperation::BwdifFrame | SmokeOperation::BwdifField => {
            let mode = if operation == SmokeOperation::BwdifFrame {
                "send_frame"
            } else {
                "send_field"
            };
            filter.insert_str(
                0,
                &format!("bwdif_videotoolbox=mode={mode}:parity=auto:deint=interlaced,"),
            );
            encode.args(["-vf", &filter]);
        }
        SmokeOperation::StrictP5(_, _)
        | SmokeOperation::Plain
        | SmokeOperation::HevcNative
        | SmokeOperation::HevcHostSoftware
        | SmokeOperation::HevcHostVideoToolbox => {
            encode.args(["-vf", &filter]);
        }
    }
    encode.args(Encoder::VideoToolbox.encode_args_for_codec(
        if hevc {
            plurx_core::transcode::VideoCodec::Hevc
        } else {
            plurx_core::transcode::VideoCodec::H264
        },
        if hevc && hdr {
            plurx_core::transcode::OutputGrade::Hdr10
        } else {
            plurx_core::transcode::OutputGrade::Sdr
        },
        500,
        EffectiveRateControl::Vbr,
        false,
        None,
    ));
    encode.args([
        "-allow_sw",
        "0",
        "-bf",
        "0",
        "-frames:v",
        if field { "24" } else { "12" },
        "-color_primaries",
        contract.expected.color_primaries.as_str(),
        "-color_trc",
        contract.expected.color_transfer.as_str(),
        "-colorspace",
        contract.expected.color_space.as_str(),
        "-color_range",
        "tv",
        "-f",
        "mp4",
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof",
        "pipe:1",
    ]);
    let encoded = bounded_probe_command(encode, deadline, CORPUS_BUDGET as u64, cancelled).await?;
    if encoded.is_empty() {
        return Err(ProbeReason::OutputContractFailed);
    }
    let name = format!("probe-{}.mp4", uuid::Uuid::new_v4().simple());
    let token = cancelled.clone();
    settle_probe_publication(
        &prepared.directory,
        &name,
        deadline,
        cancelled,
        prepared
            .directory
            .atomic_write_child_cooperative(&name, &encoded, move || {
                !token.is_cancelled() && tokio::time::Instant::now() < deadline
            }),
    )
    .await?;
    let observe = async {
        let output_path = prepared.path.join(&name);
        let output = bounded_graph_io(deadline, cancelled, async {
            Ok(prepared
                .directory
                .open_read_child(&name)
                .await
                .map_err(|_| ProbeReason::CacheUnavailable)?
                .into_std()
                .await)
        })
        .await?;
        let mut probe = tokio::process::Command::new(&implementation.ffprobe.path);
        let argument = held_file_argument(&mut probe, &output, &output_path);
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_streams",
                "-show_frames",
                "-of",
                "json",
            ])
            .arg(argument);
        let document: Value = serde_json::from_slice(
            &bounded_probe_command(probe, deadline, 512 * 1024, cancelled).await?,
        )
        .map_err(|_| ProbeReason::OutputContractFailed)?;
        let output = bounded_graph_io(deadline, cancelled, async {
            Ok(prepared
                .directory
                .open_read_child(&name)
                .await
                .map_err(|_| ProbeReason::CacheUnavailable)?
                .into_std()
                .await)
        })
        .await?;
        let mut decode = tokio::process::Command::new(&implementation.ffmpeg.path);
        let argument = held_file_argument(&mut decode, &output, &output_path);
        decode
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(argument)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-frames:v",
                "25",
                "-fps_mode",
                "passthrough",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ]);
        let raw = bounded_probe_command(decode, deadline, 160 * 90 * 3 / 2 * 25, cancelled).await?;
        observe_output(&document, &raw, &contract)?;
        if matches!(
            operation,
            SmokeOperation::BwdifFrame | SmokeOperation::BwdifField
        ) || live_graph.is_some_and(|graph| graph != MacosProcessingGraph::LiveSdrUploadScale)
        {
            live::observe_motion(&raw, field)?;
        }

        if let Some(expected) = &source_metadata {
            let frames = document["frames"]
                .as_array()
                .ok_or(ProbeReason::OutputContractFailed)?;
            for frame in std::iter::once(&document["streams"][0]).chain(frames.iter()) {
                let sides = frame["side_data_list"]
                    .as_array()
                    .ok_or(ProbeReason::OutputContractFailed)?;
                if expected.iter().any(|metadata| !sides.contains(metadata)) {
                    return Err(ProbeReason::OutputContractFailed);
                }
            }
        }

        if fixture.class == "hlg" {
            // Independent HLG203-nit reference-white patch under the pinned
            // BT.2390/ITP treatment. PQ interpretation of these scene-relative
            // values is not admitted by merely monotone gray output.
            let stride = 160 * 90 * 3 / 2;
            if (0..12).any(|index| raw[index * stride + 30 * 160 + 90].abs_diff(156) > 12) {
                return Err(ProbeReason::OutputContractFailed);
            }
        }
        if matches!(operation, SmokeOperation::Text | SmokeOperation::Bitmap) {
            observe_burn(&raw, operation)?;
        }
        Ok(())
    }
    .await;
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        prepared.directory.unlink_child(&name),
    )
    .await;
    if !matches!(cleanup, Ok(Ok(()))) {
        return Err(ProbeReason::CacheUnavailable);
    }
    observe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_pgs_observation_rejects_opaque_missing_and_stale_cues() {
        let stride = 160 * 90 * 3 / 2;
        let mut raw = vec![128; stride * 12];
        for index in 0..12 {
            let base = index * stride;
            raw[base..base + 160 * 90].fill(16);
            if (3..9).contains(&index) {
                raw[base + 75 * 160 + 130] = 39;
                raw[base + 160 * 90 + 37 * 80 + 65] = 115;
                raw[base + 160 * 90 * 5 / 4 + 37 * 80 + 65] = 185;
            }
        }
        assert_eq!(observe_burn(&raw, SmokeOperation::Bitmap), Ok(()));
        for (frame, y, cb, cr) in [(3, 16, 128, 128), (3, 62, 102, 240), (10, 39, 115, 185)] {
            let mut changed = raw.clone();
            let base = frame * stride;
            changed[base + 75 * 160 + 130] = y;
            changed[base + 160 * 90 + 37 * 80 + 65] = cb;
            changed[base + 160 * 90 * 5 / 4 + 37 * 80 + 65] = cr;
            assert_eq!(
                observe_burn(&changed, SmokeOperation::Bitmap),
                Err(ProbeReason::OutputContractFailed)
            );
        }
    }

    #[test]
    fn text_cue_observation_detects_change_over_bright_background_and_rejects_missing_or_stale() {
        let stride = 160 * 90 * 3 / 2;
        let mut raw = vec![231; stride * 12];
        // A white glyph's peak can remain within four codes of this background;
        // its dark outline still proves the scheduled composite was rendered.
        for index in [5, 6, 7] {
            for y in 70..78 {
                for x in 60..80 {
                    raw[index * stride + y * 160 + x] = 180;
                }
            }
        }
        assert_eq!(observe_burn(&raw, SmokeOperation::Text), Ok(()));
        assert_eq!(
            observe_burn(&vec![231; stride * 12], SmokeOperation::Text),
            Err(ProbeReason::OutputContractFailed)
        );
        for index in [1, 2, 10, 11] {
            let mut stale = raw.clone();
            for y in 70..78 {
                for x in 60..80 {
                    stale[index * stride + y * 160 + x] = 180;
                }
            }
            assert_eq!(
                observe_burn(&stale, SmokeOperation::Text),
                Err(ProbeReason::OutputContractFailed)
            );
        }
        let mut noise = vec![231; stride * 12];
        for index in [5, 6, 7] {
            noise[index * stride + 72 * 160 + 80] = 0;
        }
        assert_eq!(
            observe_burn(&noise, SmokeOperation::Text),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn macos_extension_corpus_pins_original_media_and_complete_classes() {
        let corpus = extension_corpus().expect("shipped extension media integrity");
        assert_eq!(corpus.fixtures.len(), 4);
        assert_eq!(
            corpus.fixtures.last().expect("HLG input").source_class,
            "hlg"
        );
        assert!(corpus
            .fixtures
            .iter()
            .any(|fixture| fixture.id == "pgs_sdr10"));
    }

    fn temporary_root() -> tempfile::TempDir {
        let root = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
        tempfile::tempdir_in(root).expect("owned temporary fixture")
    }

    fn corpus() -> Corpus {
        embedded_corpus(MANIFEST, SDR8, SDR10, HDR10).expect("shipped smoke corpus")
    }

    fn contract(corpus: &Corpus, hdr: bool) -> &OutputExpectation {
        let fixture = &corpus.fixtures[if hdr { 2 } else { 0 }];
        fixture
            .output_expectations
            .iter()
            .find(|expectation| {
                expectation.graph_id
                    == if hdr {
                        "vt_tonemap_metal"
                    } else {
                        "vt_scale_sdr"
                    }
            })
            .expect("selected graph has a smoke output contract")
    }

    fn decoded(contract: &OutputExpectation) -> (Value, Vec<u8>) {
        let e = &contract.expected;
        let stream = json!({
            "codec_name": e.codec_name, "width": e.width, "height": e.height,
            "avg_frame_rate": e.avg_frame_rate, "sample_aspect_ratio": e.sample_aspect_ratio,
            "field_order": e.field_order, "color_primaries": e.color_primaries,
            "color_transfer": e.color_transfer, "color_space": e.color_space,
            "color_range": e.color_range,
        });
        let frames: Vec<_> = (0..12)
            .map(|index| {
                let mut frame = stream.clone();
                frame["interlaced_frame"] = json!(0);
                frame["best_effort_timestamp_time"] = json!(format!("{:.6}", index as f64 / 12.0));
                frame
            })
            .collect();
        let gray = contract
            .pixels
            .expected_y
            .clone()
            .unwrap_or_else(|| vec![16.0, 20.0, 28.0, 80.0, 120.0, 160.0, 200.0, 200.0]);
        let mut one = Vec::with_capacity(160 * 90 * 3 / 2);
        for _ in 0..90 {
            for x in 0..160 {
                one.push(gray[x / 20] as u8);
            }
        }
        one.extend(std::iter::repeat_n(128, 160 * 90 / 2));
        (
            json!({"streams": [stream], "frames": frames}),
            one.repeat(12),
        )
    }

    #[test]
    fn rust_arm_architecture_maps_to_the_observed_mac_identity_name() {
        assert_eq!(processing_architecture("aarch64"), Ok("arm64"));
        assert_eq!(processing_architecture("x86_64"), Ok("x86_64"));
        assert_eq!(
            processing_architecture("unknown"),
            Err(ProbeReason::IdentityUnavailable)
        );
    }

    #[test]
    fn readiness_never_overrides_the_callers_explicit_processing_choice() {
        let mut report = MacosVideoReport::unavailable(9, ProbeReason::GraphFailed);
        report.identity = Some(
            MacosProcessingIdentity::new(
                "1".repeat(64),
                "2".repeat(64),
                "3".repeat(64),
                "4".repeat(64),
                "test-build".to_owned(),
                "arm64".to_owned(),
                "test-model".to_owned(),
            )
            .expect("valid test implementation identity"),
        );
        assert!(report
            .context(true)
            .expect("identity retains enabled context")
            .enabled());
        assert!(!report
            .context(false)
            .expect("identity retains disabled context")
            .enabled());
        assert_eq!(
            report
                .context(true)
                .expect("identity retains unavailable context")
                .sdr_scale(),
            MacosProcessingAvailability::Unavailable
        );
        let snapshot = Arc::new(report.clone());
        report.generation = 10;
        assert_eq!(snapshot.generation, 9);
    }

    #[tokio::test(start_paused = true)]
    async fn graph_filesystem_budget_times_out_without_a_child_or_retry() {
        let token = CancellationToken::new();
        let result = bounded_graph_io(tokio::time::Instant::now() + GRAPH_BUDGET, &token, async {
            tokio::time::sleep(GRAPH_BUDGET * 2).await;
            Ok(())
        })
        .await;
        assert_eq!(result, Err(ProbeReason::GraphTimedOut));
    }

    #[test]
    fn embedded_corpus_has_real_8bit_10bit_and_pq_inputs_without_generation() {
        let corpus = corpus();
        assert_eq!(
            corpus
                .fixtures
                .iter()
                .map(|fixture| fixture.id.as_str())
                .collect::<Vec<_>>(),
            ["sdr8", "sdr10", "hdr10"]
        );
        assert_eq!(corpus.fixtures[2].expected.color_transfer, "smpte2084");
        assert_eq!(corpus.fixtures[1].expected.pix_fmt, "yuv420p10le");
        assert!(MANIFEST.len() + SDR8.len() + SDR10.len() + HDR10.len() <= CORPUS_BUDGET);
    }

    #[test]
    fn corrupt_missing_or_relabelled_embedded_inputs_are_packaging_failures() {
        let mut corrupt = SDR8.to_vec();
        let end = corrupt.len() - 1;
        corrupt[end] ^= 1;
        assert!(matches!(
            embedded_corpus(MANIFEST, &corrupt, SDR10, HDR10),
            Err(ProbeReason::InvalidEmbeddedCorpus)
        ));
        assert!(matches!(
            embedded_corpus(MANIFEST, SDR8, &[], HDR10),
            Err(ProbeReason::InvalidEmbeddedCorpus)
        ));
        let mut manifest: Value =
            serde_json::from_slice(MANIFEST).expect("embedded manifest is JSON");
        manifest["fixtures"][0]["expected"]["color_transfer"] = json!("smpte2084");
        assert!(matches!(
            embedded_corpus(
                &serde_json::to_vec(&manifest).expect("serialize mutated manifest"),
                SDR8,
                SDR10,
                HDR10
            ),
            Err(ProbeReason::InvalidEmbeddedCorpus)
        ));
    }

    #[test]
    fn manifest_cannot_escape_cache_or_relax_output_tolerances() {
        for (field, value) in [
            ("path", json!("../outside.mp4")),
            ("available", json!(false)),
        ] {
            let mut manifest: Value =
                serde_json::from_slice(MANIFEST).expect("embedded manifest is JSON");
            manifest["fixtures"][0][field] = value;
            assert!(matches!(
                embedded_corpus(
                    &serde_json::to_vec(&manifest).expect("serialize mutated manifest"),
                    SDR8,
                    SDR10,
                    HDR10
                ),
                Err(ProbeReason::InvalidEmbeddedCorpus)
            ));
        }
        let mut manifest: Value =
            serde_json::from_slice(MANIFEST).expect("embedded manifest is JSON");
        manifest["fixtures"][0]["output_expectations"][0]["pixels"]["absolute_y_tolerance"] =
            json!(255);
        assert!(matches!(
            embedded_corpus(
                &serde_json::to_vec(&manifest).expect("serialize mutated manifest"),
                SDR8,
                SDR10,
                HDR10
            ),
            Err(ProbeReason::InvalidEmbeddedCorpus)
        ));
    }

    #[test]
    fn decoded_outputs_use_tolerant_pixels_instead_of_encoded_hashes() {
        let corpus = corpus();
        let contract = contract(&corpus, false);
        let (document, mut raw) = decoded(contract);
        raw[30 * 160 + 10] += 8;
        assert_eq!(observe_output(&document, &raw, contract), Ok(()));
        raw[30 * 160 + 10] = 255;
        // A complete patch changed far beyond the compression tolerance.
        for y in 28..33 {
            for x in 8..13 {
                raw[y * 160 + x] = 255;
            }
        }
        assert_eq!(
            observe_output(&document, &raw, contract),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn decoded_output_rejects_missing_frames_repeated_pts_and_nan() {
        let corpus = corpus();
        let contract = contract(&corpus, false);
        let (original, raw) = decoded(contract);
        for pts in ["0.000000", "NaN"] {
            let mut document = original.clone();
            document["frames"][1]["best_effort_timestamp_time"] = json!(pts);
            assert_eq!(
                observe_output(&document, &raw, contract),
                Err(ProbeReason::OutputContractFailed)
            );
        }
        let mut document = original;
        document["frames"]
            .as_array_mut()
            .expect("decoded fixture has a frame array")
            .pop();
        assert_eq!(
            observe_output(&document, &raw, contract),
            Err(ProbeReason::OutputContractFailed)
        );
        let (document, raw) = decoded(contract);
        assert_eq!(
            observe_output(&document, &raw[..raw.len() - 1], contract),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn decoded_output_rejects_stale_hdr_signaling_on_any_frame() {
        let corpus = corpus();
        let contract = contract(&corpus, true);
        let (original, raw) = decoded(contract);
        let mut document = original.clone();
        document["frames"][7]["color_transfer"] = json!("smpte2084");
        assert_eq!(
            observe_output(&document, &raw, contract),
            Err(ProbeReason::OutputContractFailed)
        );
        for side in [
            "Mastering display metadata",
            "Content light level metadata",
            "Dolby Vision RPU Data",
        ] {
            let mut document = original.clone();
            document["frames"][7]["side_data_list"] = json!([{"side_data_type": side}]);
            assert_eq!(
                observe_output(&document, &raw, contract),
                Err(ProbeReason::OutputContractFailed)
            );
        }
    }

    #[test]
    fn hdr_output_checks_ordering_and_contrast_without_a_named_curve_score() {
        let corpus = corpus();
        let contract = contract(&corpus, true);
        let (document, raw) = decoded(contract);
        assert_eq!(observe_output(&document, &raw, contract), Ok(()));
        let mut flat = vec![16; 160 * 90];
        flat.extend(std::iter::repeat_n(128, 160 * 90 / 2));
        assert_eq!(
            observe_output(&document, &flat.repeat(12), contract),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[tokio::test]
    async fn overlapping_reprobe_observes_current_immutable_generation() {
        let probe = MacosVideoProbe::new(PathBuf::from("unused-runtime-cache"));
        let snapshot = probe.snapshot();
        let _serial = probe.serial.lock().await;
        let observed = probe.reprobe(&CancellationToken::new()).await;
        assert!(Arc::ptr_eq(&snapshot, &observed));
        assert_eq!(observed.generation, 0);
    }

    #[tokio::test]
    async fn cancelled_reprobe_does_not_require_files_or_tools() {
        let probe = MacosVideoProbe::new(PathBuf::from("unused-runtime-cache"));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let report = probe.reprobe(&cancelled).await;
        assert_eq!(report.generation, 1);
        assert_eq!(report.sdr_scale.reason, ProbeReason::Cancelled);
        assert_eq!(report.hdr10_metal.reason, ProbeReason::Cancelled);
        assert!(report.context(true).is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn non_mac_reports_unavailable_without_altering_saved_preference() {
        let probe = MacosVideoProbe::new(PathBuf::from("unused-runtime-cache"));
        let report = probe.reprobe(&CancellationToken::new()).await;
        assert_eq!(report.sdr_scale.reason, ProbeReason::UnsupportedPlatform);
        assert!(report.context(true).is_none());
        assert!(report.context(false).is_none());
        assert_eq!(report.diagnostics()["qualification"], "external_advisory");
        assert_eq!(report.diagnostics()["dependency_inventory_tool"], "otool");
        assert_eq!(
            report.diagnostics()["supported_dependency_scope"],
            "apple_system_only"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clean_offline_cache_uses_embedded_bytes_and_recovers_corruption() {
        let root = temporary_root();
        let corpus = corpus();
        let token = CancellationToken::new();
        let prepared = prepare_corpus(root.path(), &corpus, &token)
            .await
            .expect("prepare clean private corpus");
        assert_eq!(
            prepared
                .directory
                .child_names(8)
                .await
                .expect("list prepared corpus files")
                .len(),
            4
        );
        let name = format!("{}.mp4", corpus.fixtures[0].sha256);
        prepared
            .directory
            .atomic_write_child(&name, b"corrupt")
            .await
            .expect("replace owned fixture with corrupt test bytes");
        let repaired = prepare_corpus(root.path(), &corpus, &token)
            .await
            .expect("repair cached corpus from embedded bytes");
        assert_eq!(
            repaired
                .directory
                .read_bounded_child(&name, SDR8.len() as u64)
                .await
                .expect("read repaired SDR fixture"),
            SDR8
        );
        assert!(verified_source(&repaired, &corpus.fixtures[0])
            .await
            .is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_cached_fixture_is_replaced_without_writing_its_target() {
        let root = temporary_root();
        let corpus = corpus();
        let token = CancellationToken::new();
        let prepared = prepare_corpus(root.path(), &corpus, &token)
            .await
            .expect("prepare clean private corpus");
        let name = format!("{}.mp4", corpus.fixtures[0].sha256);
        prepared
            .directory
            .unlink_child(&name)
            .await
            .expect("remove owned fixture before symlink injection");
        let outside = root.path().join("unrelated");
        tokio::fs::write(&outside, b"leave alone")
            .await
            .expect("write unrelated target sentinel");
        std::os::unix::fs::symlink(&outside, prepared.path.join(&name))
            .expect("inject symlinked cached fixture");
        let repaired = prepare_corpus(root.path(), &corpus, &token)
            .await
            .expect("repair cached corpus from embedded bytes");
        assert_eq!(
            tokio::fs::read(outside)
                .await
                .expect("read unrelated target sentinel"),
            b"leave alone"
        );
        assert!(verified_source(&repaired, &corpus.fixtures[0])
            .await
            .is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cache_permission_failure_is_explicit_and_reprobe_repairs_after_operator_fix() {
        use std::os::unix::fs::PermissionsExt;
        let root = temporary_root();
        let corpus = corpus();
        let token = CancellationToken::new();
        prepare_corpus(root.path(), &corpus, &token)
            .await
            .expect("prepare cache before permission refusal");
        let owner = root.path().join("macos-processing");
        tokio::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("inject non-private cache permissions");
        assert!(matches!(
            prepare_corpus(root.path(), &corpus, &token).await,
            Err(ProbeReason::CacheUnavailable)
        ));
        tokio::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700))
            .await
            .expect("restore private cache permissions");
        assert!(prepare_corpus(root.path(), &corpus, &token).await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_preparation_publishes_no_fixture_directory() {
        let root = temporary_root();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            prepare_corpus(root.path(), &corpus(), &token).await,
            Err(ProbeReason::Cancelled)
        ));
        assert!(!root.path().join("macos-processing").exists());
    }

    #[test]
    fn admitted_dependency_identity_covers_both_programs_and_os_build() {
        let report = b"/candidate:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.2.3)\n";
        let admitted = apple_dependency_report(report).expect("Apple dependency admitted");
        let original = apple_dependencies_digest("OS-build-a", &admitted, &admitted);
        let changed = admitted.replace("1.2.3", "1.2.4");
        assert_ne!(
            original,
            apple_dependencies_digest("OS-build-a", &changed, &admitted)
        );
        assert_ne!(
            original,
            apple_dependencies_digest("OS-build-a", &admitted, &changed)
        );
        assert_ne!(
            original,
            apple_dependencies_digest("OS-build-b", &admitted, &admitted)
        );
        for install_name in [
            "@rpath/libffprobe-only.dylib",
            "/opt/local/lib/library.dylib",
            "/usr/lib/../local/library.dylib",
            "/System/Library/./Frameworks/A",
            "/usr/lib//A",
        ] {
            let report = format!("/candidate:\n\t{install_name} (compatibility version 1.0.0, current version 1.0.0)\n");
            assert_eq!(
                apple_dependency_report(report.as_bytes()),
                Err(ProbeReason::UnsupportedDependencies)
            );
        }
        assert_eq!(
            ProbeReason::UnsupportedDependencies.as_str(),
            "unsupported_implementation_dependencies"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn identity_filesystem_wait_observes_deadline_and_cancellation() {
        let token = CancellationToken::new();
        let result: Result<(), ProbeReason> = bounded_identity_io(
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            &token,
            std::future::pending(),
        )
        .await;
        assert_eq!(result, Err(ProbeReason::IdentityUnavailable));
        token.cancel();
        let result: Result<(), ProbeReason> = bounded_identity_io(
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            &token,
            std::future::pending(),
        )
        .await;
        assert_eq!(result, Err(ProbeReason::Cancelled));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_symlink_retarget_cannot_reuse_old_executable_attestation() {
        let root = temporary_root();
        let a = root.path().join("program-a");
        let b = root.path().join("program-b");
        let configured = root.path().join("configured");
        tokio::fs::write(&a, b"implementation a")
            .await
            .expect("first implementation");
        tokio::fs::write(&b, b"implementation b")
            .await
            .expect("second implementation");
        std::os::unix::fs::symlink(&a, &configured).expect("configured alias to A");
        let captured = crate::ffmpeg::EncodedExecutable::capture_program(
            configured.to_str().expect("UTF-8 test path"),
        )
        .await
        .expect("capture configured implementation A");
        tokio::fs::remove_file(&configured)
            .await
            .expect("remove configured alias");
        std::os::unix::fs::symlink(&b, &configured).expect("retarget configured alias to B");
        let current = crate::ffmpeg::EncodedExecutable::capture_program(
            configured.to_str().expect("UTF-8 test path"),
        )
        .await
        .expect("capture configured implementation B");
        assert!(a.exists(), "the old canonical executable remains present");
        assert!(!same_executable(&current, &captured));
    }

    #[cfg(unix)]
    async fn paused_publication_is_settled_before_cleanup(reason: ProbeReason) {
        let root = temporary_root();
        let directory = plurx_core::fs_secure::SecureDirectory::open(root.path())
            .await
            .expect("secure test authority");
        let (paused, ready) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer_directory = directory.clone();
        let writer = tokio::spawn(async move {
            writer_directory
                .atomic_write_child_with_commit(
                    "probe-paused.mp4",
                    b"encoded probe",
                    move |rename| {
                        paused.send(()).expect("notify final pre-rename pause");
                        released.recv().expect("test releases paused writer");
                        rename()
                    },
                )
                .await
        });
        ready.await.expect("writer reached final pre-rename pause");
        let token = CancellationToken::new();
        let deadline = if reason == ProbeReason::Cancelled {
            token.cancel();
            tokio::time::Instant::now() + std::time::Duration::from_secs(10)
        } else {
            tokio::time::Instant::now()
        };
        let (settling, joined) = tokio::sync::oneshot::channel();
        let publication = tokio::spawn(async move {
            settle_probe_publication(
                &directory,
                "probe-paused.mp4",
                deadline,
                &token,
                async move {
                    settling
                        .send(())
                        .expect("observe retained writer settlement");
                    writer.await.map_err(std::io::Error::other)?
                },
            )
            .await
        });
        joined
            .await
            .expect("publication owner awaits the paused writer after stop");
        assert!(
            !publication.is_finished(),
            "cancellation/deadline must retain writer settlement"
        );
        release.send(()).expect("release final rename");
        assert_eq!(
            publication.await.expect("join publication owner"),
            Err(reason)
        );
        assert_eq!(
            std::fs::read_dir(root.path())
                .expect("settled cache directory")
                .count(),
            0,
            "late rename and staging file must both be cleaned before owner returns"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graph_deadline_settles_paused_writer_before_removing_output() {
        paused_publication_is_settled_before_cleanup(ProbeReason::GraphTimedOut).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graph_cancellation_settles_paused_writer_before_removing_output() {
        paused_publication_is_settled_before_cleanup(ProbeReason::Cancelled).await;
    }

    #[cfg(unix)]
    async fn killed_probe(reason: ProbeReason) {
        let root = temporary_root();
        let pid_file = root.path().join("pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "printf '%s' $$ > \"$1\"; exec /bin/sleep 60", "probe"])
            .arg(&pid_file);
        let cancelled = CancellationToken::new();
        let canceller = if reason == ProbeReason::Cancelled {
            let token = cancelled.clone();
            Some(tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                token.cancel();
            }))
        } else {
            None
        };
        let budget = if reason == ProbeReason::Cancelled {
            std::time::Duration::from_secs(5)
        } else {
            std::time::Duration::from_millis(250)
        };
        assert_eq!(
            bounded_probe_command(
                command,
                tokio::time::Instant::now() + budget,
                1024,
                &cancelled
            )
            .await,
            Err(reason)
        );
        if let Some(canceller) = canceller {
            canceller.await.expect("join test cancellation trigger");
        }
        let pid: libc::pid_t = tokio::fs::read_to_string(pid_file)
            .await
            .expect("owned probe child published its PID")
            .parse()
            .expect("child PID is an integer");
        // The helper returned only after waiting for this exact child.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graph_deadline_kills_and_reaps_the_owned_child() {
        killed_probe(ProbeReason::GraphTimedOut).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graph_cancellation_kills_and_reaps_the_owned_child() {
        killed_probe(ProbeReason::Cancelled).await;
    }
}
