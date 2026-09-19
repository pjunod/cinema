//! ATSC audio regressions: delivery decisions and decodable live HLS output.

use super::*;
use crate::live_tv_delivery::resolve_live_delivery;

fn playback(codec: &str, aac_channels: u8) -> LivePlaybackRequest {
    serde_json::from_value(serde_json::json!({
        "v": 1,
        "caps": {
            "v": 2,
            "video": [{"codec":"hevc","profiles":["main","main10"],"max_height":2160,"present":[]}],
            "audio": [codec, "aac"], "containers": ["fmp4", "mpegts"], "transports": ["hls"]
        },
        "hls_formats": [
            {"container":"fmp4","video":"hevc","audio":codec},
            {"container":"fmp4","video":"hevc","audio":"aac"}
        ],
        "video_limits": [{"codec":"hevc","max_width":3840,"max_height":2160,
            "max_frame_rate":{"num":60,"den":1},"interlaced":false}],
        "audio_limits": [{"codec":codec,"max_channels":32},{"codec":"aac","max_channels":aac_channels}]
    }))
    .expect("live playback envelope")
}

fn source(codec: &str, channels: Option<u8>) -> LiveSourceFacts {
    LiveSourceFacts {
        video_codec: Some("hevc".into()),
        video_profile: Some("main10".into()),
        width: Some(1280),
        height: Some(720),
        bit_depth: Some(10),
        field_order: Some("progressive".into()),
        audio_codec: Some(codec.into()),
        audio_sample_rate: Some(48000),
        audio_channels: channels,
        ..LiveSourceFacts::default()
    }
}

fn support() -> LiveExecutionSupport {
    LiveExecutionSupport {
        video_encode: false,
        audio_encode: true,
        tone_map: false,
    }
}

#[test]
fn atsc_aac_conversion_bounds_unknown_and_immersive_channels() {
    for (channels, limit, expected) in [
        (Some(12), 32, 6),
        (Some(12), 2, 2),
        (Some(6), 6, 6),
        (Some(1), 2, 1),
        (Some(0), 6, 2),
        (None, 6, 2),
        (Some(0), 1, 1),
    ] {
        let plan = resolve_live_delivery(
            &source("ac4", channels),
            Some(&playback("ac3", limit)),
            &LiveQualityPolicy::default(),
            &support(),
        )
        .expect("AAC route");
        assert_eq!(plan.video_action, LiveTrackAction::Copy);
        assert_eq!(plan.audio_action, LiveTrackAction::Encode);
        assert_eq!(plan.output.audio_channels, expected);
        let system = SystemInfo {
            ffmpeg: "ffmpeg".into(),
            ..SystemInfo::default()
        };
        let plan = LiveTvTranscodePlan::new(&system, plan, None, None).expect("plan");
        let command =
            live_ffmpeg_command(&system, &plan, Path::new("/fixture/live")).expect("command");
        let args: Vec<_> = command.as_std().get_args().collect();
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-ac" && pair[1] == expected.to_string().as_str()));
    }
}

#[test]
fn atsc_zero_probe_values_are_unknown_not_output_parameters() {
    let facts = parse_probe_facts(
        br#"{"streams":[
        {"codec_type":"video","codec_name":"hevc","width":1280,"height":720},
        {"codec_type":"audio","codec_name":"ac4","sample_rate":"0","channels":0}
    ]}"#,
    )
    .expect("incomplete AC-4 facts are still usable for conversion");
    assert_eq!(facts.audio_channels, None);
    assert_eq!(facts.audio_sample_rate, None);
}

#[test]
fn atsc_incomplete_audio_facts_cannot_select_copy() {
    for (channels, rate) in [
        (Some(0), Some(48000)),
        (None, Some(48000)),
        (Some(2), Some(0)),
        (Some(2), None),
    ] {
        let mut source = source("aac", channels);
        source.audio_sample_rate = rate;
        let plan = resolve_live_delivery(
            &source,
            Some(&playback("aac", 2)),
            &LiveQualityPolicy::default(),
            &support(),
        )
        .expect("conversion can wait for decoder facts");
        assert_eq!(plan.audio_action, LiveTrackAction::Encode);
        assert_eq!(plan.video_action, LiveTrackAction::Copy);
        assert_eq!(plan.output.audio_channels, 2);
    }
}

#[test]
fn atsc_ac3_fmp4_converts_audio_and_preserves_video() {
    let plan = resolve_live_delivery(
        &source("ac3", Some(6)),
        Some(&playback("ac3", 6)),
        &LiveQualityPolicy::default(),
        &support(),
    )
    .expect("safe fMP4 route");
    assert_eq!(plan.audio_action, LiveTrackAction::Encode);
    assert_eq!(plan.video_action, LiveTrackAction::Copy);
    assert_eq!(plan.output.audio_codec, "aac");
    assert_eq!(plan.output.audio_channels, 6);
    assert!(plan
        .reasons
        .iter()
        .any(|reason| reason.code == "audio_muxer_incompatible"));
}

#[test]
fn atsc_ac3_fmp4_requires_claimed_aac_and_an_encoder() {
    let source = source("ac3", Some(2));
    let mut request = playback("ac3", 2);
    let unavailable = LiveExecutionSupport {
        audio_encode: false,
        ..support()
    };
    let error = resolve_live_delivery(
        &source,
        Some(&request),
        &LiveQualityPolicy::default(),
        &unavailable,
    )
    .expect_err("no audio encoder");
    assert!(error.starts_with("audio_conversion_unavailable:"));
    request.hls_formats.retain(|format| format.audio == "ac3");
    let error = resolve_live_delivery(
        &source,
        Some(&request),
        &LiveQualityPolicy::default(),
        &support(),
    )
    .expect_err("no claimed AAC route");
    assert!(error.starts_with("client_route_unsupported:"));
}

#[test]
fn atsc_ac3_transport_stream_copy_gets_a_complete_probe_budget() {
    let mut request = playback("ac3", 2);
    for format in &mut request.hls_formats {
        format.container = "mpegts".into();
    }
    // H.264/AC-3 in MPEG-TS remains a supported copy route.
    let mut source = source("ac3", Some(2));
    source.video_codec = Some("h264".into());
    source.video_profile = Some("high".into());
    request.caps["video"][0]["codec"] = "h264".into();
    request.caps["video"][0]["profiles"] = serde_json::json!(["high"]);
    request.video_limits[0].codec = "h264".into();
    for format in &mut request.hls_formats {
        format.video = "h264".into();
    }
    let delivery = resolve_live_delivery(
        &source,
        Some(&request),
        &LiveQualityPolicy::default(),
        &support(),
    )
    .expect("MPEG-TS copy");
    assert_eq!(delivery.audio_action, LiveTrackAction::Copy);
    let system = SystemInfo {
        ffmpeg: "ffmpeg".into(),
        ..SystemInfo::default()
    };
    let plan = LiveTvTranscodePlan::new(&system, delivery, None, None).expect("plan");
    let command = live_ffmpeg_command(&system, &plan, Path::new("/fixture/live")).expect("command");
    let args: Vec<_> = command.as_std().get_args().collect();
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "-probesize" && pair[1] == "2097152"));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "-analyzeduration" && pair[1] == "2000000"));
}

#[tokio::test]
async fn atsc_audio_failures_retain_a_sanitized_cause_across_stderr_chunks() {
    for (message, expected) in [
        (
            "Unsupported channel layout \"7.1.4\"",
            "FFmpeg rejected the requested audio channel layout",
        ),
        (
            "Neither number of channels nor channel layout specified",
            "FFmpeg could not initialize live audio because the input channel layout was unknown",
        ),
        (
            "sample rate not set",
            "FFmpeg could not mux live audio because the input sample rate was unknown",
        ),
    ] {
        let (mut writer, reader) = tokio::io::duplex(1);
        let diagnostic = Arc::new(StdMutex::new(None));
        let retained = Arc::clone(&diagnostic);
        let read = capture_live_stderr(
            reader,
            Arc::new(AtomicBool::new(false)),
            retained,
            Arc::new(StdMutex::new(None)),
            Arc::new(AtomicI64::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let write = async {
            writer
                .write_all(
                    format!("Input #0, http://private.invalid/token\nStream mapping:\n{message}\n")
                        .as_bytes(),
                )
                .await
                .expect("stderr");
            drop(writer);
        };
        tokio::join!(read, write);
        let fact = *diagnostic.lock().expect("diagnostic");
        assert_eq!(fact, Some(expected));
        let error = classify_live_source_error(
            Err(LiveTvError::StreamFailed(
                "FFmpeg exited with status 234".into(),
            )),
            false,
            fact,
        )
        .expect_err("terminal failure");
        assert!(error.to_string().contains(expected));
        assert!(!error.to_string().contains("private.invalid"));
        assert!(
            classify_live_source_error(Ok(()), false, fact).is_ok(),
            "viewer cancellation is not a decoder failure"
        );
    }
}

async fn media_output(command: &mut tokio::process::Command) -> std::process::Output {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("media deadline")
        .expect("media process");
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn verify_ac3_capture(bytes: Vec<u8>) {
    let system = SystemInfo {
        ffmpeg: plurx_core::testfixtures::ffmpeg(),
        ffprobe: plurx_core::testfixtures::ffprobe(),
        ..SystemInfo::default()
    };
    let root = crate::test_tempdir().expect("live audio scratch");
    let source = probe_live_source(&system, root.path(), &bytes)
        .await
        .expect("capture probe");
    assert_eq!(source.audio_codec.as_deref(), Some("ac3"));
    let delivery = resolve_live_delivery(
        &source,
        Some(&playback("ac3", 6)),
        &LiveQualityPolicy::default(),
        &support(),
    )
    .expect("capture delivery");
    assert_eq!(delivery.video_action, LiveTrackAction::Copy);
    assert_eq!(delivery.audio_action, LiveTrackAction::Encode);
    let plan = LiveTvTranscodePlan::new(&system, delivery, None, None).expect("capture plan");
    let (mut child, _job) = spawn_live_ffmpeg(&system, &plan, root.path()).expect("live command");
    let mut stdin = child.stdin.take().expect("tuner pipe");
    let feed = async {
        let written = stdin.write_all(&bytes).await;
        let publication = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if inspect_scratch(root.path())
                    .await
                    .expect("scratch inventory")
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        drop(stdin);
        (written, publication)
    };
    let ((written, publication), result) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(feed, child.wait_with_output())
    })
    .await
    .expect("live deadline");
    let result = result.expect("live process exit");
    assert!(
        written.is_ok() && publication.is_ok() && result.status.success(),
        "live publication must precede EOF: {written:?}, {publication:?}, {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // Publication above was observed with stdin open. Close only this replay
    // manifest so an offline verifier does not poll the finished live window.
    let manifest = tokio::fs::read_to_string(root.path().join("index.m3u8"))
        .await
        .expect("live playlist");
    let playlist = root.path().join("replay.m3u8");
    tokio::fs::write(&playlist, format!("{manifest}\n#EXT-X-ENDLIST\n"))
        .await
        .expect("finite replay manifest");
    let mut probe = tokio::process::Command::new(&system.ffprobe);
    probe
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&playlist);
    let facts: serde_json::Value =
        serde_json::from_slice(&media_output(&mut probe).await.stdout).expect("HLS facts");
    let streams = facts["streams"].as_array().expect("HLS streams");
    assert!(streams.iter().any(|s| s["codec_name"] == "hevc"));
    assert!(streams
        .iter()
        .any(|s| s["codec_name"] == "aac" && s["sample_rate"] == "48000"));
    let mut decode = tokio::process::Command::new(&system.ffmpeg);
    decode
        .args(["-v", "error", "-xerror", "-nostdin", "-i"])
        .arg(&playlist)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"]);
    let decoded = media_output(&mut decode).await;
    assert!(
        decoded.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
}

#[tokio::test]
async fn atsc_delayed_ac3_produces_decodable_live_fmp4() {
    let mut generate = tokio::process::Command::new(plurx_core::testfixtures::ffmpeg());
    generate.args([
        "-v",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=128x72:rate=60",
        "-itsoffset",
        "2",
        "-f",
        "lavfi",
        "-i",
        "sine=sample_rate=48000",
        "-t",
        "10",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-x265-params",
        "keyint=60:min-keyint=60:scenecut=0:bframes=0:log-level=none:pools=1",
        "-c:a",
        "ac3",
        "-ac",
        "2",
        "-f",
        "mpegts",
        "pipe:1",
    ]);
    verify_ac3_capture(media_output(&mut generate).await.stdout).await;
}

#[tokio::test]
#[ignore = "requires the private 128.1 capture in PLURX_ATSC3_AC3_CAPTURE"]
async fn atsc_supplied_ac3_capture_produces_decodable_live_fmp4() {
    let path = std::env::var_os("PLURX_ATSC3_AC3_CAPTURE").expect("set the supplied capture path");
    verify_ac3_capture(tokio::fs::read(path).await.expect("read capture")).await;
}
