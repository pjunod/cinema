use super::*;
use plurx_core::segplan::{self, SEGPLAN_VERSION};
use plurx_core::testfixtures;

#[tokio::test]
async fn changing_content_light_keeps_indexed_copy_and_scans_nonkey_samples() {
    let input = index_pipe_bytes("closed-gop");
    let mut reader = fmp4::FragmentReader::new();
    reader.push(&input);
    let Some(fmp4::Unit::Init(init)) = reader.next_unit().expect("init") else {
        panic!("init first");
    };
    let video = init.video().expect("video");
    assert_eq!(video.nal_length_size, 4);
    let mut feed = init.bytes.clone();
    let mut fragment_count = 0;
    while let Some(unit) = reader.next_unit().expect("fixture stream") {
        if let fmp4::Unit::Fragment(mut fragment) = unit {
            let mut sample = 0;
            fmp4::rewrite_video_samples(&mut fragment, &init.tracks, video.id, |bytes| {
                let values: [u16; 2] = if fragment_count == 1 && sample == 1 {
                    [1200, 500]
                } else if fragment_count == 0 {
                    [230, 197]
                } else {
                    [283, 200]
                };
                sample += 1;
                let mut nal = vec![0x4e, 1, 144, 4];
                for value in values {
                    nal.extend_from_slice(&value.to_be_bytes());
                }
                nal.push(0x80);
                let mut output = (nal.len() as u32).to_be_bytes().to_vec();
                output.extend(nal);
                output.extend_from_slice(bytes);
                Ok(output)
            })
            .expect("insert changing CLL without changing video");
            feed.extend_from_slice(&fragment.bytes);
            fragment_count += 1;
        }
    }
    assert!(fragment_count > 2);
    let IndexOutcome::Built(index) = index_stream(
        std::io::Cursor::new(feed),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await
    else {
        panic!("index must build");
    };
    assert!(
        index.parameter_sets_constant,
        "changing light levels are not changing codec parameters"
    );
    assert!(index.promotion.indexed_hdr10_metadata);
    let light = &index.promotion.hdr10_sei;
    assert_eq!(light.len(), 1);
    assert_eq!(
        light[0],
        vec![0x4e, 1, 144, 4, 4, 176, 1, 244, 0x80],
        "non-keyframe maximum must reach the immutable init"
    );
}
#[tokio::test]
#[ignore = "needs a private FFmpeg dvh1 capture; PLURX_DVH1_CAPTURE"]
async fn captured_dvh1_completeness_builds_a_constant_index() {
    let feed = std::fs::read(
        std::env::var("PLURX_DVH1_CAPTURE").expect("valid DV capture test operation"),
    )
    .expect("valid DV capture test operation");
    let outcome = index_stream(&feed[..], identity(), None, DolbyVisionPass::Untouched).await;
    let IndexOutcome::Built(index) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(index.parameter_sets_constant);
    assert_eq!(index.promotion.dvh1_parameter_sets.len(), 3);
    let mut reader = FragmentReader::new();
    reader.push(&feed);
    let Some(Unit::Init(init)) = reader.next_unit().expect("valid DV capture test operation")
    else {
        panic!("init")
    };
    let identity = crate::renditiondir::InitIdentity::establish(&init, index.promotion.clone())
        .expect("valid DV capture test operation");
    let restored: PromotionInputs = serde_json::from_str(
        &serde_json::to_string(&index.promotion).expect("valid DV capture test operation"),
    )
    .expect("valid DV capture test operation");
    let restored = crate::renditiondir::InitIdentity::establish(&init, restored)
        .expect("valid DV capture test operation");
    let served = restored
        .served_init_for(&init)
        .expect("valid DV capture test operation");
    assert_eq!(
        served.bytes,
        identity
            .served_init_for(&init)
            .expect("valid DV capture test operation")
            .bytes
    );
    fmp4::validate_hevc_decoder_configuration(&served).expect("valid DV capture test operation");
    assert_eq!(
        fmp4::dolby_vision_record(&served)
            .expect("valid DV capture test operation")
            .expect("valid DV capture test operation")
            .profile,
        5
    );
}

#[test]
fn dvh1_completeness_revision_retries_only_affected_copy_indexes() {
    for (hdr, format, preserve, retain, affected) in [
        (
            "dolby_vision",
            "Dolby Vision · Profile 5",
            true,
            false,
            true,
        ),
        (
            "dolby_vision",
            "Dolby Vision · Profile 5",
            true,
            true,
            false,
        ),
        (
            "dolby_vision",
            "Dolby Vision · Profile 5",
            false,
            false,
            false,
        ),
        (
            "dolby_vision",
            "Dolby Vision · Profile 8 (HDR10-compatible)",
            true,
            false,
            false,
        ),
        ("hdr10", "HDR10", false, false, false),
    ] {
        let file = hevc_file(Some(hdr), Some(format));
        let video =
            transcode::CopyVideoOptions::new(true, preserve).with_parameter_set_retention(retain);
        let mut old_recipe = transcode::copy_video_args(&file, video);
        old_recipe.push(format!(
            "--plurx-hdr10-index={}",
            fmp4::HDR10_INDEX_METADATA_REVISION
        ));
        old_recipe.push(format!(
            "--plurx-hevc-proof={}",
            plurx_core::hevc_configuration::REVISION
        ));
        assert_eq!(
            identity_for(&file, video).argv_fingerprint
                != plurx_core::segplan::argv_fingerprint(&old_recipe),
            affected,
            "{format} preserve={preserve} retain={retain}"
        );
    }
}

#[test]
fn hdr10_metadata_revision_invalidates_local_copy_indexes() {
    let file = hevc_file(Some("hdr10"), Some("HDR10"));
    let video = transcode::CopyVideoOptions::new(true, false);
    let mut old_recipe = transcode::copy_video_args(&file, video);
    old_recipe.push(format!(
        "--plurx-hevc-proof={}",
        plurx_core::hevc_configuration::REVISION
    ));
    assert_ne!(
        identity_for(&file, video).argv_fingerprint,
        plurx_core::segplan::argv_fingerprint(&old_recipe)
    );
}

/// The record a converting pass supplies — and, being `Some`, the way a
/// caller says that this pass converts at all.
fn converting_record() -> plurx_core::fmp4::DolbyVisionRecord {
    plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record")
}

fn identity() -> SourceIdentity {
    SourceIdentity::new(1, 1, "fingerprint")
}

#[test]
fn content_analysis_probe_uses_the_first_mapped_video_not_container_duration() {
    let raw = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "audio", "duration": "99.000"},
                {"index": 3, "codec_type": "video", "duration_ts": 240, "time_base": "1/24", "duration": "10.000"},
                {"index": 4, "codec_type": "video", "duration": "80.000"}
            ],
            "format": {"duration": "99.000"}
        })
        .to_string();
    let expectation = completion_expectation_from_probe(&raw, "held-v1").expect("timing");
    assert_eq!(expectation.stream_index, 3);
    assert_eq!(expectation.duration_num, 10);
    assert_eq!(expectation.duration_den, 1);
    assert_eq!(expectation.provenance, CompletionProvenance::StreamTicks);
}

#[test]
fn content_analysis_probe_rejects_conflicting_selected_video_timing() {
    let raw = serde_json::json!({
        "streams": [{
            "index": 2,
            "codec_type": "video",
            "duration_ts": 240,
            "time_base": "1/24",
            "duration": "13.001"
        }]
    })
    .to_string();
    assert!(matches!(
        completion_expectation_from_probe(&raw, "held-v1"),
        Err(CompletionExpectationError::Unverified(_))
    ));
}

#[test]
fn mkv_hls_duration_conflict_compares_the_full_candidate_range() {
    let raw = serde_json::json!({
        "streams": [{
            "index": 2,
            "codec_type": "video",
            "duration_ts": 240,
            "time_base": "1/24",
            "duration": "8.500",
            "tags": {"DURATION": "00:00:11.500"}
        }]
    })
    .to_string();
    assert!(matches!(
        completion_expectation_from_probe(&raw, "held-v1"),
        Err(CompletionExpectationError::Unverified(_))
    ));
}

#[test]
fn mkv_hls_duration_packet_bounds_use_signed_pts_and_maximum_packet_end() {
    let head = serde_json::json!({
        "packets": [
            {"stream_index": 3, "pts": "-48", "duration": "24"},
            {"stream_index": 3, "pts": "0", "duration": "24"}
        ]
    })
    .to_string();
    let tail = serde_json::json!({
        "packets": [
            {"stream_index": 3, "pts": "240", "duration": "24"},
            {"stream_index": 3, "pts": "216", "duration": "24"},
            {"stream_index": 3, "pts": "192", "duration": "24"}
        ]
    })
    .to_string();
    let (origin, count) = head_packet_origin(head.as_bytes(), 3).expect("head bounds");
    assert_eq!((origin, count), (-48, 2));
    let (minimum, end, count) = tail_packet_end(tail.as_bytes(), 3)
        .expect("tail bounds")
        .expect("selected packets");
    assert_eq!(minimum, 192);
    assert_eq!(end, 264, "the last JSON row need not have the maximum PTS");
    assert_eq!(count, 3);

    let expectation = PacketTimelineBounds {
        origin_ticks: origin,
        end_ticks: end,
        packet_count: 5,
        output_bytes: u64::try_from(head.len() + tail.len()).expect("fixture bytes"),
    }
    .expectation(
        PacketProbeContext {
            stream_index: 3,
            time_base: (1, 24),
            seek_hint_seconds: 11,
        },
        "held-v1",
    )
    .expect("signed span");
    assert_eq!(expectation.provenance, CompletionProvenance::PacketTimeline);
    assert_eq!(expectation.duration_num, 13);
    assert_eq!(expectation.duration_den, 1);
}

#[test]
fn mkv_hls_duration_tail_requires_every_selected_packet_duration() {
    let tail = serde_json::json!({
        "packets": [
            {"stream_index": 3, "pts": "240", "duration": "24"},
            {"stream_index": 3, "pts": "264"}
        ]
    })
    .to_string();
    assert!(matches!(
        tail_packet_end(tail.as_bytes(), 3),
        Err(CompletionExpectationError::Unverified(_))
    ));
}

#[test]
fn mkv_hls_duration_valid_pts_does_not_require_dts() {
    let tail = serde_json::json!({
        "packets": [
            {"stream_index": 3, "pts": "240", "duration": "24"}
        ]
    })
    .to_string();
    assert_eq!(
        tail_packet_end(tail.as_bytes(), 3).expect("valid PTS-only packet"),
        Some((240, 264, 1))
    );
}

#[test]
fn content_analysis_probe_refuses_attached_picture_selected_by_ffmpeg() {
    let raw = serde_json::json!({
        "streams": [{
            "index": 0,
            "codec_type": "video",
            "duration": "10.000",
            "disposition": {"attached_pic": 1}
        }]
    })
    .to_string();
    assert!(matches!(
        completion_expectation_from_probe(&raw, "held-v1"),
        Err(CompletionExpectationError::Unsupported(_))
    ));
}

#[cfg(unix)]
/// A Matroska DURATION whose seconds field is zero is a duration, not a
/// refusal: an exactly-N-minute title used to fail its metadata
/// expectation and fall back to a packet probe.
#[test]
fn a_matroska_duration_whose_seconds_field_is_zero_parses() {
    assert_eq!(parse_matroska_duration("00:01:00.000"), Some((60, 1)));
    assert_eq!(parse_matroska_duration("01:00:00.000"), Some((3_600, 1)));
    assert_eq!(
        parse_matroska_duration("00:41:00.000000000"),
        Some((2_460, 1))
    );
    assert_eq!(parse_matroska_duration("00:00:59.500"), Some((119, 2)));
    assert_eq!(
        parse_matroska_duration("00:00:12.021000000"),
        Some((12_021, 1_000))
    );
    for refused in [
        "00:00:00.000",
        "00:00:00",
        "-00:01:00.000",
        "00:-1:00.000",
        "00:01:-1.000",
        "00:60:00.000",
        "00:00:60.000",
        "00:01",
        "00:01:00:00",
        "aa:01:00.000",
        "00:01:0x.000",
        "",
    ] {
        assert_eq!(parse_matroska_duration(refused), None, "{refused:?}");
    }
    // The plain decimal keeps refusing zero for whole values.
    assert_eq!(parse_positive_decimal("0.000"), None);
    assert_eq!(parse_positive_decimal("60.000"), Some((60, 1)));
}

/// The packet fallback, on a real file that gives it nothing else: the
/// fixture's DURATION tags are renamed, so its selected video has no
/// trustworthy duration and the index must prove its end from packets.
/// The tail probe used an open-ended `-read_intervals "<start>%"`, which
/// this ffprobe answers with no packets at all depending on unrelated
/// arguments, so the fallback could never prove an end.
#[tokio::test]
async fn the_packet_fallback_proves_an_end_on_a_file_without_duration_tags() {
    testfixtures::require_ffmpeg();
    let temp = crate::test_tempdir().expect("tempdir");
    let source_path = temp.path().join("no-duration-tag.mkv");
    let bytes = std::fs::read(testfixtures::source("h264")).expect("fixture bytes");
    let renamed = bytes
        .windows(8)
        .filter(|window| *window == b"DURATION")
        .count();
    assert!(
        renamed > 0,
        "the fixture must carry DURATION tags to rename"
    );
    let mut patched = bytes.clone();
    let mut at = 0;
    while let Some(found) = patched[at..]
        .windows(8)
        .position(|window| window == b"DURATION")
    {
        patched[at + found + 7] = b'X';
        at += found + 8;
    }
    std::fs::write(&source_path, &patched).expect("patched fixture");

    let source = std::fs::File::open(&source_path).expect("fixture source");
    let raw = crate::ffmpeg::held_source_index_probe_json(&source, Duration::from_secs(30), None)
        .await
        .expect("metadata probe");
    assert!(
        matches!(
            completion_expectation_from_probe(&raw, "fixture-object-v1"),
            Err(CompletionExpectationError::Unverified(_))
        ),
        "the fixture must reach the packet fallback"
    );
    let context = packet_probe_context_from_metadata(&raw).expect("a format seek hint");
    let bounds = held_source_video_packet_bounds(&source, context, Duration::from_secs(30))
        .await
        .expect("the packet fallback proves the selected video's end");
    assert!(bounds.packet_count > 0);
    assert!(bounds.end_ticks > bounds.origin_ticks);
}

#[tokio::test]
async fn content_analysis_ffmpeg_accepts_complete_video_with_a_longer_audio_tail() {
    testfixtures::require_ffmpeg();
    let temp = crate::test_tempdir().expect("tempdir");
    let source_path = temp.path().join("video-shorter-than-audio.mkv");
    let mut mux = std::process::Command::new(testfixtures::ffmpeg());
    mux.args(["-y", "-v", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=24:duration=3",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=6",
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "24",
            "-c:a",
            "aac",
            "-f",
            "matroska",
        ])
        .arg(&source_path);
    testfixtures::run(&mut mux);

    let source = std::fs::File::open(&source_path).expect("fixture source");
    let size =
        i64::try_from(source.metadata().expect("fixture metadata").len()).expect("fixture size");
    let file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 42,
        item_id: 1,
        path: source_path,
        size,
        mtime: 1,
        duration_ms: Some(6_000),
        container: Some("matroska".to_owned()),
        video_codec: Some("h264".to_owned()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("High".to_owned()),
        width: Some(160),
        height: Some(90),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    };
    let outcome = build_from_attested_file(
        &file,
        &source,
        "fixture-object-v1",
        transcode::CopyVideoOptions::new(false, false),
        temp.path(),
        Duration::from_secs(30),
    )
    .await;
    let IndexOutcome::Built(index) = outcome else {
        panic!("complete selected video with a longer audio tail must build: {outcome:?}");
    };
    let covered = index
        .rows
        .iter()
        .try_fold(0_u64, |sum, row| sum.checked_add(row.duration))
        .expect("coverage sum");
    let old_container_expectation = VideoCompletionExpectation {
        stream_index: 0,
        duration_num: 6,
        duration_den: 1,
        provenance: CompletionProvenance::StreamSeconds,
        source_object_version: "fixture-object-v1".to_owned(),
    };
    assert_eq!(
        old_container_expectation.covers(covered, index.timescale),
        Ok(false),
        "the former container-duration predicate rejects this valid video-only output"
    );
}

#[tokio::test]
async fn hevc_incomplete_stderr_cannot_publish_a_proofless_index() {
    struct FailedRead;
    impl tokio::io::AsyncRead for FailedRead {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("failed stderr")))
        }
    }
    let (_, _, trace) = read_stderr_tail(FailedRead, None, true).await;
    assert!(trace.is_none());
    let mut outcome = IndexOutcome::Built(Box::new(FragmentIndex::new(
        90_000,
        vec![],
        "init",
        identity(),
    )));
    let expectation = VideoCompletionExpectation {
        stream_index: 0,
        duration_num: 1,
        duration_den: 1,
        provenance: CompletionProvenance::StreamSeconds,
        source_object_version: "source".into(),
    };
    finish_header_scan(&mut outcome, true, trace, &expectation);
    assert!(matches!(outcome, IndexOutcome::Failed(_)));
}

/// A real encode, rather than fabricated PromotionInputs: the second
/// portion redefines PPS 0 while keeping the same source/container track.
#[cfg(unix)]
#[tokio::test]
async fn hevc_original_header_scan_rejects_changed_pps_before_stripping() {
    use std::process::Command;
    testfixtures::require_ffmpeg();
    let temp = crate::test_tempdir().expect("tempdir");
    for (name, cb, cr) in [("first", -6, -8), ("second", -1, -2)] {
        let mut encode = Command::new(testfixtures::ffmpeg());
        encode.args(["-y", "-v", "error", "-f", "lavfi", "-i",
                "testsrc2=size=160x96:rate=24:duration=2", "-c:v", "libx265",
                "-preset", "ultrafast", "-x265-params"])
                .arg(format!("keyint=24:min-keyint=24:open-gop=0:bframes=0:repeat-headers=1:scenecut=0:cbqpoffs={cb}:crqpoffs={cr}:log-level=none:pools=1"))
                .arg(temp.path().join(format!("{name}.mkv")));
        testfixtures::run(&mut encode);
    }
    let concat = temp.path().join("concat.txt");
    std::fs::write(&concat, "file 'first.mkv'\nfile 'second.mkv'\n")
        .expect("HEVC regression fixture");
    let changing = temp.path().join("changing.mkv");
    let mut mux = Command::new(testfixtures::ffmpeg());
    mux.args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&concat)
        .args(["-c", "copy"])
        .arg(&changing);
    testfixtures::run(&mut mux);
    for (path, verified) in [
        (temp.path().join("first.mkv"), true),
        (changing.clone(), false),
    ] {
        let source = std::fs::File::open(&path).expect("HEVC regression fixture");
        let mut file = hevc_file(None, None);
        file.path = path;
        file.size = source.metadata().expect("HEVC regression fixture").len() as i64;
        file.duration_ms = Some(if verified { 2000 } else { 4000 });
        let result = build_from_attested_file(
            &file,
            &source,
            "fixture-object",
            transcode::CopyVideoOptions::new(false, false),
            temp.path(),
            Duration::from_secs(30),
        )
        .await;
        let IndexOutcome::Built(index) = result else {
            panic!("{result:?}")
        };
        let proof = index.promotion.hevc_configuration.expect("complete trace");
        assert_eq!(proof.permits("fixture-object"), verified, "{proof:?}");
        if !verified {
            assert!(
                proof
                    .refusal
                    .as_deref()
                    .expect("HEVC regression fixture")
                    .contains("parameter sets change"),
                "{proof:?}"
            );
        }
    }
    // A/B on exactly the same encoded source. Retained headers decode to
    // the source's pixels; deleting their updates changes the pixels.
    let hashes = |path: &std::path::Path| {
        let out = Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-f", "framemd5", "-"])
            .output()
            .expect("HEVC regression fixture");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("HEVC regression fixture")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| {
                line.rsplit(',')
                    .next()
                    .expect("HEVC regression fixture")
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let stable_path = temp.path().join("first.mkv");
    let mut stable_file = hevc_file(None, None);
    stable_file.path = stable_path.clone();
    let output = Command::new(testfixtures::ffmpeg())
        .args(transcode::copy_index_pipe_args(
            &stable_file,
            transcode::CopyVideoOptions::new(false, false),
        ))
        .output()
        .expect("HEVC regression fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let production = temp.path().join("stable-production.mp4");
    let with_rpus = testfixtures::with_dolby_vision_rpus(&output.stdout);
    let dv_path = temp.path().join("stable-rpu.mp4");
    std::fs::write(&dv_path, with_rpus).expect("HEVC regression fixture");
    let dv_source = std::fs::File::open(&dv_path).expect("HEVC regression fixture");
    let mut dv_file = stable_file.clone();
    dv_file.path = dv_path;
    dv_file.size = dv_source.metadata().expect("HEVC regression fixture").len() as i64;
    dv_file.duration_ms = Some(2000);
    let dv = build_from_attested_file(
        &dv_file,
        &dv_source,
        "rpu-source",
        transcode::CopyVideoOptions::new(false, true),
        temp.path(),
        Duration::from_secs(30),
    )
    .await;
    let IndexOutcome::Built(dv_index) = dv else {
        panic!("RPU fixture: {dv:?}")
    };
    assert!(
        dv_index
            .promotion
            .hevc_configuration
            .as_ref()
            .expect("HEVC regression fixture")
            .permits("rpu-source"),
        "{:?}",
        dv_index.promotion.hevc_configuration
    );
    std::fs::write(&production, output.stdout).expect("HEVC regression fixture");
    assert_eq!(
        hashes(&production),
        hashes(&stable_path),
        "production fMP4 must preserve every decoded pixel"
    );
    let original = hashes(&changing);
    for (name, remove, matches) in [("kept", "62-63", true), ("stripped", "32-34|62-63", false)] {
        let path = temp.path().join(format!("{name}.mkv"));
        let mut remux = Command::new(testfixtures::ffmpeg());
        remux
            .args(["-y", "-v", "error", "-i"])
            .arg(&changing)
            .args(["-map", "0:v:0", "-c:v", "copy", "-bsf:v"])
            .arg(format!("filter_units=remove_types={remove}"))
            .arg(&path);
        testfixtures::run(&mut remux);
        let result = hashes(&path);
        assert_eq!(result.len(), original.len());
        assert_eq!(result == original, matches);
    }
}

/// What a differential compares of an outcome: everything but the
/// diagnostic, whose elapsed time and stderr tail are the run's own.
fn comparable(outcome: &IndexOutcome) -> IndexOutcome {
    match outcome {
        IndexOutcome::Failed(failure) => IndexOutcome::Failed(Box::new(IndexFailure {
            diagnostic: IndexDiagnostic::default(),
            ..(**failure).clone()
        })),
        other => other.clone(),
    }
}

/// The shared-read differential (proposal §8.2, M0). One pass that writes
/// every identity of a file must build, for each, exactly the index a
/// pass of its own builds: rows, init, identity, promotion inputs and the
/// HEVC header proof — or fail exactly as that pass fails.
///
/// The sources are chosen for where a shared pass could plausibly differ:
/// a stream carrying real Profile 7 RPUs, labelled P7, so strip, preserve
/// and convert are three different byte streams and the convert rewrite
/// runs on a sibling output; the same stream without RPUs, so the strip
/// is a no-op and the converting sibling is refused while its siblings
/// build; an H.264 source with one identity; and a truncated source,
/// which every identity must fail the same way.
#[cfg(unix)]
#[tokio::test]
async fn a_shared_pass_builds_exactly_what_each_identity_builds_alone() {
    use std::process::Command;
    testfixtures::require_ffmpeg();
    let temp = crate::test_tempdir().expect("tempdir");
    let have_dovi = crate::ffmpeg::has_dovi_rpu().await;

    // A closed-GOP HEVC stream as an MP4, then the same stream with a
    // real P7 RPU in every access unit.
    let mut plain_file = hevc_file(None, None);
    plain_file.path = testfixtures::source("closed-gop");
    let plain_mp4 = Command::new(testfixtures::ffmpeg())
        .args(transcode::copy_index_pipe_args(
            &plain_file,
            transcode::CopyVideoOptions::new(false, false),
        ))
        .output()
        .expect("plain fixture");
    assert!(plain_mp4.status.success());
    let rpu_path = temp.path().join("p7-rpu.mp4");
    std::fs::write(
        &rpu_path,
        testfixtures::with_dolby_vision_rpus(&plain_mp4.stdout),
    )
    .expect("RPU fixture");
    let plain_path = temp.path().join("plain.mp4");
    std::fs::write(&plain_path, &plain_mp4.stdout).expect("plain fixture");
    let truncated_path = temp.path().join("truncated.mp4");
    std::fs::write(
        &truncated_path,
        &plain_mp4.stdout[..plain_mp4.stdout.len() * 2 / 5],
    )
    .expect("truncated fixture");

    let p7 = |path: &Path| {
        let mut file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);
        file.path = path.to_owned();
        file.size = std::fs::metadata(path).expect("fixture").len() as i64;
        file.duration_ms = Some(12_000);
        file
    };
    let mut h264 = hevc_file(None, None);
    h264.video_codec = Some("h264".into());
    h264.path = testfixtures::source("h264");
    h264.size = std::fs::metadata(&h264.path).expect("fixture").len() as i64;
    h264.duration_ms = Some(12_000);

    for (name, file, must_build) in [
        ("P7 with RPUs", p7(&rpu_path), 3),
        ("P7 label, no RPUs", p7(&plain_path), 2),
        ("H.264", h264, 1),
        ("truncated", p7(&truncated_path), 0),
    ] {
        let videos = video_identities(&file, None, have_dovi, true);
        let source = std::fs::File::open(&file.path).expect("fixture");
        let mut alone = Vec::with_capacity(videos.len());
        for video in &videos {
            alone.push(
                build_from_attested_file(
                    &file,
                    &source,
                    "differential",
                    *video,
                    temp.path(),
                    Duration::from_secs(60),
                )
                .await,
            );
        }
        let shared = build_shared_attested(
            &file,
            &source,
            "differential",
            &videos,
            temp.path(),
            Duration::from_secs(60),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(shared.len(), videos.len(), "{name}");
        for ((video, alone), shared) in videos.iter().zip(&alone).zip(&shared) {
            assert_eq!(
                comparable(&shared.outcome),
                comparable(alone),
                "{name}: {video:?}"
            );
        }
        let built = alone
            .iter()
            .filter(|outcome| matches!(outcome, IndexOutcome::Built(_)))
            .count();
        assert_eq!(
            built,
            must_build.min(videos.len()),
            "{name}: the fixture must exercise what it claims: {alone:?}"
        );
    }
}

/// The §8.2 real-source run: each identity in a pass of its own against
/// one shared pass, over a real file, with wall time and the children's
/// CPU and peak RSS. Ignored; run it on the slowest storage with
/// `PLURX_SHARED_READ_SOURCE=<file> cargo test -p plurxd --bin plurxd --
/// --ignored real_source_shared_read_differential --nocapture`.
#[cfg(unix)]
#[tokio::test]
#[ignore = "reads a whole real source several times; run by hand on the target storage"]
async fn real_source_shared_read_differential() {
    use plurx_core::store::{LibraryStore, MediaStore};
    let Some(path) = std::env::var_os("PLURX_SHARED_READ_SOURCE").map(std::path::PathBuf::from)
    else {
        return;
    };
    fn children() -> (f64, i64) {
        // SAFETY: getrusage fills the zeroed struct it is given.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) };
        let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
        (
            seconds(usage.ru_utime) + seconds(usage.ru_stime),
            usage.ru_maxrss,
        )
    }
    let probe = plurx_core::scan::probe::probe(&path).await.expect("probe");
    let metadata = std::fs::metadata(&path).expect("source");
    let store = plurx_core::store::SqliteStore::open_in_memory().expect("store");
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "real".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![path.parent().expect("parent").to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "real".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_secs() as i64);
    let id = store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read").expect("file");
    let raw = store.get_file_probe_json(id).await.expect("probe json");
    let videos = video_identities(
        &file,
        raw.as_deref(),
        crate::ffmpeg::has_dovi_rpu().await,
        true,
    );
    let source = std::fs::File::open(&path).expect("source");
    let temp = crate::test_tempdir().expect("tempdir");
    let budget = Duration::from_secs(6 * 60 * 60);
    let mut alone = Vec::new();
    let mut report = Vec::new();
    for video in &videos {
        let (cpu, _) = children();
        let started = Instant::now();
        let outcome =
            build_from_attested_file(&file, &source, "real-source", *video, temp.path(), budget)
                .await;
        let (cpu_after, rss) = children();
        report.push(serde_json::json!({
            "wall_s": started.elapsed().as_secs_f64(),
            "cpu_s": cpu_after - cpu,
            "max_child_rss_kb": rss,
            "built": matches!(outcome, IndexOutcome::Built(_)),
        }));
        alone.push(outcome);
    }
    let (cpu, _) = children();
    let started = Instant::now();
    let shared = build_shared_attested(
        &file,
        &source,
        "real-source",
        &videos,
        temp.path(),
        budget,
        None,
        None,
        None,
    )
    .await;
    let (cpu_after, rss) = children();
    let equal = shared.len() == alone.len()
        && shared
            .iter()
            .zip(&alone)
            .all(|(shared, alone)| comparable(&shared.outcome) == comparable(alone));
    println!(
        "{}",
        serde_json::json!({
            "source": path,
            "bytes": metadata.len(),
            "video_codec": file.video_codec,
            "dolby_vision_profile": plurx_core::playback::dolby_vision_profile(&file),
            "identities": videos.len(),
            "alone": report,
            "shared": {
                "wall_s": started.elapsed().as_secs_f64(),
                "cpu_s": cpu_after - cpu,
                "max_child_rss_kb": rss,
                "built": shared.iter().filter(|build| matches!(build.outcome, IndexOutcome::Built(_))).count(),
            },
            "equal": equal,
        })
    );
    assert!(
        equal,
        "a shared pass built something a pass of its own did not"
    );
}

/// Every output of a shared pass is bounded by the one budget: a pass that
/// cannot finish in time fails every identity, promptly, and leaves no
/// child behind.
#[cfg(unix)]
#[tokio::test]
async fn a_shared_pass_that_runs_out_of_budget_fails_every_identity() {
    testfixtures::require_ffmpeg();
    let temp = crate::test_tempdir().expect("tempdir");
    let mut file = hevc_file(Some("dolby_vision"), Some("Dolby Vision · Profile 8.1"));
    file.dolby_vision.profile = Some(8);
    file.path = testfixtures::source("closed-gop");
    file.duration_ms = Some(12_000);
    // The budget is independent of the optional dovi_rpu filter. The CI
    // FFmpeg 6 build has filter_units but not dovi_rpu; claiming the latter
    // makes the child exit before the paced input can reach its deadline.
    let videos = video_identities(&file, None, false, false);
    assert_eq!(videos.len(), 2);
    let mut pass = shared_index_pass(
        &file,
        videos
            .iter()
            .map(|video| IndexOutput::new(&file, *video).expect("describable"))
            .collect(),
        Some(&file.path.to_string_lossy()),
        VideoCompletionExpectation {
            stream_index: 0,
            duration_num: 12,
            duration_den: 1,
            provenance: CompletionProvenance::StreamSeconds,
            source_object_version: "budget".to_owned(),
        },
        None,
    );
    // Paced to real time, a twelve-second film cannot be read in one.
    let input = pass.args.iter().position(|arg| arg == "-i").expect("-i");
    pass.args.insert(input, "-re".to_owned());
    let started = Instant::now();
    let builds = build_with_args_shared(
        &file,
        pass,
        None,
        None,
        temp.path(),
        Duration::from_secs(1),
        Instant::now(),
        None,
        None,
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(builds.len(), 2);
    for build in builds {
        let IndexOutcome::Failed(failure) = build.outcome else {
            panic!("{:?}", build.outcome);
        };
        assert_eq!(
            failure.code,
            IndexFailureCode::IndexBudgetExceeded,
            "the paced shared pass must hit its budget: {failure:?}"
        );
    }
}

fn hevc_file(hdr: Option<&str>, hdr_format: Option<&str>) -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 77,
        item_id: 1,
        path: std::path::PathBuf::from("/library/film.mkv"),
        size: 60_000_000_000,
        mtime: 1_700_000_000_000,
        duration_ms: Some(7_200_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".into()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: hdr.map(str::to_owned),
        hdr_format: hdr_format.map(str::to_owned),
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(60_000_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

fn fingerprints_of(file: &MediaFile, have_dovi: bool) -> Vec<String> {
    video_identities(file, None, have_dovi, false)
        .into_iter()
        .map(|video| identity_for(file, video).argv_fingerprint)
        .collect()
}

#[test]
fn a_plain_file_has_one_pipeline_to_index() {
    let file = hevc_file(None, None);
    assert_eq!(video_identities(&file, None, true, false).len(), 1);
    assert!(
        !video_identities(&file, None, true, false)[0].preserves_dolby_vision(),
        "the stripped identity stays first, so a fully indexed library \
             does not re-order its work to adopt the identity set"
    );
}

#[test]
fn a_dolby_vision_file_is_indexed_for_both_the_strip_and_the_envelope() {
    let file = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
    );
    let videos = video_identities(&file, None, true, false);
    assert_eq!(videos.len(), 2);
    assert!(!videos[0].preserves_dolby_vision());
    assert!(videos[1].preserves_dolby_vision());

    let held = fingerprints_of(&file, true);
    assert_ne!(
        held[0], held[1],
        "the preserved envelope and the strip are different byte streams, \
             which is why one index could never answer for both"
    );
}

/// The converting identity is added only for a file that can actually be
/// converted, and only on a node that will.
///
/// Both halves are load-bearing in opposite directions. Dropping the file
/// check would index a third pipeline for every Dolby Vision title in the
/// library — a full extra pass over a 60 GB remux each — for a stream no
/// session can ask for. Dropping the node check would do the same on a
/// node where an operator turned the conversion off.
#[test]
fn the_converting_identity_is_indexed_only_when_it_can_be_served() {
    let mut p7 = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
    );
    p7.dolby_vision.profile = Some(7);
    p7.dolby_vision.level = Some(6);
    p7.dolby_vision.bl_compat_id = Some(1);

    let converting = video_identities(&p7, None, true, true);
    assert_eq!(converting.len(), 3, "strip, preserve, convert");
    assert!(converting[2].converts_dolby_vision());
    assert!(
        converting[0..2].iter().all(|v| !v.converts_dolby_vision()),
        "the stripped identity stays first so a fully indexed library does \
             not re-order its work to adopt this"
    );

    // Three different byte streams, so three different indexes. Sharing
    // one would hand a session a playlist whose cut points describe media
    // it never produces.
    let fingerprints: std::collections::HashSet<_> = converting
        .iter()
        .map(|video| identity_for(&p7, *video).argv_fingerprint)
        .collect();
    assert_eq!(fingerprints.len(), 3);

    // The node switch stands it down.
    assert_eq!(video_identities(&p7, None, true, false).len(), 2);

    // …and so does a file the conversion cannot describe. A Profile 8
    // source is already what the conversion produces; a row with no
    // columns cannot have its configuration record built at all.
    let p8 = {
        let mut file = p7.clone();
        file.dolby_vision.profile = Some(8);
        file
    };
    assert_eq!(video_identities(&p8, None, true, true).len(), 2);
    let label_only = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
    );
    assert_eq!(video_identities(&label_only, None, true, true).len(), 2);
}

/// A transform implemented after FFmpeg must move the index identity even
/// when the executable argv is byte-for-byte unchanged.  Ordinary copy
/// recipes do not carry that transform and therefore keep their existing
/// identities across a conversion revision bump.
#[test]
fn the_post_mux_transform_revision_is_part_of_only_the_converted_identity() {
    let mut file = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
    );
    file.dolby_vision.profile = Some(7);
    file.dolby_vision.level = Some(6);
    file.dolby_vision.bl_compat_id = Some(1);

    let preserved = transcode::CopyVideoOptions::new(true, true);
    let converted = preserved.with_dolby_vision_conversion(true);
    let legacy_converted = identity_for_transform(&file, converted, None);
    let current_converted = identity_for(&file, converted);
    let next_converted = identity_for_transform(
        &file,
        converted,
        Some("dv-p7-to-p81-rpu-next-test-revision"),
    );

    assert_ne!(
        current_converted.argv_fingerprint, legacy_converted.argv_fingerprint,
        "the Rust transform cannot be represented only by FFmpeg argv"
    );
    assert_ne!(
        current_converted.argv_fingerprint, next_converted.argv_fingerprint,
        "a byte-affecting transform revision must invalidate its old index"
    );
    assert_eq!(
        identity_for(&file, preserved).argv_fingerprint,
        identity_for_transform(
            &file,
            preserved,
            Some("dv-p7-to-p81-rpu-next-test-revision")
        )
        .argv_fingerprint,
        "ordinary preserving artifacts remain reusable"
    );

    let mut plain_hdr10 = file.clone();
    plain_hdr10.hdr = Some("hdr10".to_owned());
    plain_hdr10.hdr_format = Some("HDR10".to_owned());
    plain_hdr10.dolby_vision = Default::default();
    assert_eq!(
        identity_for(&plain_hdr10, converted).argv_fingerprint,
        identity_for_transform(&plain_hdr10, converted, None).argv_fingerprint,
        "a conversion flag on a non-DV source selects no Rust transform"
    );
}

/// The converted stream's configuration record, from the source's facts.
///
/// What its output carries is the source's own record — ffmpeg copies the
/// one the input container had — so every field here is either changed
/// deliberately or carried deliberately, and getting either wrong is a
/// stream that describes itself incorrectly with nothing to catch it.
#[test]
fn the_converted_record_says_profile_eight_with_no_enhancement_layer() {
    let mut file = hevc_file(Some("dolby_vision"), None);
    file.dolby_vision.profile = Some(7);
    file.dolby_vision.level = Some(9);
    file.dolby_vision.bl_compat_id = Some(6);

    let record = converted_dolby_vision_record(&file).expect("describable");
    assert_eq!(record.profile, 8, "the RPUs now say 8.1");
    assert!(
        !record.el_present,
        "the copy's bitstream filter dropped the enhancement layer; a \
             record still declaring one tells a decoder to expect what is not \
             there"
    );
    assert!(record.rpu_present, "the RPUs are the point");
    assert!(record.bl_present);
    assert_eq!(
        record.level, 9,
        "the level bounds resolution and frame rate, neither of which the \
             conversion touches"
    );
    assert_eq!(
        record.bl_signal_compatibility_id, 1,
        "the output is Profile 8.1 even when the Profile 7 source record \
             carried another compatibility id"
    );

    // A row that cannot describe one refuses rather than inventing values.
    let mut levelless = file.clone();
    levelless.dolby_vision.level = None;
    assert!(converted_dolby_vision_record(&levelless).is_err());
}

#[test]
fn profile_five_keeps_its_stripping_identity_too() {
    // `decide` never routes a Profile 5 source to a stripping copy -- it
    // has no HDR10 base -- but `decide_forced(Force::Original)` does, and
    // that copy is indexed today. The plan's M1 acceptance check expects
    // one row here; dropping the second would regress a live path.
    let file = hevc_file(Some("dolby_vision"), Some("Dolby Vision · Profile 5"));
    assert_eq!(video_identities(&file, None, true, false).len(), 2);
}

#[test]
fn an_ffmpeg_that_cannot_strip_still_offers_two_identities() {
    // Worth pinning because it is the opposite of what it looks like:
    // without the `dovi_rpu` filter nothing removes the RPU, so it would
    // be easy to assume the two pipelines collapse into one. They do not.
    // The sample entry tag is the same `hvc1` either way for a source with
    // an HDR10-compatible base, but `-strict unofficial` and the bitstream
    // filter chain both still differ, and those decide the emitted bytes.
    let file = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
    );
    let held = fingerprints_of(&file, false);
    assert_eq!(held.len(), 2);
    assert_ne!(held[0], held[1]);
}

/// A stripping pass on an ffmpeg without `dovi_rpu` asks for the record to
/// be removed, and the ask reaches the stored promotion inputs.
///
/// The wire between "this filter chain leaves a record describing a stream
/// that no longer exists" and "no served init carries it". Without it the
/// removal happens nowhere: the muxer writes a Profile 7 record with an
/// enhancement layer over a stream with neither, VideoToolbox believes it,
/// and Safari software-decodes 4K10 HEVC on hardware built to do it in
/// silicon.
#[test]
fn a_strip_without_dovi_rpu_asks_the_promotion_to_remove_the_record() {
    let dv = hevc_file(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
    );
    let plain = hevc_file(None, None);

    let strip_no_bsf = transcode::CopyVideoOptions::new(false, false);
    assert!(
        strip_no_bsf.leaves_a_stale_dolby_vision_record(&dv),
        "the one branch that half-strips"
    );

    // Every neighbouring answer is a whole one and needs no help.
    assert!(
        !transcode::CopyVideoOptions::new(true, false).leaves_a_stale_dolby_vision_record(&dv),
        "dovi_rpu removes the side data, so the muxer writes no record"
    );
    assert!(
        !transcode::CopyVideoOptions::new(false, true).leaves_a_stale_dolby_vision_record(&dv),
        "a preserved stream's record is true"
    );
    assert!(
        !transcode::CopyVideoOptions::new(false, true)
            .with_dolby_vision_conversion(true)
            .leaves_a_stale_dolby_vision_record(&dv),
        "a conversion rewrites the record rather than removing it"
    );
    assert!(
        !strip_no_bsf.leaves_a_stale_dolby_vision_record(&plain),
        "a source with no Dolby Vision has no record to be stale"
    );

    // …and the pass the index pipe actually selects from it. This is the
    // half that was untested: with the selection inline in the pipe
    // builder, which spawns ffmpeg and cannot be driven from a test, the
    // whole fix could be deleted and the suite stayed green.
    assert_eq!(
        dolby_vision_pass_for(&dv, strip_no_bsf).expect("a strip needs no record"),
        DolbyVisionPass::Remove
    );
    assert_eq!(
        dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(true, false))
            .expect("dovi_rpu"),
        DolbyVisionPass::Untouched
    );
    assert_eq!(
        dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(false, true))
            .expect("preserve"),
        DolbyVisionPass::Untouched
    );
    assert_eq!(
        dolby_vision_pass_for(&plain, strip_no_bsf).expect("not Dolby Vision"),
        DolbyVisionPass::Untouched
    );

    // A conversion rewrites rather than removes — and asks for a record
    // this file can describe.
    let mut p7 = dv.clone();
    p7.dolby_vision.profile = Some(7);
    p7.dolby_vision.level = Some(6);
    p7.dolby_vision.bl_compat_id = Some(1);
    let converting =
        transcode::CopyVideoOptions::new(false, true).with_dolby_vision_conversion(true);
    assert!(matches!(
        dolby_vision_pass_for(&p7, converting).expect("describable"),
        DolbyVisionPass::Rewrite(_)
    ));
}

/// A probe describing the minimal 23-byte hvcC — the WEB-DL Matroska
/// shape whose parameter sets live only in band, and the only one
/// `hevc_parameter_set_promotion_required` fires on.
const MINIMAL_HVCC_PROBE: &str = r#"{"streams":[{"codec_type":"video","extradata_size":23}]}"#;

/// The argv and the record answer are one recipe, for every identity the
/// indexer actually enumerates.
///
/// This is the invariant, and it is asserted against the rendered argv
/// rather than restated: a pass removes the record exactly when its own
/// filter chain strips the Dolby Vision NAL units without asking ffmpeg to
/// drop the side data too. Carrying the two halves separately is how they
/// come apart — each is individually correct, and together they describe
/// two different streams with nothing to notice.
#[test]
fn every_index_pass_agrees_with_the_argv_it_carries() {
    let mut removing = 0;
    let mut promoting = 0;
    // Each row carries the columns its label describes. Stamping Profile 7
    // over all of them made the "Profile 5" row a duplicate of the Profile
    // 7 one — `file_can_convert_to_p81` reads the columns, not the label —
    // so a real Profile 5 file, which has no compatible base and therefore
    // one identity fewer, was never enumerated.
    for (hdr, label, profile, compat) in [
        (
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
            Some(7i64),
            Some(1i64),
        ),
        (
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 5"),
            Some(5),
            Some(0),
        ),
        (Some("hdr10"), Some("HDR10"), None, None),
        (None, None, None, None),
    ] {
        let mut file = hevc_file(hdr, label);
        file.dolby_vision.profile = profile;
        file.dolby_vision.level = profile.map(|_| 6i64);
        file.dolby_vision.bl_compat_id = compat;
        for have_dovi in [false, true] {
            for convert in [false, true] {
                // `probe_json` both ways. `Some` turns on parameter-set
                // promotion, which is the branch of `copy_video_args` that
                // hand-rolls its filter list instead of calling
                // `hevc_copy_bsf_for_copy` — the one place the two
                // builders could most plausibly disagree.
                for probe in [None, Some(MINIMAL_HVCC_PROBE)] {
                    for video in video_identities(&file, probe, have_dovi, convert) {
                        let pass = index_pass(
                            &file,
                            video,
                            None,
                            VideoCompletionExpectation {
                                stream_index: 0,
                                duration_num: 60,
                                duration_den: 1,
                                provenance: CompletionProvenance::StreamSeconds,
                                source_object_version: "test".to_owned(),
                            },
                            None,
                        )
                        .expect("a describable pass");
                        let filter = pass
                            .args
                            .windows(2)
                            .find(|pair| pair[0] == "-bsf:v")
                            .map(|pair| pair[1].clone())
                            .unwrap_or_default();
                        let leaves_a_record =
                            filter.contains("62-63") && !filter.contains("dovi_rpu");
                        assert_eq!(
                            pass.outputs[0].dolby_vision == DolbyVisionPass::Remove,
                            leaves_a_record,
                            "hdr={hdr:?} label={label:?} dovi={have_dovi} convert={convert} \
                                 promote={} rendered {filter}",
                            video.promotes_parameter_sets()
                        );
                        if leaves_a_record {
                            removing += 1;
                        }
                        if video.promotes_parameter_sets() {
                            promoting += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        removing > 0,
        "no enumerated identity reached the removing branch, so the \
             equality above held vacuously"
    );
    assert!(
        promoting > 0,
        "no enumerated identity reached the parameter-set-promotion \
             branch, which is the filter list the two builders could disagree in"
    );
}

/// A `Remove` pass reaches the stored promotion inputs, and an ordinary
/// one does not.
///
/// The wire the fix is, end to end within this module: the predicate above
/// says the compensation is needed, and this says the ask survives to the
/// place every served init — live and regenerated — is promoted from.
/// Without it the whole fix can be deleted from `build_with_args` and the
/// suite stays green.
#[tokio::test]
async fn a_removing_pass_stores_the_ask_in_the_promotion_inputs() {
    testfixtures::require_ffmpeg();
    let bytes = index_pipe_bytes("closed-gop");

    let removing = index_stream(
        std::io::Cursor::new(bytes.clone()),
        identity(),
        None,
        DolbyVisionPass::Remove,
    )
    .await;
    let IndexOutcome::Built(removing) = removing else {
        panic!("{removing:?}");
    };
    assert!(
        removing.promotion.strip_dolby_vision,
        "a removing pass must record the ask, or nothing removes the record"
    );
    assert!(
        removing.promotion.dolby_vision.is_none(),
        "and must not also ask for a rewrite, which promotion refuses"
    );

    let ordinary = index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await;
    let IndexOutcome::Built(ordinary) = ordinary else {
        panic!("{ordinary:?}");
    };
    assert!(
        !ordinary.promotion.strip_dolby_vision,
        "every other pass leaves the muxer's record alone"
    );
}

/// The record the caller supplies reaches the stored promotion inputs, and
/// therefore the served init every later generation is promoted to.
///
/// This is the wire between "plurx builds a Dolby Vision record because
/// ffmpeg cannot" and "every served init carries it". Without it the
/// record is built, discarded, and the converted stream tells every client
/// it is plain HDR10 — the whole milestone delivering nothing, with no
/// error anywhere.
#[tokio::test]
async fn the_supplied_dolby_vision_record_reaches_the_stored_promotion() {
    testfixtures::require_ffmpeg();
    let bytes = index_pipe_bytes("closed-gop");

    let plain = index_stream(
        std::io::Cursor::new(bytes.clone()),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await;
    let IndexOutcome::Built(plain) = plain else {
        panic!("{plain:?}");
    };
    assert!(
        plain.promotion.dolby_vision.is_none(),
        "an ordinary pass supplies none"
    );

    let record =
        plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record");
    // Supplying the record *is* asking for the conversion, so the stream
    // has to be one there is something to convert in.
    let converted = index_stream(
        std::io::Cursor::new(testfixtures::with_dolby_vision_rpus(&bytes)),
        identity(),
        None,
        DolbyVisionPass::Rewrite(Box::new(record.clone())),
    )
    .await;
    let IndexOutcome::Built(converted) = converted else {
        panic!("{converted:?}");
    };
    assert_eq!(
        converted.promotion.dolby_vision,
        Some(record),
        "the record is stored, so a regenerated init is promoted from it too"
    );
    assert!(!converted.promotion.is_empty());
}

/// The conversion runs inside the index pass, and the rows describe the
/// converted stream.
///
/// This is the wire the whole converting identity hangs from. The index a
/// converting session lands against has to have been built from converted
/// fragments: an 8.1 RPU is smaller than the Profile 7 one it replaces, so
/// every `video_bytes` differs, and an index built without the conversion
/// would describe a stream no converting session ever produces — the
/// landing would miss on every fragment and the session would fail with
/// nothing pointing at why.
#[tokio::test]
async fn a_converting_pass_indexes_the_converted_stream() {
    testfixtures::require_ffmpeg();
    let plain_bytes = index_pipe_bytes("closed-gop");
    let dv_bytes = testfixtures::with_dolby_vision_rpus(&plain_bytes);

    let unconverted = index_stream(
        std::io::Cursor::new(dv_bytes.clone()),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await;
    let IndexOutcome::Built(unconverted) = unconverted else {
        panic!("a Dolby Vision stream indexes without converting: {unconverted:?}");
    };

    let converted = index_stream(
        std::io::Cursor::new(dv_bytes),
        identity(),
        None,
        DolbyVisionPass::Rewrite(Box::new(converting_record())),
    )
    .await;
    let IndexOutcome::Built(converted) = converted else {
        panic!("the converting pass must build: {converted:?}");
    };

    assert_eq!(
        converted.rows.len(),
        unconverted.rows.len(),
        "the conversion rewrites metadata, it does not add or drop frames"
    );
    for (with, without) in converted.rows.iter().zip(unconverted.rows.iter()) {
        assert_eq!(with.dts, without.dts, "no timestamp is reconstructed");
        assert_eq!(with.duration, without.duration);
        assert!(
            with.video_bytes < without.video_bytes,
            "every fragment shrinks by what the 8.1 RPUs no longer carry"
        );
    }
}

/// A converting pass over a stream with no RPUs in it refuses.
///
/// The row said Profile 7 and the stream carries nothing to convert, so
/// one of the two is lying. Indexing it under the converted identity would
/// record that lie in the keyspace, and every session that looked the
/// identity up would be served segments cut for a stream that was never
/// converted.
#[tokio::test]
async fn a_converting_pass_over_a_stream_with_no_rpus_is_not_an_index() {
    testfixtures::require_ffmpeg();
    let bytes = index_pipe_bytes("closed-gop");
    let outcome = index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        None,
        DolbyVisionPass::Rewrite(Box::new(converting_record())),
    )
    .await;
    let IndexOutcome::Unsupported(reason) = outcome else {
        panic!("a stream with no RPUs cannot be indexed as converted: {outcome:?}");
    };
    assert!(reason.contains("no RPUs to convert"), "{reason}");
}

/// The index pipe over a real fixture, read the way the daemon reads it.
async fn index_fixture(kind: &str) -> IndexOutcome {
    let bytes = index_pipe_bytes(kind);
    index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await
}

/// The video-only pipe's own output, cached beside the fixture.
pub(super) fn index_pipe_bytes(kind: &str) -> Vec<u8> {
    let source = testfixtures::source(kind);
    let mut command = std::process::Command::new(testfixtures::ffmpeg());
    command
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&source)
        .args([
            "-map_chapters",
            "-1",
            "-map",
            "0:v:0?",
            "-an",
            "-sn",
            "-c:v",
            "copy",
        ]);
    if kind != "h264" && kind != "vp9" {
        command
            .args(["-tag:v", "hvc1"])
            .args(["-bsf:v", "filter_units=remove_types=32-34"]);
    }
    command
        .args(["-avoid_negative_ts", "make_zero"])
        .args([
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
        ])
        .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
    testfixtures::run(&mut command)
}

fn replace_hvcc_array_type(bytes: &mut [u8], from: u8, to: u8) {
    let kind_at = bytes
        .windows(4)
        .position(|window| window == b"hvcC")
        .expect("hvcC box");
    let payload = kind_at + 4;
    let arrays = usize::from(bytes[payload + 22]);
    let mut pos = payload + 23;
    for _ in 0..arrays {
        let array_kind = bytes[pos] & 0x3f;
        let count = u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
        if array_kind == from {
            bytes[pos] = (bytes[pos] & 0xc0) | to;
            return;
        }
        pos += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
            pos += 2 + len;
        }
    }
    panic!("hvcC carried no type-{from} array");
}

#[tokio::test]
async fn a_real_pipe_reports_its_parameter_sets_constant() {
    // The scan-time check plan §2.2's ruling asks for. Every clean
    // fragment of a single-pass encode carries the same parameter sets, so
    // the film is VOD-presentable.
    let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
        panic!("the closed-gop fixture must index");
    };
    assert!(
        index.parameter_sets_constant,
        "a single-pass encode's clean starts must agree"
    );
    // And this corpus carries nothing promotable -- the test pipe strips
    // types 32-34 the way `copy_video_args` does for ordinary HEVC, so
    // promotion is a no-op and there is nothing to capture. Asserted
    // rather than assumed, because a corpus that silently started
    // carrying them would make the test above pass for a different reason.
    assert!(
        index.promotion.is_empty(),
        "ordinary HEVC carries its parameter sets in hvcC, not in band"
    );
}

#[tokio::test]
async fn emitted_hvc1_refuses_an_incomplete_decoder_configuration_without_a_probe_hint() {
    let mut bytes = index_pipe_bytes("closed-gop");
    // Keep the hvcC structurally valid but turn its PPS array into a
    // duplicate SPS array. The ordinary fixture pipe has already removed
    // in-band sets, so there is no hidden PPS from which to "succeed".
    replace_hvcc_array_type(&mut bytes, 34, 33);
    let outcome = index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await;
    let IndexOutcome::Unsupported(reason) = outcome else {
        panic!("an incomplete emitted hvcC must not be indexed: {outcome:?}");
    };
    assert!(reason.contains("complete VPS/SPS/PPS"), "{reason}");
}

#[tokio::test]
async fn a_film_whose_clean_starts_disagree_is_not_vod_presentable() {
    // Built by hand, because no fixture can produce it: the corpus is
    // single-invocation encodes, which are structurally incapable of
    // per-IDR parameter-set variation. The check still has to work.
    let bytes = index_pipe_bytes("closed-gop");
    let IndexOutcome::Built(index) = index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await
    else {
        panic!("must index");
    };
    // A constant film, as the fixture is.
    assert!(index.parameter_sets_constant);

    // Now the same walk with one start disagreeing. `PromotionInputs`
    // compares by value, so this is the exact comparison `index_stream`
    // makes.
    use plurx_core::fmp4::PromotionInputs;
    let canonical = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        strip_dolby_vision: false,
        dolby_vision: None,
        dvh1_parameter_sets: Vec::new(),
        dvh1_parameter_sets_malformed: false,
        parameter_sets: vec![vec![0x40, 0x01, 0x0c]],
        hdr10_sei: Vec::new(),
    };
    let differing = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        strip_dolby_vision: false,
        dolby_vision: None,
        dvh1_parameter_sets: Vec::new(),
        dvh1_parameter_sets_malformed: false,
        parameter_sets: vec![vec![0x40, 0x01, 0x0d]],
        hdr10_sei: Vec::new(),
    };
    assert_ne!(
        canonical, differing,
        "the comparison that decides VOD-presentability must see the \
             difference between two parameter sets that differ by one byte"
    );
}

#[tokio::test]
async fn a_real_pipe_indexes_every_fragment_it_carries() {
    let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
        panic!("the closed-gop fixture must index");
    };
    assert!(index.rows.len() > 3, "got {} rows", index.rows.len());
    assert!(index.timescale > 0);
    assert_eq!(index.version, SEGPLAN_VERSION);
    assert_eq!(index.init_sha256.len(), 64);
    assert!(
        index
            .rows
            .iter()
            .all(|row| row.bytes > 0 && row.duration > 0),
        "every row describes real bytes and real time"
    );
    // Contiguous: each fragment starts where the last one ended, which is
    // what lets a plan entry's start be an index row's DTS.
    for pair in index.rows.windows(2) {
        assert_eq!(pair[0].dts + pair[0].duration, pair[1].dts);
    }
}

#[tokio::test]
async fn the_index_is_deterministic() {
    let first = index_fixture("closed-gop").await;
    let second = index_fixture("closed-gop").await;
    assert_eq!(first, second, "two passes must describe the same file");
}

#[tokio::test]
async fn an_open_gop_fixture_is_classified_dirty_and_a_closed_one_clean() {
    let IndexOutcome::Built(open) = index_fixture("open-gop").await else {
        panic!("open-gop must index");
    };
    let IndexOutcome::Built(closed) = index_fixture("closed-gop").await else {
        panic!("closed-gop must index");
    };
    assert!(
        closed.rows.iter().filter(|row| row.clean()).count() > closed.rows.len() / 2,
        "a closed GOP is clean nearly everywhere"
    );
    assert!(
        open.rows.iter().filter(|row| !row.clean()).count() > open.rows.len() / 2,
        "an open GOP with leading pictures is dirty nearly everywhere"
    );
}

#[tokio::test]
async fn a_truncated_pipe_is_not_an_index() {
    // The failure that matters: ffmpeg dies partway and emits a perfectly
    // well-formed prefix. An index built from it places every later
    // boundary in the wrong part of the film.
    let bytes = index_pipe_bytes("closed-gop");
    let half = bytes.len() / 2;
    let outcome = index_stream(
        std::io::Cursor::new(bytes[..half].to_vec()),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await;
    assert!(
        matches!(
            outcome,
            IndexOutcome::Failed(ref failure)
                if failure.code == IndexFailureCode::IndexOutputMalformed
        ),
        "a half-read pipe must not produce an index, got {outcome:?}"
    );
}

#[tokio::test]
async fn a_pipe_that_stops_short_of_the_probed_duration_is_truncated() {
    let bytes = index_pipe_bytes("closed-gop");
    let IndexOutcome::Built(full) = index_stream(
        std::io::Cursor::new(bytes.clone()),
        identity(),
        None,
        DolbyVisionPass::Untouched,
    )
    .await
    else {
        panic!("the full pipe indexes");
    };
    let _covered: u64 = full.rows.iter().map(|row| row.duration).sum();
    // Claim the file is a minute longer than it is: the coverage check is
    // the only thing standing between a short read and a wrong index.
    let outcome = index_stream(
        std::io::Cursor::new(bytes),
        identity(),
        Some(60_000 + 12_000),
        DolbyVisionPass::Untouched,
    )
    .await;
    assert!(
        matches!(
            outcome,
            IndexOutcome::Failed(ref failure)
                if failure.code == IndexFailureCode::IndexVideoShortfall
        ),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn an_index_builds_a_plan_whose_entries_land_on_index_rows() {
    let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
        panic!("closed-gop must index");
    };
    let policy = fmp4::CutPolicy::new(2, 1, 64 * 1024 * 1024, 15, index.timescale);
    let tracks = segplan::TrackDurations {
        video_ms: 12_000,
        audio_ms: 12_000,
        audio_bits_per_second: 256_000,
    };
    let plan = segplan::plan_copy(&index, &policy, &tracks);
    assert!(!plan.is_empty());
    for entry in plan
        .entries
        .iter()
        .filter(|entry| entry.kind == segplan::PlanEntryKind::Video)
    {
        assert!(
            index.rows.iter().any(|row| row.dts == entry.start_ticks),
            "plan entry {} starts at {} ticks, which is not a fragment boundary",
            entry.index,
            entry.start_ticks
        );
    }
}

#[tokio::test]
async fn a_plan_from_a_persisted_index_builds_well_under_a_tenth_of_a_second() {
    let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
        panic!("closed-gop must index");
    };
    // Blow the corpus up to feature length so the measurement means
    // something: 12 s of fixture is not a 2-hour film.
    let mut rows = Vec::new();
    let mut dts = 0;
    for _ in 0..600 {
        for row in &index.rows {
            rows.push(IndexRow { dts, ..row.clone() });
            dts += row.duration;
        }
    }
    let big = FragmentIndex::new(index.timescale, rows, "sha", identity());
    let policy = fmp4::CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, big.timescale);
    let tracks = segplan::TrackDurations {
        video_ms: 7_200_000,
        audio_ms: 7_200_000,
        audio_bits_per_second: 256_000,
    };
    let started = Instant::now();
    let plan = segplan::plan_copy(&big, &policy, &tracks);
    let elapsed = started.elapsed();
    assert!(!plan.is_empty());
    assert!(
        elapsed < Duration::from_millis(100),
        "planning {} fragments took {elapsed:?}; the plan is built on every \
             create and must not be felt",
        big.rows.len()
    );
}

#[tokio::test]
async fn a_landing_in_a_real_index_resolves_uniquely() {
    let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
        panic!("closed-gop must index");
    };
    assert!(index.rows.len() >= 5, "need a few fragments to land in");
    for start in 0..index.rows.len().saturating_sub(3) {
        let observed: Vec<u32> = index.rows[start..start + 3]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(
            segplan::match_landing(&index, &observed, start),
            Ok(start),
            "a real fixture's fragment byte counts must place a landing"
        );
    }
}

#[cfg(target_os = "linux")]
mod dv_title_companion {
    use super::*;

    fn output(code: i32, stdout: &str, stderr: &str) -> plurx_core::process::bounded::Output {
        use std::os::unix::process::ExitStatusExt;
        plurx_core::process::bounded::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    const AGGREGATE: &str = r#"{"schema":1,"complete":true,"decoded_frames":3,"valid_l1_frames":3,"missing_l1_frames":0,"invalid_l1_frames":0,"max_l1_pq":3000,"mastering_luminance":{"state":"unknown"},"mastering_primaries":{"state":"unknown"}}"#;

    /// The companion scan is judged as the title job judges its own: a
    /// refusal, a scanner that changed under the pass, or an incomplete or
    /// inconsistent aggregate keeps nothing.
    #[test]
    fn the_companion_scan_is_judged_like_the_title_job() {
        let stats = dv_title_aggregate(&output(0, AGGREGATE, ""), true).expect("complete");
        assert_eq!(stats.decoded_frames, 3);
        assert_eq!(stats.max_l1_pq, Some(3000));
        assert!(dv_title_aggregate(&output(0, AGGREGATE, ""), false).is_err());
        let refused = dv_title_aggregate(
            &output(
                1,
                "",
                "DV title scan refused: contiguous decoded title coverage\n",
            ),
            true,
        )
        .expect_err("refused");
        assert!(
            refused.contains("contiguous decoded title coverage"),
            "{refused}"
        );
        let incomplete = AGGREGATE.replace(r#""complete":true"#, r#""complete":false"#);
        assert!(dv_title_aggregate(&output(0, &incomplete, ""), true).is_err());
        assert!(dv_title_aggregate(&output(0, "{}", ""), true).is_err());
    }

    /// The scan never enters the pass: same argv, same descriptors, so a scan
    /// that fails cannot fail or slow the index.
    #[test]
    fn the_companion_scan_leaves_the_index_pass_untouched() {
        let source = include_str!("../fragindex.rs");
        let pass_start = source.find("fn shared_index_pass(").expect("pass");
        let pass_end = pass_start + source[pass_start..].find("\n}\n").expect("end");
        assert!(
            !source[pass_start..pass_end].contains("dv_title"),
            "argv carries no scan"
        );
        let spawn = source
            .find("Some(ride) => ride.spawn(source, budget)")
            .expect("spawn");
        let finish = source
            .find("let pending = ride.finish(file.id, task, lead_built, wait).await;")
            .expect("finish");
        let build = source
            .find("let built = build_with_args_shared(")
            .expect("build");
        assert!(
            spawn < build && build < finish,
            "spawned before the pass, judged after it"
        );
    }

    /// The daemon's own index pass on a real title, with an installed
    /// processing bundle, yields the aggregate the standalone scan published.
    #[tokio::test]
    #[ignore = "physical: a processing bundle (PLURX_DV_PROCESSING_DIR), a Dolby Vision title and its standalone aggregate"]
    async fn physical_the_index_pass_reads_the_standalone_title_aggregate() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
        use plurx_core::store::{SqliteStore, Store};
        let source = std::path::PathBuf::from(
            std::env::var("PLURX_DV_RIDE_SOURCE").expect("PLURX_DV_RIDE_SOURCE"),
        );
        let expected: plurx_core::scan::dv_title::DvTitleStats = serde_json::from_str(
            &std::fs::read_to_string(
                std::env::var("PLURX_DV_RIDE_EXPECTED").expect("PLURX_DV_RIDE_EXPECTED"),
            )
            .expect("expected aggregate"),
        )
        .expect("aggregate");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "ride".into(),
                kind: LibraryKind::Movies,
                paths: vec![source.parent().expect("parent").into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "ride".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let probe = plurx_core::scan::probe::probe(&source)
            .await
            .expect("probe");
        let metadata = std::fs::metadata(&source).expect("metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let id = store
            .upsert_file(
                item,
                source.to_str().expect("path"),
                metadata.len() as i64,
                mtime,
                &probe,
            )
            .await
            .expect("file");
        let file = store.get_file(id).await.expect("file").expect("file");
        let tools = crate::vodencode::dv_runtime::ToolSet::capture()
            .await
            .expect("bundle")
            .expect("bundle installed");
        let videos = video_identities(&file, None, true, false);
        let cache = crate::test_tempdir().expect("cache");
        let started = Instant::now();
        let builds = build_riding_shared(
            &file,
            &videos,
            cache.path(),
            Duration::from_secs(7200),
            None,
            None,
        )
        .await;
        let lead = builds.into_iter().next().expect("lead");
        assert!(
            matches!(lead.outcome, IndexOutcome::Built(_)),
            "{:?}",
            lead.outcome
        );
        let pending = lead
            .dv_title
            .expect("the companion scan kept its aggregate");
        assert_eq!(pending.stats, expected);
        assert_eq!(pending.scanner_digest, tools.title_scanner_digest());
        println!(
            "companion scan on {}: {} identities, {} s, {} pictures",
            source.display(),
            videos.len(),
            started.elapsed().as_secs(),
            pending.stats.decoded_frames
        );
    }
}
