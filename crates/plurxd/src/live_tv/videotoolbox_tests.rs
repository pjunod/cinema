//! Opt-in hardware acceptance for the ATSC 1.0 live command on macOS.

use super::*;

async fn output(command: &mut tokio::process::Command) -> std::process::Output {
    command.kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("media command deadline")
        .expect("media command started");
    assert!(
        result.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

#[tokio::test]
#[ignore = "requires macOS VideoToolbox hardware access and FFmpeg; run explicitly on a Mac"]
async fn live_tv_videotoolbox_atsc1_publishes_decodable_segments() {
    let system = SystemInfo {
        ffmpeg: plurx_core::testfixtures::ffmpeg(),
        ffprobe: plurx_core::testfixtures::ffprobe(),
        ..SystemInfo::default()
    };
    // Interlaced SD (non-square pixels), progressive HD and interlaced HD.
    // Keep both original-height and downscaled 1080i routes in the matrix.
    for (width, height, interlaced, target_height, audio_channels) in [
        (720, 480, true, 480, 2),
        (1280, 720, false, 720, 6),
        (1920, 1080, true, 1080, 2),
        (1920, 1080, true, 720, 2),
    ] {
        let root = crate::test_tempdir().expect("VideoToolbox fixture root");
        let filter = match (height, interlaced) {
            (480, true) => "tinterlace=mode=interleave_top,setsar=8/9",
            (_, true) => "tinterlace=mode=interleave_top",
            _ => "null",
        };
        let mut generate = tokio::process::Command::new(&system.ffmpeg);
        generate.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={width}x{height}:rate=60000/1001"),
            "-t",
            "8",
            "-vf",
            filter,
            "-c:v",
            "mpeg2video",
            "-b:v",
            "8M",
        ]);
        if interlaced {
            generate.args(["-flags", "+ilme+ildct"]);
        }
        generate.args(["-f", "mpeg2video", "pipe:1"]);
        let elementary = output(&mut generate).await.stdout;
        // ATSC user_data_start_code + GA94 + cc_data: one valid CEA-608
        // resume-caption-loading pair. Insert before each picture's first
        // slice, after its coding extension, just as a broadcaster does.
        let captions = b"\x00\x00\x01\xb2GA94\x03\x41\xff\xfc\x94\x20\xff";
        let mut captioned = Vec::new();
        let mut previous = 0;
        for (offset, _) in elementary
            .windows(4)
            .enumerate()
            .filter(|(_, bytes)| *bytes == b"\x00\x00\x01\x01")
        {
            captioned.extend_from_slice(&elementary[previous..offset]);
            captioned.extend_from_slice(captions);
            previous = offset;
        }
        assert!(
            previous > 0,
            "generated MPEG-2 pictures must contain slices"
        );
        captioned.extend_from_slice(&elementary[previous..]);
        let elementary_path = root.path().join("captioned.m2v");
        tokio::fs::write(&elementary_path, captioned)
            .await
            .expect("caption fixture");
        let mut mux = tokio::process::Command::new(&system.ffmpeg);
        mux.args([
            "-v",
            "error",
            "-fflags",
            "+genpts",
            "-r",
            if interlaced {
                "30000/1001"
            } else {
                "60000/1001"
            },
            "-f",
            "mpegvideo",
            "-i",
        ])
        .arg(&elementary_path)
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "8",
            "-c:v",
            "copy",
            "-c:a",
            "ac3",
            "-ac",
            &audio_channels.to_string(),
            "-b:a",
            "384k",
            "-f",
            "mpegts",
            "pipe:1",
        ]);
        let source_bytes = output(&mut mux).await.stdout;
        let source_path = root.path().join("captioned.ts");
        tokio::fs::write(&source_path, &source_bytes)
            .await
            .expect("transport fixture");
        let mut probe_captions = tokio::process::Command::new(&system.ffprobe);
        probe_captions
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_frames",
                "-show_entries",
                "frame=side_data_list",
                "-of",
                "json",
            ])
            .arg(&source_path);
        let frames: serde_json::Value =
            serde_json::from_slice(&output(&mut probe_captions).await.stdout)
                .expect("decoded frame side data");
        let frames = frames["frames"].as_array().expect("decoded frames");
        assert!(!frames.is_empty());
        assert!(
            frames.iter().all(
                |frame| frame["side_data_list"].as_array().is_some_and(|data| data
                    .iter()
                    .any(|entry| entry["side_data_type"] == "ATSC A53 Part 4 Closed Captions"))
            ),
            "every decoded source frame must carry A/53 captions"
        );
        let source = probe_live_source(&system, root.path(), &source_bytes)
            .await
            .expect("probe generated ATSC 1.0 source");
        assert_eq!(source.video_codec.as_deref(), Some("mpeg2video"));
        assert_eq!(source.audio_codec.as_deref(), Some("ac3"));
        assert_eq!(source.audio_channels, Some(audio_channels));
        let delivery = crate::live_tv_delivery::resolve_live_delivery(
            &source,
            None,
            &LiveQualityPolicy {
                max_height: Some(target_height),
                max_bitrate_bps: None,
                ..LiveQualityPolicy::default()
            },
            &LiveExecutionSupport {
                video_encode: true,
                audio_encode: true,
                tone_map: false,
            },
        )
        .expect("resolve live delivery");
        assert_eq!(delivery.deinterlace, interlaced);
        let plan = LiveTvTranscodePlan::new(&system, delivery, Some(Encoder::VideoToolbox), None)
            .expect("freeze VideoToolbox plan");
        let (mut child, _job) = spawn_live_ffmpeg(&system, &plan, root.path())
            .expect("spawn production pipe-input command");
        let mut stdin = child.stdin.take().expect("live input pipe");
        let playlist_path = root.path().join("index.m3u8");
        let feed = async move {
            let written = stdin.write_all(&source_bytes).await;
            if let Err(error) = written {
                drop(stdin);
                return Err(format!("feed MPEG-TS: {error}"));
            }
            // A tuner never closes stdin. Require publication before EOF so
            // encoder/muxer flushing cannot disguise a live startup failure.
            let publication = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if tokio::fs::read_to_string(&playlist_path)
                        .await
                        .is_ok_and(|text| {
                            text.lines().filter(|line| line.ends_with(".ts")).count() >= 2
                        })
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
            drop(stdin);
            publication.map_err(|_| "live playlist did not advance before input closed".to_owned())
        };
        let (feed_result, result) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(feed, child.wait_with_output())
        })
        .await
        .expect("live encode deadline");
        let result = result.expect("live encode exit");
        assert!(
            feed_result.is_ok() && result.status.success(),
            "input result: {feed_result:?}; FFmpeg exit: {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        let playlist = tokio::fs::read_to_string(root.path().join("index.m3u8"))
            .await
            .expect("published playlist");
        assert!(!playlist.contains("#EXT-X-ENDLIST"));
        let segments: Vec<_> = playlist
            .lines()
            .filter(|line| line.ends_with(".ts"))
            .collect();
        assert!(segments.len() >= 2, "live window must advance: {playlist}");
        // Every segment must decode video. A finite input may leave a last
        // segment with a single video frame and no audio packets at the live
        // one-second cadence; do not ask FFmpeg to initialize absent audio.
        for (index, segment) in segments.iter().enumerate() {
            let path = root.path().join(segment);
            let mut probe = tokio::process::Command::new(&system.ffprobe);
            probe
                .args([
                    "-v",
                    "error",
                    "-count_packets",
                    "-show_streams",
                    "-of",
                    "json",
                ])
                .arg(&path);
            let probed = output(&mut probe).await.stdout;
            let document: serde_json::Value =
                serde_json::from_slice(&probed).expect("segment facts");
            let streams = document["streams"].as_array().expect("segment streams");
            let video = streams
                .iter()
                .find(|s| s["codec_type"] == "video")
                .expect("video");
            assert_eq!(video["codec_name"], "h264");
            assert_eq!(video["height"].as_u64(), Some(u64::from(target_height)));
            assert_eq!(video["field_order"], "progressive");
            let audio = streams.iter().find(|s| s["codec_type"] == "audio");
            let audio_packets = audio
                .and_then(|s| s["nb_read_packets"].as_str())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            if index + 1 < segments.len() {
                assert!(
                    audio_packets > 0,
                    "non-final segment {segment} must contain audio packets"
                );
            }
            if audio_packets > 0 {
                let audio = audio.expect("audio packets have a stream");
                assert_eq!(audio["codec_name"], "aac");
                assert_eq!(audio["channels"], 2);
            }
            let mut decode = tokio::process::Command::new(&system.ffmpeg);
            decode
                .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
                .arg(&path)
                .args(["-map", "0:v:0"]);
            if audio_packets > 0 {
                decode.args(["-map", "0:a:0"]);
            }
            decode.args(["-f", "null", "-"]);
            output(&mut decode).await;
        }
        eprintln!(
            "passed {width}x{height} {} + AC-3 {audio_channels}ch -> {target_height}p VideoToolbox HLS",
            if interlaced {
                "interlaced"
            } else {
                "progressive"
            }
        );
    }
}
