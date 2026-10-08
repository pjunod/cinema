//! Embedded Mac smoke inputs and node-local, immutable compatibility reports.
//!
//! The caller launches explicit reprobes after startup. This owner never stores
//! the operator preference, selects a production plan, or schedules retries.
//! Production graph spelling belongs to `Pipeline`, including processing order.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use plurx_core::transcode::{
    MacosProcessingAvailability, MacosProcessingContext, MacosProcessingIdentity,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MANIFEST: &[u8] = include_bytes!("../fixtures/macos-processing/manifest.json");
const SDR8: &[u8] = include_bytes!("../fixtures/macos-processing/sdr8.mp4");
const SDR10: &[u8] = include_bytes!("../fixtures/macos-processing/sdr10.mp4");
const HDR10: &[u8] = include_bytes!("../fixtures/macos-processing/hdr10.mp4");
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
    identity: Option<MacosProcessingIdentity>,
}

impl MacosVideoReport {
    fn pending(generation: u64) -> Self {
        Self {
            generation,
            sdr_scale: GraphObservation::pending(),
            hdr10_metal: GraphObservation::pending(),
            identity: None,
        }
    }

    fn unavailable(generation: u64, reason: ProbeReason) -> Self {
        Self {
            generation,
            sdr_scale: GraphObservation::from_result(Err(reason)),
            hdr10_metal: GraphObservation::from_result(Err(reason)),
            identity: None,
        }
    }

    /// Capture one report for one new plan; readiness never modifies `enabled`.
    pub(crate) fn context(&self, enabled: bool) -> Option<MacosProcessingContext> {
        self.identity.as_ref().map(|identity| {
            MacosProcessingContext::new(
                enabled,
                identity.clone(),
                self.sdr_scale.availability,
                self.hdr10_metal.availability,
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
            "qualification": "external_advisory",
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

#[derive(Debug, Deserialize)]
struct Corpus {
    schema_version: u32,
    corpus_version: u32,
    generator_recipe_version: u32,
    size_budget_bytes: usize,
    aggregate_media_bytes: usize,
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
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
    output_expectations: Vec<OutputExpectation>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct OutputExpectation {
    graph_id: String,
    expected: OutputFacts,
    timestamps: Timestamps,
    pixels: PixelExpectation,
    forbidden_side_data: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OutputFacts {
    codec_name: String,
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

#[derive(Debug, Deserialize)]
struct Timestamps {
    start_seconds: f64,
    step_seconds: f64,
    absolute_tolerance_seconds: f64,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
const ALL_GRAPHS_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
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
        let key = digest(MANIFEST);
        let directory = owner
            .create_child_directory(&key)
            .await
            .map_err(|_| ProbeReason::CacheUnavailable)?;
        verify_private_directory(&directory).await?;
        let path = root.join("macos-processing").join(key);
        for (name, bytes) in std::iter::once(("manifest.json".to_owned(), MANIFEST)).chain(
            corpus
                .fixtures
                .iter()
                .zip([SDR8, SDR10, HDR10])
                .map(|(fixture, bytes)| (format!("{}.mp4", fixture.sha256), bytes)),
        ) {
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

async fn capture_implementation(
    cancelled: &CancellationToken,
) -> Result<Implementation, ProbeReason> {
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
    // Only file capture is cancellable by dropping this future. Once a child
    // exists, its existing bounded owner handles timeout/cancellation and reap.
    let (ffmpeg, ffprobe) = tokio::select! {
        biased;
        () = cancelled.cancelled() => return Err(ProbeReason::Cancelled),
        result = tokio::time::timeout(IDENTITY_BUDGET, executable_capture) =>
            result.unwrap_or(Err(ProbeReason::IdentityUnavailable))?,
    };
    {
        // This existing dependency collector owns bounded children internally;
        // await its settlement rather than dropping their resource ownership.
        let linked = crate::ffmpeg::fragment_index_engine_digest().await;
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err(ProbeReason::IdentityUnavailable);
        }
        let (os_build, hardware_class) = platform_facts()?;
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
        let listing = crate::ffmpeg::bounded_command_output_cancellable(
            command,
            IDENTITY_BUDGET,
            1024 * 1024,
            "Mac filter inventory",
            Some(cancelled),
            PROBE_WORK,
        )
        .await
        .map_err(|_| {
            if cancelled.is_cancelled() {
                ProbeReason::Cancelled
            } else {
                ProbeReason::IdentityUnavailable
            }
        })?;
        let filters =
            String::from_utf8(listing.stdout).map_err(|_| ProbeReason::IdentityUnavailable)?;
        Ok(Implementation {
            ffmpeg,
            ffprobe,
            identity,
            filters,
        })
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
    let objects: Arc<[(PathBuf, String)]> = vec![
        implementation.ffmpeg.attestation_object(),
        implementation.ffprobe.attestation_object(),
    ]
    .into();
    crate::ffmpeg::engine_objects_are_current_batch(None, objects)
        .await
        .0
        && crate::ffmpeg::fragment_index_engine_is_current().await
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
    };
    // Real identity is now known: new plans can retain pending per-class
    // observations without inventing an implementation fingerprint.
    owner.publish(report.clone());
    let deadline = tokio::time::Instant::now() + ALL_GRAPHS_BUDGET;
    let mut sdr_result = Ok(());
    for fixture in &corpus.fixtures {
        let pipeline = if fixture.class == "hdr10" {
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
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let name = format!("{}.mp4", fixture.sha256);
    let mut file = prepared
        .directory
        .open_read_child(&name)
        .await
        .map_err(|_| ProbeReason::CacheUnavailable)?;
    let mut bytes = Vec::with_capacity(fixture.byte_length);
    (&mut file)
        .take(fixture.byte_length as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ProbeReason::CacheUnavailable)?;
    if bytes.len() != fixture.byte_length || digest(&bytes) != fixture.sha256 {
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

async fn run_smoke(
    prepared: &PreparedCorpus,
    fixture: &Fixture,
    pipeline: plurx_core::transcode::Pipeline,
    implementation: &Implementation,
    cancelled: &CancellationToken,
    budget: std::time::Duration,
) -> Result<(), ProbeReason> {
    use plurx_core::transcode::{EffectiveRateControl, Encoder};
    let deadline = tokio::time::Instant::now() + budget;
    if cancelled.is_cancelled() {
        return Err(ProbeReason::Cancelled);
    }
    let hdr = fixture.class == "hdr10";
    let required: &[&str] = if hdr {
        &["scale_vt", "tonemap_videotoolbox"]
    } else {
        &["scale_vt"]
    };
    if !crate::pipeprobe::declares_filters(&implementation.filters, required) {
        return Err(ProbeReason::MissingFilter);
    }
    if !bounded_graph_io(deadline, cancelled, async {
        Ok(implementation_is_current(implementation).await)
    })
    .await?
    {
        return Err(ProbeReason::ImplementationChanged);
    }
    let contract = fixture
        .output_expectations
        .iter()
        .find(|e| {
            e.graph_id
                == if hdr {
                    "vt_tonemap_metal"
                } else {
                    "vt_scale_sdr"
                }
        })
        .ok_or(ProbeReason::InvalidEmbeddedCorpus)?;
    let source = bounded_graph_io(deadline, cancelled, verified_source(prepared, fixture)).await?;
    let source_path = prepared.path.join(format!("{}.mp4", fixture.sha256));
    let mut encode = tokio::process::Command::new(&implementation.ffmpeg.path);
    let source_arg = held_file_argument(&mut encode, &source, &source_path);
    encode.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    encode.args(pipeline.decode_args());
    encode
        .arg("-i")
        .arg(source_arg)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn"]);
    let filter = pipeline
        .filters(Some(160), 90, hdr.then_some("hdr10"))
        .ok_or(ProbeReason::GraphFailed)?;
    encode.args(["-vf", &filter]);
    encode.args(Encoder::VideoToolbox.encode_args(500, EffectiveRateControl::Vbr, false, None));
    encode.args([
        "-allow_sw",
        "0",
        "-bf",
        "0",
        "-frames:v",
        "12",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
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
    let publication = bounded_graph_io(deadline, cancelled, async {
        prepared
            .directory
            .atomic_write_child_cooperative(&name, &encoded, move || {
                !token.is_cancelled() && tokio::time::Instant::now() < deadline
            })
            .await
            .map_err(|_| ProbeReason::CacheUnavailable)
    })
    .await;
    if let Err(reason) = publication {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            prepared.directory.unlink_child(&name),
        )
        .await;
        return Err(reason);
    }
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
                "13",
                "-fps_mode",
                "passthrough",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ]);
        let raw = bounded_probe_command(decode, deadline, 160 * 90 * 3 / 2 * 13, cancelled).await?;
        observe_output(&document, &raw, contract)
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
