//! Hash-bound, current-access-unit Dolby controls for the existing probe owner.
use super::*;
use plurx_core::transcode::{EffectiveRateControl, Encoder, OutputGrade, Pipeline, VideoCodec};

pub(crate) const MANIFEST: &[u8] = include_bytes!("../../fixtures/macos-processing/strict-p5.json");
/// The strict corpus raster, the manifest's `source_shape`. An encoded probe
/// that downscales below it inherits each encoder's minimum frame size: AMD
/// VA-API aligns 160x90 to 160x96 and refuses anything under 128 rows, which
/// reads as "no Dolby support" when the decoder worked. Encoded positive
/// probes therefore keep this raster, and the delivered pixels are projected
/// to the independent 160x90 color oracle before they are compared.
pub(crate) const SOURCE_RASTER: (u32, u32) = (320, 180);
macro_rules! media {
    ($($name:literal),+ $(,)?) => { &[$(($name, include_bytes!(concat!("../../fixtures/macos-processing/strict_p5_", $name, ".mp4")) as &[u8])),+] };
}
pub(crate) const MEDIA: &[(&str, &[u8])] = media!(
    "fresh",
    "variable",
    "missing_first",
    "missing_midstream",
    "missing_seek_start",
    "malformed_first",
    "malformed_midstream",
    "omitted_color_first",
    "omitted_color_midstream",
    "terminal_eos",
    "terminal_eob",
    "terminal_eos_eob"
);

pub(super) fn add_work(
    corpus: &Corpus,
    work: &mut Vec<(Fixture, SmokeOperation, MacosProcessingGraph)>,
) -> Result<(), ProbeReason> {
    let invalid = ProbeReason::InvalidEmbeddedCorpus;
    let doc: Value = serde_json::from_slice(MANIFEST).map_err(|_| invalid)?;
    let cases = doc["cases"].as_array().ok_or(invalid)?;
    if doc["schema_version"] != 1
        || doc["generator_recipe_version"] != 3
        || doc["license"] != "CC0-1.0"
        || doc["source_shape"] != serde_json::json!([320, 180])
        || doc["frame_count"] != 24
        || doc["frame_rate"] != serde_json::json!([12, 1])
        || doc["source_sample_aspect_ratio"] != "1:1"
        || cases.len() != MEDIA.len()
        || MANIFEST.len() + MEDIA.iter().map(|(_, b)| b.len()).sum::<usize>() > 256 * 1024
    {
        return Err(invalid);
    }
    for (case, (id, bytes)) in cases.iter().zip(MEDIA) {
        if case["id"] != *id
            || case["path"] != format!("strict_p5_{id}.mp4")
            || case["sha256"] != digest(bytes)
            || case["byte_length"].as_u64() != Some(bytes.len() as u64)
            || bytes.is_empty()
            || case["seek_seconds"].as_u64().is_none()
            || (matches!(
                *id,
                "fresh" | "variable" | "terminal_eos" | "terminal_eob" | "terminal_eos_eob"
            ) != case["negative_max_frames"].is_null())
        {
            return Err(invalid);
        }
        let expected_maximum = match *id {
            "fresh" | "variable" | "terminal_eos" | "terminal_eob" | "terminal_eos_eob" => None,
            "missing_midstream" | "malformed_midstream" | "omitted_color_midstream" => Some(10),
            _ => Some(0),
        };
        if case["negative_max_frames"].as_u64() != expected_maximum
            || case["seek_seconds"].as_u64() != Some(u64::from(*id == "missing_seek_start"))
        {
            return Err(invalid);
        }
        let mut fixture = corpus.fixtures[2].clone();
        fixture.id = format!("strict_p5_{id}");
        fixture.class = "p5".into();
        fixture.sha256 = digest(bytes);
        fixture.byte_length = bytes.len();
        fixture.input_pixels = case.clone();
        for (pipeline, encoder, graph) in [
            (
                Pipeline::DoviStrictTonemapx,
                Encoder::VideoToolbox,
                MacosProcessingGraph::P5SoftwareCpu,
            ),
            (
                Pipeline::DoviStrictTonemapx,
                Encoder::Software,
                MacosProcessingGraph::P5SoftwareCpu,
            ),
            (
                Pipeline::VtDoviTonemapx,
                Encoder::VideoToolbox,
                MacosProcessingGraph::P5VtTonemapx,
            ),
            (
                Pipeline::DoviMetal,
                Encoder::VideoToolbox,
                MacosProcessingGraph::P5SoftwareMetal,
            ),
            (
                Pipeline::VtDoviMetal,
                Encoder::VideoToolbox,
                MacosProcessingGraph::P5VtMetal,
            ),
        ] {
            work.push((
                fixture.clone(),
                SmokeOperation::StrictP5(pipeline, encoder),
                graph,
            ));
        }
    }
    Ok(())
}

pub(crate) fn observe_negative(
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
    maximum: usize,
) -> Result<(), ProbeReason> {
    let diagnostic = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let strict_error = [
        "required current-access-unit dolby state missing",
        "required effective dolby mapping/color state incomplete",
        "required effective dolby metadata missing or unsupported",
        "error parsing dovi nal unit",
    ]
    .iter()
    .any(|message| diagnostic.contains(message));
    if status.code().is_none_or(|code| code == 0)
        || !stdout.len().is_multiple_of(160 * 90 * 3 / 2)
        || stdout.len() / (160 * 90 * 3 / 2) > maximum
        || !strict_error
    {
        return Err(ProbeReason::OutputContractFailed);
    }
    Ok(())
}

// showinfo is metadata-only with checksum=0; its position immediately before
// the mapper observes the selected decoder's AVFrame after any layout adapter.
const METADATA_OBSERVER: &str = "showinfo@plurx_p5_contract=checksum=0,";

fn observed_filter(pipeline: Pipeline) -> Result<String, ProbeReason> {
    let filter = pipeline
        .filters(Some(160), 90, Some("dovi"))
        .ok_or(ProbeReason::GraphFailed)?;
    let mapper = if matches!(pipeline, Pipeline::DoviMetal | Pipeline::VtDoviMetal) {
        "tonemap_videotoolbox="
    } else {
        "tonemapx="
    };
    let offset = filter.find(mapper).ok_or(ProbeReason::GraphFailed)?;
    let mut observed = filter;
    observed.insert_str(offset, METADATA_OBSERVER);
    Ok(observed)
}

fn log_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut tokens = line.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        if let Some(value) = token.strip_prefix(key) {
            let value = if value.is_empty() {
                tokens.next()?
            } else {
                value
            };
            return Some(value.trim_end_matches(';')).filter(|value| !value.is_empty());
        }
    }
    None
}

#[derive(Default)]
struct SelectedFrameMetadata {
    header: bool,
    mapping: bool,
    signature: Option<(u64, u64)>,
}

pub(crate) fn observe_selected_metadata(
    stderr: &[u8],
    pipeline: Pipeline,
    variable: bool,
) -> Result<(), ProbeReason> {
    let expected_format = if matches!(pipeline, Pipeline::DoviMetal | Pipeline::VtDoviMetal) {
        "videotoolbox_vld"
    } else {
        "yuv420p10le"
    };
    observe_selected_layout_metadata(stderr, variable, expected_format, "plurx_p5_contract")
}

/// Pure observations shared by selected decoder graphs. The caller must put
/// this named metadata-only observer at the actual layout being asserted;
/// opaque hardware frames are never normalized to a software-format claim.
pub(crate) fn observe_selected_layout_metadata(
    stderr: &[u8],
    variable: bool,
    expected_format: &str,
    observer: &str,
) -> Result<(), ProbeReason> {
    observe_selected_metadata_shape(stderr, variable, expected_format, observer, false)
}

#[cfg(target_os = "linux")]
pub(crate) fn observe_selected_4k_metadata(
    stderr: &[u8],
    expected_format: &str,
    observer: &str,
) -> Result<(), ProbeReason> {
    observe_selected_metadata_shape(stderr, false, expected_format, observer, true)
}

fn observe_selected_metadata_shape(
    stderr: &[u8],
    variable: bool,
    expected_format: &str,
    observer: &str,
    uhd24: bool,
) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    if !matches!(
        expected_format,
        "vaapi" | "yuv420p10le" | "videotoolbox_vld"
    ) || !matches!(observer, "plurx_p5_hardware" | "plurx_p5_contract")
    {
        return Err(fail);
    }
    let text = std::str::from_utf8(stderr).map_err(|_| fail)?;
    let reference: Value =
        serde_json::from_slice(MANIFEST).map_err(|_| ProbeReason::InvalidEmbeddedCorpus)?;
    let prefix = format!("[showinfo@{observer} @ ");
    let configuration = if uhd24 {
        "config in time_base: 1/12288, frame_rate: 24/1"
    } else {
        "config in time_base: 1/12288, frame_rate: 12/1"
    };
    let pts_step = if uhd24 { 512 } else { 1024 };
    let fps = if uhd24 { 24.0 } else { 12.0 };
    let shape = if uhd24 { "3840x2160" } else { "320x180" };
    let mut frames = Vec::<SelectedFrameMetadata>::new();
    let mut configured = false;
    for line in text.lines().filter(|line| line.starts_with(&prefix)) {
        let body = line.split_once(']').ok_or(fail)?.1.trim();
        if body.starts_with("config in time_base:") {
            if configured || body != configuration {
                return Err(fail);
            }
            configured = true;
        } else if body.starts_with("n:") {
            let number = log_field(body, "n:")
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or(fail)?;
            let pts = log_field(body, "pts:")
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(fail)?;
            let time = log_field(body, "pts_time:")
                .and_then(|v| v.parse::<f64>().ok())
                .ok_or(fail)?;
            if !configured
                || number != frames.len()
                || number >= 24
                || pts != number as u64 * pts_step
                || !time.is_finite()
                || (time - number as f64 / fps).abs() > 0.00001
                || log_field(body, "fmt:") != Some(expected_format)
                || log_field(body, "sar:") != Some("1/1")
                || log_field(body, "s:") != Some(shape)
                || log_field(body, "i:") != Some("P")
            {
                return Err(fail);
            }
            frames.push(SelectedFrameMetadata::default());
        } else if body.contains("rpu_type=") {
            let frame = frames.last_mut().ok_or(fail)?;
            if frame.header {
                return Err(fail);
            }
            for (key, expected) in [
                ("rpu_type=", "2"),
                ("vdr_rpu_profile=", "0"),
                ("bl_bit_depth=", "10"),
                ("disable_residual_flag=", "1"),
            ] {
                if log_field(body, key) != Some(expected) {
                    return Err(fail);
                }
            }
            frame.header = true;
        } else if body.contains("color metadata:") {
            let index = frames.len().checked_sub(1).ok_or(fail)?;
            let frame = &mut frames[index];
            if frame.mapping
                || body.matches("channel ").count() != 3
                || !body.contains("channel 0:")
                || !body.contains("channel 1:")
                || !body.contains("channel 2:")
            {
                return Err(fail);
            }
            frame.mapping = true;
            for (key, expected) in [
                ("signal_eotf=", "65535"),
                ("signal_color_space=", "2"),
                ("signal_bit_depth=", "12"),
            ] {
                if log_field(body, key) != Some(expected) {
                    return Err(fail);
                }
            }
            let dm = log_field(body, "dm_metadata_id=")
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(fail)?;
            let peak = log_field(body, "source_max_pq=")
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(fail)?;
            let expected = if variable {
                let signature = &reference["variable_frame_metadata"][index];
                (
                    signature["dm_metadata_id"].as_u64().ok_or(fail)?,
                    signature["source_max_pq"].as_u64().ok_or(fail)?,
                )
            } else {
                (0, 3079)
            };
            if (dm, peak) != expected || frame.signature.replace((dm, peak)).is_some() {
                return Err(fail);
            }
        }
    }
    if frames.len() != 24
        || frames
            .iter()
            .any(|frame| !frame.header || !frame.mapping || frame.signature.is_none())
    {
        return Err(fail);
    }
    Ok(())
}

pub(crate) fn observe_colors(raw: &[u8]) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    if raw.len() != 24 * 160 * 90 * 3 / 2 {
        return Err(fail);
    }
    let reference: Value =
        serde_json::from_slice(MANIFEST).map_err(|_| ProbeReason::InvalidEmbeddedCorpus)?;
    let oracle = &reference["color_interpretation_oracle"];
    let ranges = oracle["yuv420p_ranges"].as_array().ok_or(fail)?;
    let centers = oracle["sample_centers"].as_array().ok_or(fail)?;
    if ranges.len() != 6 || centers.len() != 6 {
        return Err(fail);
    }
    for (patch, (range, center)) in ranges.iter().zip(centers).enumerate() {
        if range.as_array().map(Vec::len) != Some(3)
            || center != &serde_json::json!([10 + 20 * patch, 67])
        {
            return Err(fail);
        }
        let x = 10 + 20 * patch;
        let y = 67;
        for (component, (width, offset, cx, cy)) in [
            (160, 0, x, y),
            (80, 14400, x / 2, y / 2),
            (80, 18000, x / 2, y / 2),
        ]
        .into_iter()
        .enumerate()
        {
            let lo = range[component][0]
                .as_u64()
                .filter(|v| *v <= 255)
                .ok_or(fail)? as f64;
            let hi = range[component][1]
                .as_u64()
                .filter(|v| *v <= 255)
                .ok_or(fail)? as f64;
            if lo > hi {
                return Err(fail);
            }
            for frame in 0..24 {
                let mean = (cy - 1..=cy + 1)
                    .flat_map(|yy| (cx - 1..=cx + 1).map(move |xx| (yy, xx)))
                    .map(|(yy, xx)| f64::from(raw[frame * 21600 + offset + yy * width + xx]))
                    .sum::<f64>()
                    / 9.0;
                if !(lo..=hi).contains(&mean) {
                    return Err(fail);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn observe(document: &Value, raw: &[u8]) -> Result<(), ProbeReason> {
    observe_encoded(document, raw, (160, 90))
}

/// [`observe`] for a stream encoded at `encoded`. The stream and every frame
/// must report exactly that raster; `raw` is always the delivered picture
/// decoded and projected to the 160x90 oracle, so the timing, gray-ramp,
/// neutral-chroma and color-interpretation checks are the same for every
/// encoded raster.
pub(crate) fn observe_encoded(
    document: &Value,
    raw: &[u8],
    encoded: (u32, u32),
) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    let stream = &document["streams"][0];
    let frames = document["frames"].as_array().ok_or(fail)?;
    if document["streams"].as_array().map(Vec::len) != Some(1)
        || frames.len() != 24
        || raw.len() != 24 * 160 * 90 * 3 / 2
    {
        return Err(fail);
    }
    for (key, expected) in [
        ("codec_name", "h264"),
        ("sample_aspect_ratio", "1:1"),
        ("avg_frame_rate", "12/1"),
        ("field_order", "progressive"),
        ("pix_fmt", "yuv420p"),
    ] {
        if stream[key].as_str() != Some(expected) {
            return Err(fail);
        }
    }
    for frame in std::iter::once(stream).chain(frames) {
        if frame["width"] != encoded.0 || frame["height"] != encoded.1 {
            return Err(fail);
        }
        for (key, expected) in [
            ("color_primaries", "bt709"),
            ("color_transfer", "bt709"),
            ("color_space", "bt709"),
            ("color_range", "tv"),
        ] {
            if frame[key].as_str() != Some(expected) {
                return Err(fail);
            }
        }
        if frame["side_data_list"].as_array().is_some_and(|sides| {
            sides.iter().any(|side| {
                let name = side["side_data_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                name.contains("dolby")
                    || name.contains("dovi")
                    || name.contains("mastering")
                    || name.contains("content light")
            })
        }) {
            return Err(fail);
        }
    }
    for (i, frame) in frames.iter().enumerate() {
        let pts = frame["best_effort_timestamp_time"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or(fail)?;
        if (pts - i as f64 / 12.0).abs() > 0.001 {
            return Err(fail);
        }
        let base = i * 160 * 90 * 3 / 2;
        let gray = (0..8)
            .map(|patch| raw[base + 23 * 160 + 10 + 20 * patch])
            .collect::<Vec<_>>();
        if gray[0].abs_diff(16) > 8
            || gray[7] < 100
            || gray.windows(2).any(|p| p[0] > p[1].saturating_add(8))
        {
            return Err(fail);
        }
        for patch in 0..8 {
            let uv = 11 * 80 + 5 + 10 * patch;
            if raw[base + 160 * 90 + uv].abs_diff(128) > 8
                || raw[base + 160 * 90 + 160 * 90 / 4 + uv].abs_diff(128) > 8
            {
                return Err(fail);
            }
        }
    }
    observe_colors(raw)
}

pub(crate) fn observe_source_pixels(raw: &[u8]) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    let reference: Value =
        serde_json::from_slice(MANIFEST).map_err(|_| ProbeReason::InvalidEmbeddedCorpus)?;
    let codes = reference["source_codes"].as_array().ok_or(fail)?;
    let stride = 320 * 180 * 3;
    if codes.len() != 16 || raw.len() != 24 * stride {
        return Err(fail);
    }
    for frame in 0..24 {
        for (patch, expected) in codes.iter().enumerate() {
            let x = 20 + (patch % 8) * 40;
            let y = if patch < 8 { 45 } else { 135 };
            let positions = [
                y * 320 + x,
                320 * 180 + (y / 2) * 160 + x / 2,
                320 * 180 + 320 * 180 / 4 + (y / 2) * 160 + x / 2,
            ];
            for (component, position) in positions.into_iter().enumerate() {
                let offset = frame * stride + position * 2;
                let value = u16::from_le_bytes([raw[offset], raw[offset + 1]]);
                if expected[component].as_u64() != Some(u64::from(value)) {
                    return Err(fail);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn observe_source(document: &Value, variable: bool) -> Result<(), ProbeReason> {
    let fail = ProbeReason::OutputContractFailed;
    let frames = document["frames"].as_array().ok_or(fail)?;
    let reference: Value =
        serde_json::from_slice(MANIFEST).map_err(|_| ProbeReason::InvalidEmbeddedCorpus)?;
    if frames.len() != 24 {
        return Err(fail);
    }
    for (index, frame) in frames.iter().enumerate() {
        if frame["pts"].as_u64() != Some(index as u64 * 1024)
            || frame["sample_aspect_ratio"] != "1:1"
            || frame["color_space"] != "ipt-c2"
            || frame["color_range"] != "pc"
            || frame["width"] != 320
            || frame["height"] != 180
        {
            return Err(fail);
        }
        let metadata = frame["side_data_list"]
            .as_array()
            .and_then(|sides| {
                sides
                    .iter()
                    .find(|side| side["side_data_type"] == "Dolby Vision Metadata")
            })
            .ok_or(fail)?;
        for (key, value) in [
            ("vdr_rpu_profile", 0),
            ("bl_bit_depth", 10),
            ("disable_residual_flag", 1),
            ("signal_eotf", 65535),
            ("signal_color_space", 2),
        ] {
            if metadata[key].as_u64() != Some(value) {
                return Err(fail);
            }
        }
        if metadata["components"].as_array().map(Vec::len) != Some(3) {
            return Err(fail);
        }
        if variable
            && (metadata["dm_metadata_id"]
                != reference["variable_frame_metadata"][index]["dm_metadata_id"]
                || metadata["source_max_pq"]
                    != reference["variable_frame_metadata"][index]["source_max_pq"])
        {
            return Err(fail);
        }
    }
    Ok(())
}

pub(super) async fn run(
    prepared: &PreparedCorpus,
    fixture: &Fixture,
    pipeline: Pipeline,
    encoder: Encoder,
    implementation: &Implementation,
    cancelled: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<(), ProbeReason> {
    let required = if matches!(pipeline, Pipeline::DoviMetal | Pipeline::VtDoviMetal) {
        &["tonemap_videotoolbox", "scale_vt", "showinfo"][..]
    } else {
        &["tonemapx", "scale", "showinfo"][..]
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
    let source = bounded_graph_io(deadline, cancelled, verified_source(prepared, fixture)).await?;
    let source_path = prepared.path.join(format!("{}.mp4", fixture.sha256));
    if fixture.input_pixels["negative_max_frames"].is_null() {
        let mut probe = tokio::process::Command::new(&implementation.ffprobe.path);
        let argument = held_file_argument(&mut probe, &source, &source_path);
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_frames",
                "-of",
                "json",
            ])
            .arg(argument);
        let document: Value = serde_json::from_slice(
            &bounded_probe_command(probe, deadline, 512 * 1024, cancelled).await?,
        )
        .map_err(|_| ProbeReason::OutputContractFailed)?;
        observe_source(&document, fixture.input_pixels["id"] == "variable")?;
        use std::io::Seek;
        (&source)
            .rewind()
            .map_err(|_| ProbeReason::CacheUnavailable)?;
        let mut decode = tokio::process::Command::new(&implementation.ffmpeg.path);
        let argument = held_file_argument(&mut decode, &source, &source_path);
        decode
            .args([
                "-v", "error", "-nostdin", "-threads", "1", "-hwaccel", "none", "-c:v", "hevc",
                "-i",
            ])
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
                "yuv420p10le",
                "-f",
                "rawvideo",
                "pipe:1",
            ]);
        let raw = bounded_probe_command(decode, deadline, 25 * 320 * 180 * 3, cancelled).await?;
        observe_source_pixels(&raw)?;
        (&source)
            .rewind()
            .map_err(|_| ProbeReason::CacheUnavailable)?;
    }
    let mut command = tokio::process::Command::new(&implementation.ffmpeg.path);
    let argument = held_file_argument(&mut command, &source, &source_path);
    command.args([
        "-hide_banner",
        "-loglevel",
        if fixture.input_pixels["negative_max_frames"].is_null() {
            "info"
        } else {
            "warning"
        },
        "-nostats",
        "-nostdin",
        "-xerror",
        "-threads",
        "1",
    ]);
    command.args(pipeline.device_args(encoder));
    command.args(
        pipeline
            .strict_dolby_input_args()
            .ok_or(ProbeReason::GraphFailed)?,
    );
    if fixture.input_pixels["seek_seconds"] == 1 {
        command.args(["-ss", "1"]);
    }
    command
        .arg("-i")
        .arg(argument)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"]);
    let mut filter = if fixture.input_pixels["negative_max_frames"].is_null() {
        observed_filter(pipeline)?
    } else {
        pipeline
            .filters(Some(160), 90, Some("dovi"))
            .ok_or(ProbeReason::GraphFailed)?
    };
    if fixture.input_pixels["negative_max_frames"]
        .as_u64()
        .is_some()
        && matches!(pipeline, Pipeline::DoviMetal | Pipeline::VtDoviMetal)
    {
        filter.push_str(",hwdownload,format=nv12,format=yuv420p");
    }
    command.arg(filter);
    if let Some(maximum) = fixture.input_pixels["negative_max_frames"].as_u64() {
        command.args([
            "-frames:v",
            "24",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(ProbeReason::GraphTimedOut);
        }
        let outcome = crate::ffmpeg::bounded_command_capture_cancellable(
            command,
            remaining,
            24 * 160 * 90 * 3 / 2,
            "Mac strict Dolby control",
            Some(cancelled),
            PROBE_WORK,
        )
        .await
        .map_err(|_| ProbeReason::GraphFailed)?;
        let output = outcome.output.map_err(|_| ProbeReason::GraphFailed)?;
        return observe_negative(
            outcome.status,
            &output.stdout,
            &output.stderr,
            maximum as usize,
        );
    }
    command.args(encoder.encode_args_for_codec(
        VideoCodec::H264,
        OutputGrade::Sdr,
        500,
        EffectiveRateControl::Vbr,
        false,
        None,
    ));
    if encoder == Encoder::VideoToolbox {
        command.args(["-allow_sw", "0"]);
    }
    command.args([
        "-bf",
        "0",
        "-frames:v",
        "24",
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
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(ProbeReason::GraphTimedOut);
    }
    let output = crate::ffmpeg::bounded_command_output_cancellable(
        command,
        remaining,
        CORPUS_BUDGET as u64,
        "Mac strict Dolby graph",
        Some(cancelled),
        PROBE_WORK,
    )
    .await
    .map_err(|_| {
        if cancelled.is_cancelled() {
            ProbeReason::Cancelled
        } else if tokio::time::Instant::now() >= deadline {
            ProbeReason::GraphTimedOut
        } else {
            ProbeReason::GraphFailed
        }
    })?;
    observe_selected_metadata(
        &output.stderr,
        pipeline,
        fixture.input_pixels["id"] == "variable",
    )?;
    let encoded = output.stdout;
    let name = format!("p5-probe-{}.mp4", uuid::Uuid::new_v4().simple());
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
    let result = async {
        let path = prepared.path.join(&name);
        let file = bounded_graph_io(deadline, cancelled, async {
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
        let arg = held_file_argument(&mut probe, &file, &path);
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
            .arg(arg);
        let doc: Value = serde_json::from_slice(
            &bounded_probe_command(probe, deadline, 512 * 1024, cancelled).await?,
        )
        .map_err(|_| ProbeReason::OutputContractFailed)?;
        let file = bounded_graph_io(deadline, cancelled, async {
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
        let arg = held_file_argument(&mut decode, &file, &path);
        decode
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(arg)
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
        let raw = bounded_probe_command(decode, deadline, 25 * 160 * 90 * 3 / 2, cancelled).await?;
        observe(&doc, &raw)
    }
    .await;
    if !matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            prepared.directory.unlink_child(&name)
        )
        .await,
        Ok(Ok(()))
    ) {
        return Err(ProbeReason::CacheUnavailable);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_controls_cover_every_graph_and_software_encoder_recovery() {
        let corpus = embedded_corpus(super::super::MANIFEST, SDR8, SDR10, HDR10)
            .expect("valid baseline corpus");
        let mut work = Vec::new();
        add_work(&corpus, &mut work).expect("valid hash-bound strict corpus");
        assert_eq!(work.len(), 60);
        for graph in [
            MacosProcessingGraph::P5SoftwareCpu,
            MacosProcessingGraph::P5VtTonemapx,
            MacosProcessingGraph::P5SoftwareMetal,
            MacosProcessingGraph::P5VtMetal,
        ] {
            let controls = work
                .iter()
                .filter(|(_, _, g)| *g == graph)
                .collect::<Vec<_>>();
            assert_eq!(
                controls.len(),
                if graph == MacosProcessingGraph::P5SoftwareCpu {
                    24
                } else {
                    12
                }
            );
            assert!(controls
                .iter()
                .any(|(f, _, _)| f.input_pixels["seek_seconds"] == 1));
            assert!(controls
                .iter()
                .any(|(f, _, _)| f.input_pixels["id"] == "omitted_color_midstream"));
            for terminal in ["terminal_eos", "terminal_eob", "terminal_eos_eob"] {
                assert!(controls
                    .iter()
                    .any(|(fixture, _, _)| fixture.input_pixels["id"] == terminal
                        && fixture.input_pixels["negative_max_frames"].is_null()));
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn strict_negative_requires_diagnostic_nonzero_and_affected_frame_absence() {
        use std::os::unix::process::ExitStatusExt;
        let fail = ProbeReason::OutputContractFailed;
        let status = std::process::ExitStatus::from_raw(1 << 8);
        let message = b"Required current-access-unit Dolby state missing at pts 12288";
        assert_eq!(observe_negative(status, &[], message, 0), Ok(()));
        assert_eq!(observe_negative(status, &[0], message, 0), Err(fail));
        assert_eq!(
            observe_negative(status, &vec![0; 160 * 90 * 3 / 2 * 11], message, 10),
            Err(fail)
        );
        assert_eq!(
            observe_negative(status, &[], b"unknown option strict_dovi", 0),
            Err(fail)
        );
        assert_eq!(
            observe_negative(std::process::ExitStatus::from_raw(9), &[], message, 0),
            Err(fail)
        );
        assert_eq!(
            observe_negative(std::process::ExitStatus::from_raw(0), &[], message, 0),
            Err(fail)
        );
    }
    #[test]
    fn reordered_metadata_must_remain_bound_to_its_access_unit() {
        let reference: Value = serde_json::from_slice(MANIFEST).expect("valid strict reference");
        let frames = (0..24).map(|index| serde_json::json!({
            "pts":index*1024,"sample_aspect_ratio":"1:1","color_space":"ipt-c2","color_range":"pc","width":320,"height":180,
            "side_data_list":[{"side_data_type":"Dolby Vision Metadata","vdr_rpu_profile":0,"bl_bit_depth":10,"disable_residual_flag":1,"signal_eotf":65535,"signal_color_space":2,"components":[{}, {}, {}],"dm_metadata_id":reference["variable_frame_metadata"][index]["dm_metadata_id"],"source_max_pq":reference["variable_frame_metadata"][index]["source_max_pq"]}]
        })).collect::<Vec<_>>();
        let mut document = serde_json::json!({"frames":frames});
        assert_eq!(observe_source(&document, true), Ok(()));
        document["frames"][1]["side_data_list"][0]["dm_metadata_id"] = serde_json::json!(1);
        assert_eq!(
            observe_source(&document, true),
            Err(ProbeReason::OutputContractFailed)
        );
    }
    fn selected_trace() -> Vec<[String; 3]> {
        let reference: Value = serde_json::from_slice(MANIFEST).expect("valid metadata reference");
        (0..24).map(|index| {
            let signature=&reference["variable_frame_metadata"][index];
            let dm=signature["dm_metadata_id"].as_u64().expect("DM identifier");
            let peak=signature["source_max_pq"].as_u64().expect("PQ signature");
            [format!("[showinfo@plurx_p5_contract @ 0x1] n: {index} pts: {} pts_time:{} fmt:videotoolbox_vld sar:1/1 s:320x180 i:P\n",index*1024,index as f64/12.0),
             "[showinfo@plurx_p5_contract @ 0x1] rpu_type=2; vdr_rpu_profile=0; bl_bit_depth=10; disable_residual_flag=1\n".into(),
             format!("[showinfo@plurx_p5_contract @ 0x1] channel 0: channel 1: channel 2: color metadata: dm_metadata_id={dm}; source_max_pq={peak}; signal_eotf=65535; signal_color_space=2; signal_bit_depth=12\n")]
        }).collect()
    }

    fn trace_bytes(rows: &[[String; 3]]) -> Vec<u8> {
        let mut log =
            "[showinfo@plurx_p5_contract @ 0x1] config in time_base: 1/12288, frame_rate: 12/1\n"
                .to_owned();
        for row in rows {
            for line in row {
                log.push_str(line);
            }
        }
        log.into_bytes()
    }

    #[test]
    fn selected_vaapi_metadata_cannot_be_proved_by_downloaded_or_other_observer_frames() {
        let trace = String::from_utf8(trace_bytes(&selected_trace()))
            .expect("UTF8 trace")
            .replace("videotoolbox_vld", "vaapi")
            .replace("plurx_p5_contract", "plurx_p5_hardware");
        assert_eq!(
            observe_selected_layout_metadata(trace.as_bytes(), true, "vaapi", "plurx_p5_hardware"),
            Ok(())
        );
        let downloaded = trace.replace("fmt:vaapi", "fmt:yuv420p10le");
        assert_eq!(
            observe_selected_layout_metadata(
                downloaded.as_bytes(),
                true,
                "vaapi",
                "plurx_p5_hardware"
            ),
            Err(ProbeReason::OutputContractFailed)
        );
        assert_eq!(
            observe_selected_layout_metadata(trace.as_bytes(), true, "vaapi", "plurx_p5_contract"),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn selected_graph_metadata_rejects_stale_shuffled_missing_and_wrong_surface_frames() {
        let rows = selected_trace();
        let pipeline = Pipeline::VtDoviMetal;
        assert_eq!(
            observe_selected_metadata(&trace_bytes(&rows), pipeline, true),
            Ok(())
        );
        let mut stale = rows.clone();
        for row in &mut stale {
            row[2] = rows[0][2].clone();
        }
        let mut shuffled = rows.clone();
        shuffled[1][2] = rows[2][2].clone();
        shuffled[2][2] = rows[1][2].clone();
        let mut missing = rows.clone();
        missing[12][2].clear();
        let mut wrong_surface = rows.clone();
        wrong_surface[1][0] = wrong_surface[1][0].replace("videotoolbox_vld", "yuv420p10le");
        let mut wrong_pts = rows.clone();
        wrong_pts[1][0] = wrong_pts[1][0].replace("pts: 1024", "pts: 2048");
        for invalid in [stale, shuffled, missing, wrong_surface, wrong_pts] {
            assert_eq!(
                observe_selected_metadata(&trace_bytes(&invalid), pipeline, true),
                Err(ProbeReason::OutputContractFailed)
            );
        }
        let mut absent = rows;
        absent[0][1].clear();
        assert_eq!(
            observe_selected_metadata(&trace_bytes(&absent), pipeline, true),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn metadata_only_observer_preserves_selected_mapper_and_opaque_adapters() {
        for pipeline in [
            Pipeline::DoviStrictTonemapx,
            Pipeline::VtDoviTonemapx,
            Pipeline::DoviMetal,
            Pipeline::VtDoviMetal,
        ] {
            let production = pipeline
                .filters(Some(160), 90, Some("dovi"))
                .expect("strict mapper graph");
            let observed = observed_filter(pipeline).expect("observable strict mapper graph");
            assert_eq!(observed.replace(METADATA_OBSERVER, ""), production);
            assert_eq!(observed.matches(METADATA_OBSERVER).count(), 1);
            let tail = &observed
                [observed.find(METADATA_OBSERVER).expect("observer") + METADATA_OBSERVER.len()..];
            assert!(tail.starts_with("tonemapx=") || tail.starts_with("tonemap_videotoolbox="));
        }
    }

    fn synthetic_observed_output() -> (Value, Vec<u8>) {
        let mut raw = vec![128; 24 * 21600];
        let reference: Value = serde_json::from_slice(MANIFEST).expect("valid color oracle");
        for frame in 0..24 {
            for (patch, y) in [16, 40, 79, 170, 200, 223, 233, 235]
                .into_iter()
                .enumerate()
            {
                raw[frame * 21600 + 23 * 160 + 10 + 20 * patch] = y;
            }
            for patch in 0..6 {
                let x = 10 + 20 * patch;
                let y = 67;
                for (component, (width, offset, cx, cy)) in [
                    (160, 0, x, y),
                    (80, 14400, x / 2, y / 2),
                    (80, 18000, x / 2, y / 2),
                ]
                .into_iter()
                .enumerate()
                {
                    let bounds = &reference["color_interpretation_oracle"]["yuv420p_ranges"][patch]
                        [component];
                    let code = ((bounds[0].as_u64().expect("lower bound")
                        + bounds[1].as_u64().expect("upper bound"))
                        / 2) as u8;
                    for yy in cy - 1..=cy + 1 {
                        for xx in cx - 1..=cx + 1 {
                            raw[frame * 21600 + offset + yy * width + xx] = code;
                        }
                    }
                }
            }
        }
        let stream = serde_json::json!({"codec_name":"h264","sample_aspect_ratio":"1:1","avg_frame_rate":"12/1","field_order":"progressive","pix_fmt":"yuv420p","width":160,"height":90,"color_primaries":"bt709","color_transfer":"bt709","color_space":"bt709","color_range":"tv"});
        let frames = (0..24)
            .map(|index| {
                let mut frame = stream.clone();
                frame["best_effort_timestamp_time"] =
                    serde_json::json!((index as f64 / 12.0).to_string());
                frame
            })
            .collect::<Vec<_>>();
        (serde_json::json!({"streams":[stream],"frames":frames}), raw)
    }

    #[test]
    fn source_raster_is_the_manifest_source_shape() {
        let reference: Value = serde_json::from_slice(MANIFEST).expect("valid manifest");
        assert_eq!(
            reference["source_shape"],
            serde_json::json!([SOURCE_RASTER.0, SOURCE_RASTER.1])
        );
    }

    #[test]
    fn encoded_raster_is_checked_exactly_while_pixels_keep_the_oracle_projection() {
        let (document, raw) = synthetic_observed_output();
        let reraster = |(width, height): (u32, u32)| {
            let mut document = document.clone();
            for key in ["streams", "frames"] {
                for value in document[key].as_array_mut().expect("observed array") {
                    value["width"] = serde_json::json!(width);
                    value["height"] = serde_json::json!(height);
                }
            }
            document
        };
        let source = reraster(SOURCE_RASTER);
        assert_eq!(observe_encoded(&source, &raw, SOURCE_RASTER), Ok(()));
        // The declared raster is never rewritten to fit: a 160x90 stream is
        // not a 320x180 one, nor is the hardware-aligned 160x96 a 160x90 one.
        assert_eq!(
            observe_encoded(&document, &raw, SOURCE_RASTER),
            Err(ProbeReason::OutputContractFailed)
        );
        assert_eq!(
            observe_encoded(&reraster((160, 96)), &raw, (160, 90)),
            Err(ProbeReason::OutputContractFailed)
        );
        // The wrapper the macOS graph uses keeps its exact 160x90 contract.
        assert_eq!(
            observe(&source, &raw),
            Err(ProbeReason::OutputContractFailed)
        );
        // Pixels still decide: the same corrupt chroma fails at either raster.
        let mut corrupt = raw.clone();
        for yy in 32..=34 {
            for xx in 4..=6 {
                corrupt[12 * 21600 + 14400 + yy * 80 + xx] = 255;
            }
        }
        assert_eq!(
            observe_encoded(&source, &corrupt, SOURCE_RASTER),
            Err(ProbeReason::OutputContractFailed)
        );
        // One frame reporting a different raster fails the stream.
        let mut mixed = source.clone();
        mixed["frames"][7]["height"] = serde_json::json!(90);
        assert_eq!(
            observe_encoded(&mixed, &raw, SOURCE_RASTER),
            Err(ProbeReason::OutputContractFailed)
        );
    }

    #[test]
    fn every_colored_patch_must_match_interpretation_even_when_gray_is_unchanged() {
        let (document, raw) = synthetic_observed_output();
        assert_eq!(observe(&document, &raw), Ok(()));
        for patch in 0..6 {
            for offset in [14400, 18000] {
                let mut corrupt = raw.clone();
                let x = (10 + 20 * patch) / 2;
                let y = 67 / 2;
                for yy in y - 1..=y + 1 {
                    for xx in x - 1..=x + 1 {
                        corrupt[12 * 21600 + offset + yy * 80 + xx] = 255;
                    }
                }
                assert_eq!(
                    &corrupt[12 * 21600..12 * 21600 + 14400],
                    &raw[12 * 21600..12 * 21600 + 14400],
                    "gray/luma has not changed"
                );
                assert_eq!(
                    observe(&document, &corrupt),
                    Err(ProbeReason::OutputContractFailed),
                    "patch{patch} chroma plane{offset}"
                );
            }
        }
    }
}
