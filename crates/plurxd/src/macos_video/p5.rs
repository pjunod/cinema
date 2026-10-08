//! Hash-bound, current-access-unit Dolby controls for the existing probe owner.
use super::*;
use plurx_core::transcode::{EffectiveRateControl, Encoder, OutputGrade, Pipeline, VideoCodec};

pub(super) const MANIFEST: &[u8] = include_bytes!("../../fixtures/macos-processing/strict-p5.json");
macro_rules! media {
    ($($name:literal),+ $(,)?) => { &[$(($name, include_bytes!(concat!("../../fixtures/macos-processing/strict_p5_", $name, ".mp4")) as &[u8])),+] };
}
pub(super) const MEDIA: &[(&str, &[u8])] = media!(
    "fresh",
    "variable",
    "missing_first",
    "missing_midstream",
    "missing_seek_start",
    "malformed_first",
    "malformed_midstream",
    "omitted_color_first",
    "omitted_color_midstream"
);

pub(super) fn add_work(
    corpus: &Corpus,
    work: &mut Vec<(Fixture, SmokeOperation, MacosProcessingGraph)>,
) -> Result<(), ProbeReason> {
    let invalid = ProbeReason::InvalidEmbeddedCorpus;
    let doc: Value = serde_json::from_slice(MANIFEST).map_err(|_| invalid)?;
    let cases = doc["cases"].as_array().ok_or(invalid)?;
    if doc["schema_version"] != 1
        || doc["generator_recipe_version"] != 1
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
            || (matches!(*id, "fresh" | "variable") != case["negative_max_frames"].is_null())
        {
            return Err(invalid);
        }
        let expected_maximum = match *id {
            "fresh" | "variable" => None,
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

fn observe_negative(
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

fn observe(document: &Value, raw: &[u8]) -> Result<(), ProbeReason> {
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
        if frame["width"] != 160 || frame["height"] != 90 {
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
    Ok(())
}

fn observe_source_pixels(raw: &[u8]) -> Result<(), ProbeReason> {
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

fn observe_source(document: &Value, variable: bool) -> Result<(), ProbeReason> {
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
        &["tonemap_videotoolbox", "scale_vt"][..]
    } else {
        &["tonemapx", "scale"][..]
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
        "warning",
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
    let mut filter = pipeline
        .filters(Some(160), 90, Some("dovi"))
        .ok_or(ProbeReason::GraphFailed)?;
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
    let encoded = bounded_probe_command(command, deadline, CORPUS_BUDGET as u64, cancelled).await?;
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
        assert_eq!(work.len(), 45);
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
                    18
                } else {
                    9
                }
            );
            assert!(controls
                .iter()
                .any(|(f, _, _)| f.input_pixels["seek_seconds"] == 1));
            assert!(controls
                .iter()
                .any(|(f, _, _)| f.input_pixels["id"] == "omitted_color_midstream"));
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
}
