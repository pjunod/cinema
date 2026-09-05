use plurx_core::domain::{DolbyVisionFacts, MediaFile};
use plurx_core::transcode::{
    hls_args, Encoder, Pacing, Pipeline, PipelineDigest, Recipe, TranscodeOptions,
};
use std::process::Command;

fn reported_mpeg4(container: &str) -> MediaFile {
    MediaFile {
        id: 1,
        item_id: 1,
        path: format!("/media/episode.{container}").into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(3_358_400),
        container: Some(container.into()),
        video_codec: Some("mpeg4".into()),
        video_profile: Some("Advanced Simple Profile".into()),
        width: Some(624),
        height: Some(352),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        dolby_vision: DolbyVisionFacts::default(),
        bitrate: Some(1_599_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
    }
}

fn args_for(source: &MediaFile, encoder: Encoder, opts: &TranscodeOptions) -> Vec<String> {
    hls_args(source, encoder, opts, Pacing::unpaced(), "/tmp/issue-913")
}

fn input_options(args: &[String]) -> &[String] {
    let input = args
        .iter()
        .position(|arg| arg == "-i")
        .expect("FFmpeg arguments contain an input boundary");
    &args[..input]
}

fn has_adjacent(args: &[String], option: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && pair[1] == value)
}

fn assert_decode(args: &[String], expected: Option<&str>) {
    let before_input = input_options(args);
    match expected {
        Some(decoder) => assert!(
            has_adjacent(before_input, "-hwaccel", decoder),
            "expected -hwaccel {decoder} before -i: {args:?}"
        ),
        None => assert!(
            !before_input.iter().any(|arg| arg == "-hwaccel"),
            "software decode must not request a hardware decoder before -i: {args:?}"
        ),
    }
}

fn assert_video_encoder(args: &[String], expected: &str) {
    let input = args
        .iter()
        .position(|arg| arg == "-i")
        .expect("FFmpeg arguments contain an input boundary");
    assert!(
        has_adjacent(&args[input + 2..], "-c:v", expected),
        "expected -c:v {expected} after the input: {args:?}"
    );
}

fn videotoolbox_args(source: &MediaFile) -> Vec<String> {
    let args = args_for(source, Encoder::VideoToolbox, &TranscodeOptions::default());
    assert_video_encoder(&args, "h264_videotoolbox");
    args
}

#[test]
fn reported_mpeg4_asp_avi_software_decodes_and_videotoolbox_encodes() {
    assert_decode(&videotoolbox_args(&reported_mpeg4("avi")), None);
}

#[test]
fn mpeg4_compatibility_rule_is_container_independent() {
    for container in ["avi", "mkv", "mp4"] {
        assert_decode(&videotoolbox_args(&reported_mpeg4(container)), None);
    }
}

#[test]
fn mpeg4_compatibility_rule_does_not_depend_on_profile_or_geometry() {
    let mut source = reported_mpeg4("mkv");
    source.video_profile = Some("Simple Profile".into());
    source.width = Some(3840);
    source.height = Some(2160);
    source.bit_depth = Some(10);
    source.bitrate = Some(40_000_000);

    assert_decode(&videotoolbox_args(&source), None);
}

#[test]
fn h264_1080p_preserves_videotoolbox_decode() {
    let mut source = reported_mpeg4("mp4");
    source.video_codec = Some("h264".into());
    source.video_profile = Some("High".into());
    source.width = Some(1920);
    source.height = Some(1080);

    assert_decode(&videotoolbox_args(&source), Some("videotoolbox"));
}

#[test]
fn h264_4k_preserves_videotoolbox_decode() {
    let mut source = reported_mpeg4("mkv");
    source.video_codec = Some("h264".into());
    source.video_profile = Some("High".into());
    source.width = Some(3840);
    source.height = Some(2160);

    assert_decode(&videotoolbox_args(&source), Some("videotoolbox"));
}

#[test]
fn cropped_sdr_hevc_preserves_videotoolbox_decode() {
    let mut source = reported_mpeg4("mkv");
    source.video_codec = Some("hevc".into());
    source.video_profile = Some("Main".into());
    source.width = Some(3840);
    source.height = Some(1600);
    source.hdr = None;

    assert_decode(&videotoolbox_args(&source), Some("videotoolbox"));
}

#[test]
fn hdr_hevc_4k_preserves_videotoolbox_decode() {
    let mut source = reported_mpeg4("mkv");
    source.video_codec = Some("hevc".into());
    source.video_profile = Some("Main 10".into());
    source.width = Some(3840);
    source.height = Some(2160);
    source.bit_depth = Some(10);
    source.hdr = Some("hdr10".into());
    source.hdr_format = Some("HDR10".into());

    assert_decode(&videotoolbox_args(&source), Some("videotoolbox"));
}

#[test]
fn renderer_software_decode_requirement_still_wins() {
    let mut source = reported_mpeg4("mkv");
    source.video_codec = Some("hevc".into());
    source.video_profile = Some("Main 10".into());
    source.width = Some(3840);
    source.height = Some(2160);
    source.bit_depth = Some(10);
    source.hdr = Some("dolby_vision".into());
    let opts = TranscodeOptions {
        pipeline: Pipeline::DoviPassthrough,
        ..Default::default()
    };
    let args = args_for(&source, Encoder::VideoToolbox, &opts);

    assert_decode(&args, None);
    assert_video_encoder(&args, "libx265");
}

#[test]
fn operator_override_forces_software_decode_in_an_isolated_process() {
    const CHILD: &str = "PLURX_ISSUE_913_OVERRIDE_CHILD";
    if let Ok(value) = std::env::var(CHILD) {
        let mut source = reported_mpeg4("mp4");
        source.video_codec = Some("h264".into());
        let args = videotoolbox_args(&source);
        assert_decode(&args, None);
        assert_eq!(
            std::env::var("PLURX_HWDECODE").as_deref(),
            Ok(value.as_str())
        );
        return;
    }

    let test_binary = std::env::current_exe().expect("current test binary");
    for value in ["off", "0", "false", "no"] {
        let status = Command::new(&test_binary)
            .args([
                "--exact",
                "operator_override_forces_software_decode_in_an_isolated_process",
            ])
            .env(CHILD, value)
            .env("PLURX_HWDECODE", value)
            .status()
            .expect("run isolated override test");
        assert!(status.success(), "PLURX_HWDECODE={value} child failed");
    }
}

#[test]
fn mpeg4_compatibility_rule_does_not_change_other_encoder_families() {
    let args = args_for(
        &reported_mpeg4("avi"),
        Encoder::Nvenc,
        &TranscodeOptions::default(),
    );

    assert_decode(&args, Some("cuda"));
    assert_video_encoder(&args, "h264_nvenc");
}

#[test]
fn absent_or_unrecognized_codec_metadata_preserves_videotoolbox_decode() {
    for codec in [
        None,
        Some("mpeg4video".to_owned()),
        Some("MPEG4".to_owned()),
    ] {
        let mut source = reported_mpeg4("avi");
        source.video_codec = codec;
        assert_decode(&videotoolbox_args(&source), Some("videotoolbox"));
    }
}

fn digest(encoder: Encoder) -> PipelineDigest {
    PipelineDigest {
        ffmpeg_build: "ffmpeg version 7.1.4-Jellyfin".into(),
        encoder,
    }
}

#[test]
fn affected_recipe_key_rejects_the_pre_fix_decode_policy() {
    let source = reported_mpeg4("avi");
    let digest = digest(Encoder::VideoToolbox);
    let opts = TranscodeOptions::default();
    let corrected = Recipe {
        digest: &digest,
        file: &source,
        opts: &opts,
        audio_copied: false,
    };

    assert_ne!(
        corrected.hash(),
        "3f18ad21b86d87f8e405c20efa02a71dfc9e222df16e7cb6ac775f24cb64ea81",
        "the affected recipe still has its pre-fix identity"
    );
}

#[test]
fn unaffected_recipe_keeps_its_pre_fix_identity() {
    let mut source = reported_mpeg4("mp4");
    source.video_codec = Some("h264".into());
    let digest = digest(Encoder::VideoToolbox);
    let opts = TranscodeOptions::default();
    let corrected = Recipe {
        digest: &digest,
        file: &source,
        opts: &opts,
        audio_copied: false,
    };

    assert_eq!(
        corrected.hash(),
        "3f18ad21b86d87f8e405c20efa02a71dfc9e222df16e7cb6ac775f24cb64ea81",
        "an unaffected VideoToolbox/H.264 recipe moved"
    );
}
