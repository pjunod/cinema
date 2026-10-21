use super::testing::*;
use super::*;
use crate::fragindex::{IndexBuild, IndexOutcome};
use crate::subtitle_source::testing::{settled as settled_entry, stamp_of, write_manifest};
use crate::subtitle_source::{Live, Lookup, TRANSIENT_ATTEMPTS};
use plurx_core::store::ClusterFragmentIndexStore;

const VIDEO: plurx_core::transcode::CopyVideoOptions =
    plurx_core::transcode::CopyVideoOptions::new(false, false);

fn probe(streams: &[(u64, &str, &str)]) -> String {
    serde_json::json!({
        "streams": streams
            .iter()
            .map(|(index, kind, codec)| serde_json::json!({
                "index": index, "codec_type": kind, "codec_name": codec,
            }))
            .collect::<Vec<_>>()
    })
    .to_string()
}

// -- ordinals --------------------------------------------------------

#[test]
fn pgs_ordinals_count_every_subtitle_stream_in_stream_order() {
    // Out of order on purpose: the ordinal follows the stream index.
    let raw = probe(&[
        (3, "subtitle", PGS_CODEC),
        (0, "video", "h264"),
        (1, "subtitle", "subrip"),
        (4, "audio", "aac"),
        (2, "subtitle", PGS_CODEC),
        (5, "subtitle", "ass"),
        (6, "subtitle", PGS_CODEC),
    ]);
    assert_eq!(pgs_ordinals_from_probe(&raw), vec![1, 2, 4]);
    assert!(pgs_ordinals_from_probe(&probe(&[(0, "video", "h264")])).is_empty());
    // A document this cannot read is no ordinals, never a guess.
    assert!(pgs_ordinals_from_probe("not json").is_empty());
    let unindexed = serde_json::json!({"streams": [
        {"index": 0, "codec_type": "subtitle", "codec_name": PGS_CODEC},
        {"codec_type": "subtitle", "codec_name": PGS_CODEC},
    ]});
    assert!(pgs_ordinals_from_probe(&unindexed.to_string()).is_empty());
}

#[test]
fn eligible_text_ordinals_are_typed_from_the_held_fd_probe() {
    let raw = probe(&[
        (8, "subtitle", "ssa"),
        (0, "video", "h264"),
        (1, "subtitle", "subrip"),
        (2, "subtitle", "dvd_subtitle"),
        (3, "subtitle", "ass"),
        (4, "subtitle", "mov_text"),
        (5, "subtitle", "dvb_subtitle"),
        (6, "subtitle", "webvtt"),
        (7, "subtitle", "text"),
        (9, "subtitle", PGS_CODEC),
        (10, "subtitle", "unknown"),
    ]);
    let actual: Vec<_> = eligible_tracks_from_probe(&raw)
        .into_iter()
        .map(|track| (track.ordinal, track.kind, track.stream_index))
        .collect();
    assert_eq!(
        actual,
        vec![
            (0, ProbedKind::Text, 1),
            (2, ProbedKind::TextStyled, 3),
            (3, ProbedKind::Text, 4),
            (5, ProbedKind::Text, 6),
            (6, ProbedKind::Text, 7),
            (7, ProbedKind::TextStyled, 8),
            (8, ProbedKind::Pgs, 9),
        ]
    );
}

#[test]
fn ass_double_map_has_webvtt_and_matroska_slaves_after_the_index_pipe() {
    let root = crate::test_tempdir().expect("stage");
    let source = root.path().join("source");
    std::fs::write(&source, b"x").expect("source");
    let mut plan =
        RideAlongPlan::standalone(root.path(), 1, stamp_of(&source), vec![0, 1, 2]).expect("plan");
    plan.kinds.insert(0, ProbedKind::Text);
    plan.kinds.insert(1, ProbedKind::TextStyled);
    let args = plan.args();
    assert_eq!(args.iter().filter(|arg| arg.as_str() == "0:s:1").count(), 2);
    assert!(args.windows(2).any(|pair| pair == ["-c:s:2", "copy"]));
    assert!(args.windows(2).any(|pair| pair == ["-c:s:1", "webvtt"]));
    let spec = args.last().expect("tee spec");
    assert!(spec.contains("[select=0:f=webvtt:onfail=ignore]"));
    assert!(spec.contains("[select=1:f=webvtt:onfail=ignore]"));
    assert!(spec.contains("[select=2:f=matroska:onfail=ignore]"));
    assert!(spec.contains("[select=3:f=sup:onfail=ignore]"));
    assert!(spec.ends_with("|[f=null]-"));
}

// -- tee escaping ----------------------------------------------------

/// `libavutil/avstring.c`'s `av_get_token`, as `tee.c` calls it with
/// `"|"`: the one unescaping pass a slave's file name goes through.
fn av_get_token(input: &str) -> (String, &str) {
    const WHITESPACE: &[char] = &[' ', '\n', '\t', '\r'];
    let mut rest = input.trim_start_matches(WHITESPACE);
    let mut out = String::new();
    let mut end = 0;
    while let Some(character) = rest.chars().next() {
        if character == '|' {
            break;
        }
        rest = &rest[character.len_utf8()..];
        if character == '\\' {
            if let Some(next) = rest.chars().next() {
                out.push(next);
                rest = &rest[next.len_utf8()..];
                end = out.len();
                continue;
            }
            out.push(character);
        } else if character == '\'' {
            let close = rest.find('\'');
            let quoted = &rest[..close.unwrap_or(rest.len())];
            out.push_str(quoted);
            rest = &rest[quoted.len()..];
            if close.is_some() {
                rest = &rest[1..];
                end = out.len();
            }
        } else {
            out.push(character);
        }
    }
    let trimmed = out.trim_end_matches(WHITESPACE).len().max(end);
    out.truncate(trimmed);
    (out, rest)
}

#[test]
fn a_tee_escaped_name_survives_the_tee_tokeniser_exactly() {
    for name in [
        "/cache/runtime/subtitle-source-v1/.stage-1/s0.sup",
        r"C:\cache\runtime\s0.sup",
        "/a b/ c |d/[e]/'f'/g\\h/s1.crc ",
        "/trailing space /s0.sup",
    ] {
        let slave = format!("[select=0:f=sup:onfail=ignore]{}", tee_escape(name));
        let spec = format!("{slave}|[f=null]-");
        let (token, rest) = av_get_token(&spec);
        assert_eq!(
            rest, "|[f=null]-",
            "the slave ends at its own delimiter: {spec}"
        );
        assert_eq!(
            token.strip_prefix("[select=0:f=sup:onfail=ignore]"),
            Some(name),
            "{spec}"
        );
    }
    // Unescaped, the Windows path is a different path (§6.2 fact 8).
    let (token, _) = av_get_token(r"[f=sup]C:\cache\s0.sup");
    assert_eq!(token, "[f=sup]C:caches0.sup");
}

// -- stderr ----------------------------------------------------------

#[test]
fn the_stderr_scan_reads_both_failure_lines_across_chunk_boundaries() {
    let slaves = vec![
        "/st/s0.sup".to_owned(),
        "/st/s0.crc".to_owned(),
        "/st/s1.sup".to_owned(),
        "/st/s1.crc".to_owned(),
        "/st/s11.sup".to_owned(),
    ];
    let mut scan = StderrScan::new(slaves);
    let text = "[sup @ 0x1] Not enough data, skipping 884 bytes\n\
                    [tee @ 0x2] Slave muxer #2 failed: Invalid data found when processing input, continuing with 4/5 slaves.\n\
                    [tee @ 0x2] Slave '[select=0:f=framecrc:onfail=ignore]/st/s0.crc': error opening: No space left on device\r\n\
                    [tee @ 0x2] Slave muxer #7 failed: Input/output error, continuing with 3/5 slaves.";
    for chunk in text.as_bytes().chunks(7) {
        scan.feed(chunk);
    }
    scan.finish();
    assert_eq!(
        scan.failure(2),
        Some("Invalid data found when processing input")
    );
    assert_eq!(scan.failure(1), Some("No space left on device"));
    assert_eq!(scan.failure(7), Some("Input/output error"));
    assert_eq!(scan.failure(0), None, "s0.sup is not s0.crc");
    assert_eq!(scan.failure(4), None, "/st/s1.sup names s1, not s11");
    assert!(is_os_error(scan.failure(1).unwrap_or_default()));
    assert!(!is_os_error(scan.failure(2).unwrap_or_default()));
}

// -- the verdict table, on hand-made stages --------------------------

const INTACT_CRC: &str = "#software: Lavf62.3.100\n#tb 0: 1/1000\n#media_type 0: subtitle\n\
        #codec_id 0: hdmv_pgs_subtitle\n\
        0,       1000,       1000,        0,     1142, 0x5021ba81\n\
        0,       7000,       7000,        0,       30, 0x29b90386, F=0x0\n\
        0,       9000,       9000,        0,     1142, 0x61a7ba85, S=1, 8, 0x00000000\n\
        0,      15000,      15000,        0,       30, 0x29e30388\n";

fn stage_with(sup: Option<&[u8]>, crc: Option<&str>) -> tempfile::TempDir {
    let stage = crate::test_tempdir().expect("stage");
    if let Some(sup) = sup {
        std::fs::write(stage.path().join("s0.sup"), sup).expect("sup");
    }
    if let Some(crc) = crc {
        std::fs::write(stage.path().join("s0.crc"), crc).expect("crc");
    }
    stage
}

fn judged(stage: &Path, failures: [Option<&str>; 2]) -> Verdict {
    judge(&stage.join("s0.sup"), &stage.join("s0.crc"), failures).0
}

/// The authored track has four packets carrying sixteen segments. The
/// sums agree only at ten bytes of header per segment, with the segment
/// count taken from walking the `.sup` — never at thirteen, and never
/// with the packet count.
#[test]
fn the_framecrc_rule_is_ten_bytes_per_walked_segment() {
    let size = SELF_TEST_PGS.len() as u64;
    let walk = walk_sup(
        std::fs::File::open(stage_with(Some(SELF_TEST_PGS), None).path().join("s0.sup"))
            .expect("open"),
    )
    .expect("walk");
    let totals = crc_totals(INTACT_CRC).expect("crc");
    assert_eq!((size, walk.segments, totals.packets), (2504, 16, 4));
    assert_eq!(
        walk.packet_bytes, totals.bytes,
        "Σ(3 + len) is the framecrc total"
    );
    assert_eq!(size - 10 * walk.segments, totals.bytes);
    assert_ne!(
        size - 13 * walk.segments,
        totals.bytes,
        "minus 13 per segment"
    );
    assert_ne!(size - 10 * totals.packets, totals.bytes, "per packet");
    let stage = stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC));
    assert_eq!(judged(stage.path(), [None, None]), Verdict::Kept);
}

#[test]
fn every_row_of_the_verdict_table() {
    let header = "#software: Lavf\n#tb 0: 1/1000\n";
    // Zero packets under a header, no failure: a real track with no cues.
    assert_eq!(
        judged(stage_with(Some(b""), Some(header)).path(), [None, None]),
        Verdict::Empty
    );
    // An OS error on either slave, or no companion at all.
    for failure in [
        [Some("No space left on device"), None],
        [None, Some("Permission denied")],
        [
            Some("Input/output error"),
            Some("Invalid data found when processing input"),
        ],
    ] {
        assert_eq!(
            judged(
                stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC)).path(),
                failure
            ),
            Verdict::Transient
        );
    }
    assert_eq!(
        judged(stage_with(Some(SELF_TEST_PGS), None).path(), [None, None]),
        Verdict::Transient
    );
    assert_eq!(
        judged(stage_with(Some(b""), Some("")).path(), [None, None]),
        Verdict::Transient,
        "never written"
    );
    // Any other reported failure, even when the totals agree.
    assert_eq!(
        judged(
            stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC)).path(),
            [None, Some("Broken pipe at close")]
        ),
        Verdict::Malformed
    );
    // Totals that disagree: a track cut at a display-set boundary is a
    // valid shorter stream (§6.2 fact 6) — three of the four sets here.
    let shorter = &SELF_TEST_PGS[..2444];
    assert!(walk_sup(
        std::fs::File::open(stage_with(Some(shorter), None).path().join("s0.sup")).expect("open")
    )
    .is_ok());
    assert_eq!(
        judged(
            stage_with(Some(shorter), Some(INTACT_CRC)).path(),
            [None, None]
        ),
        Verdict::Malformed
    );
    // A walk that leaves bytes over, or runs off the end.
    let mut extra = SELF_TEST_PGS.to_vec();
    extra.extend_from_slice(b"PG");
    assert_eq!(
        judged(
            stage_with(Some(&extra), Some(INTACT_CRC)).path(),
            [None, None]
        ),
        Verdict::Malformed
    );
    assert_eq!(
        judged(
            stage_with(Some(&SELF_TEST_PGS[..2500]), Some(INTACT_CRC)).path(),
            [None, None]
        ),
        Verdict::Malformed
    );
    // Packets counted and no track written.
    assert_eq!(
        judged(stage_with(None, Some(INTACT_CRC)).path(), [None, None]),
        Verdict::Malformed
    );
    // An unreadable framecrc line.
    assert_eq!(
        judged(
            stage_with(Some(SELF_TEST_PGS), Some("#h\n0, 1, 1, 0\n")).path(),
            [None, None]
        ),
        Verdict::Malformed
    );
    // Totals that agree over bytes the PGS parser refuses: every segment
    // well framed, the first one's type no segment type at all.
    let mut unparseable = SELF_TEST_PGS.to_vec();
    unparseable[10] = 0x42;
    assert_eq!(
        judged(
            stage_with(Some(&unparseable), Some(INTACT_CRC)).path(),
            [None, None]
        ),
        Verdict::Malformed
    );
}

#[test]
fn text_packet_cue_count_rule_requires_a_valid_webvtt_cue_per_packet() {
    let stage = crate::test_tempdir().expect("stage");
    let vtt = stage.path().join("s0.vtt");
    let crc = stage.path().join("s0.crc");
    let two = "#software: Lavf\n#media_type 0: subtitle\n0, 1000, 1000, 0, 5, 0x1\n0, 3000, 3000, 0, 5, 0x2\n";
    std::fs::write(&crc, two).expect("crc");
    std::fs::write(
        &vtt,
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\none\n\n00:00:03.000 --> 00:00:04.000\ntwo\n",
    )
    .expect("vtt");
    assert_eq!(
        judge_text_vtt(&vtt, &crc, [None, None], false).0,
        Verdict::Kept
    );
    std::fs::write(&vtt, b"WEBVTT\n\n00:00:03.000 --> 00:00:04.000\ntwo\n").expect("short vtt");
    assert_eq!(
        judge_text_vtt(&vtt, &crc, [None, None], false).0,
        Verdict::Malformed
    );
    std::fs::write(&vtt, b"WEBVTT\n\ninvalid --> cue\ntext\n").expect("bad vtt");
    assert_eq!(
        judge_text_vtt(&vtt, &crc, [None, None], false).0,
        Verdict::Malformed
    );
}

#[test]
fn corrupt_text_packet_decoder_error_vetoes_equal_output_counts_for_only_its_stream() {
    let stage = crate::test_tempdir().expect("stage");
    let vtt = stage.path().join("s0.vtt");
    let crc = stage.path().join("s0.crc");
    // M0 E5: ffmpeg silently dropped the corrupt first packet from both
    // outputs, leaving one cue and one framecrc packet.
    std::fs::write(&vtt, b"WEBVTT\n\n00:00:03.500 --> 00:00:04.500\nsecond\n").expect("vtt");
    std::fs::write(&crc, b"#software: Lavf\n0, 3500, 3500, 0, 6, 0x1\n").expect("crc");
    let mut scan = StderrScan::new(Vec::new());
    scan.feed(b"[sist#0:1/subrip @ 0x1] [dec:srt @ 0x2] Error decoding subtitles: Invalid data found when processing input\n");
    scan.finish();
    assert!(scan.decoder_failed(1));
    assert!(
        !scan.decoder_failed(2),
        "an independent text stream is unaffected"
    );
    assert_eq!(
        judge_text_vtt(&vtt, &crc, [None, None], true).0,
        Verdict::Malformed
    );
    assert_eq!(
        judge_text_vtt(&vtt, &crc, [None, None], false).0,
        Verdict::Kept
    );
}

/// Build a source whose first cue is two seconds in. `source_start` moves
/// the whole container timeline; the index and direct sidecar see the
/// same source bytes under both timestamp regimes.
async fn text_source_case(
    codec: &str,
    source_start: &str,
) -> (tempfile::TempDir, RideAlongPlan, Vec<u8>) {
    plurx_core::testfixtures::require_ffmpeg();
    let dir = crate::test_tempdir().expect("text fixture");
    let subtitle = dir.path().join(if codec == "ass" {
        "input.ass"
    } else {
        "input.srt"
    });
    if codec == "ass" {
        std::fs::write(&subtitle, "[Script Info]\nScriptType: v4.00+\nPlayResX: 64\nPlayResY: 64\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,18,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,{\\pos(12,20)\\b1}Styled line\nDialogue: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,Second line\n").expect("ass");
    } else {
        std::fs::write(&subtitle, "1\n00:00:02,000 --> 00:00:04,000\nFirst cue\n\n2\n00:00:05,000 --> 00:00:06,000\nSecond cue\n\n").expect("srt");
    }
    let container = if codec == "mov_text" {
        "mp4"
    } else {
        "matroska"
    };
    let source = dir.path().join(if codec == "mov_text" {
        "source.mp4"
    } else {
        "source.mkv"
    });
    let mut command = std::process::Command::new(plurx_core::testfixtures::ffmpeg());
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=5:d=8",
            "-i",
        ])
        .arg(&subtitle)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:s:0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:s",
            codec,
            "-output_ts_offset",
            source_start,
            "-f",
            container,
        ])
        .arg(&source);
    plurx_core::testfixtures::run(&mut command);
    let mut file = synthetic_media_file(&source, 8_000).expect("media file");
    file.id = 90_000 + i64::from(codec.as_bytes()[0]) + if source_start == "0" { 0 } else { 1_000 };
    let probe = std::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&source)
        .output()
        .expect("probe");
    assert!(probe.status.success());
    let tracks = eligible_tracks_from_probe(&String::from_utf8_lossy(&probe.stdout));
    assert_eq!(tracks.len(), 1, "one eligible subtitle track");
    assert_eq!(tracks[0].ordinal, 0);
    let handle = std::fs::File::open(&source).expect("source");
    let gate = RideAlongGate::for_test(dir.path().join("store"));
    let plan = plan_tracks(&gate, file.id, &handle, &tracks)
        .await
        .expect("ride");
    let input = source.to_string_lossy().into_owned();
    let bin = plurx_core::testfixtures::ffmpeg();
    let bare = run_ffmpeg(
        &bin,
        &crate::fragindex::index_argv(&file, VIDEO, Some(&input), None),
        dir.path(),
    )
    .await
    .expect("bare pass");
    let riding = run_ffmpeg(
        &bin,
        &crate::fragindex::index_argv(&file, VIDEO, Some(&input), Some(&plan)),
        dir.path(),
    )
    .await
    .expect("riding pass");
    assert!(
        bare.status_ok && riding.status_ok,
        "{}",
        riding.stderr_text()
    );
    assert_eq!(
        bare.stdout, riding.stdout,
        "the index digest's bytes are unchanged"
    );
    let mut scan = plan.stderr_scan();
    scan.feed(&riding.stderr);
    scan.finish();
    let verdict_input = VerdictInput {
        tracks: plan.tracks.clone(),
        kinds: plan.kinds.clone(),
        stream_indexes: plan.stream_indexes.clone(),
        stage: plan.stage().to_owned(),
    };
    let outcomes = verdicts(&verdict_input, Some(&scan));
    assert_eq!(outcomes[0].verdict, Verdict::Kept, "{outcomes:?}");
    let direct = dir.path().join("direct.vtt");
    let mut command = std::process::Command::new(&bin);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&source)
        .args(["-map", "0:s:0", "-f", "webvtt"])
        .arg(&direct);
    plurx_core::testfixtures::run(&mut command);
    let vtt = std::fs::read(plan.vtt_path(0)).expect("tee VTT");
    assert_eq!(
        vtt,
        std::fs::read(direct).expect("direct VTT"),
        "tee and extract_vtt bytes"
    );
    assert_eq!(webvtt_cue_count(&vtt).expect("cues"), 2);
    (dir, plan, vtt)
}

macro_rules! text_case {
    ($name:ident, $codec:literal, $start:literal) => {
        #[tokio::test]
        async fn $name() {
            let (_dir, plan, _vtt) = text_source_case($codec, $start).await;
            if $codec == "ass" {
                let mks = plan.mks_path(0);
                assert!(mks.is_file(), "styled text also keeps Matroska");
                assert!(std::fs::metadata(mks).expect("mks").len() > 0);
            }
        }
    };
}

text_case!(subrip_nonzero_first_cue_zero_source_start, "subrip", "0");
text_case!(
    subrip_nonzero_first_cue_nonzero_source_start,
    "subrip",
    "7.5"
);
text_case!(ass_nonzero_first_cue_zero_source_start, "ass", "0");
text_case!(ass_nonzero_first_cue_nonzero_source_start, "ass", "7.5");
text_case!(
    mov_text_nonzero_first_cue_zero_source_start,
    "mov_text",
    "0"
);
text_case!(
    mov_text_nonzero_first_cue_nonzero_source_start,
    "mov_text",
    "7.5"
);

#[tokio::test]
async fn ass_positioning_and_styling_burn_identically_from_stored_mks_and_source() {
    let filters = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
        .args(["-hide_banner", "-filters"])
        .output()
        .expect("query FFmpeg filters");
    assert!(
        filters.status.success(),
        "querying FFmpeg filters: {}",
        String::from_utf8_lossy(&filters.stderr)
    );
    if !String::from_utf8_lossy(&filters.stdout)
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("subtitles"))
    {
        eprintln!("skipping ASS burn comparison: this FFmpeg lacks the subtitles filter");
        return;
    }
    let (dir, plan, _) = text_source_case("ass", "0").await;
    let source = dir.path().join("source.mkv");
    let render = |subtitle: &Path| {
        let output = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-ss",
                "3",
                "-i",
            ])
            .arg(&source)
            .args([
                "-vf",
                &format!("subtitles=filename='{}':si=0", subtitle.display()),
                "-frames:v",
                "1",
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .output()
            .expect("render");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
        output.stdout
    };
    assert_eq!(render(&source), render(&plan.mks_path(0)));
}

// -- the gate ------------------------------------------------------------

#[test]
fn advisory_readiness_reports_the_switch_self_test_and_local_cache() {
    let passed = SelfTest::Passed {
        elapsed_ms: 1,
        version: "8.0.1".into(),
    };
    let local = || Ok::<_, String>("ext4".to_owned());
    let roomy = || Ok::<_, String>("plenty".to_owned());
    let root = PathBuf::from("/cache/runtime/subtitle-source-v1");
    assert!(gate_from(true, &passed, local(), roomy(), root.clone()).is_ok());
    assert!(gate_from(false, &passed, local(), roomy(), root.clone()).is_err());
    let full = gate_from(
        true,
        &passed,
        local(),
        Err("0.2 GiB free".into()),
        root.clone(),
    )
    .expect_err("a full cache does not ride");
    assert!(full.contains("too full"), "{full}");
    for state in [
        SelfTest::NotRun,
        SelfTest::Running,
        SelfTest::Failed {
            reason: "boom".into(),
        },
    ] {
        let refused = gate_from(true, &state, local(), roomy(), root.clone())
            .expect_err("off until it passes");
        assert!(refused.contains("self-test"), "{refused}");
    }
    assert!(gate_from(true, &passed, Err("NFS (0x6969)".into()), roomy(), root).is_err());
}

/// Review finding 8: the margin is one gibibyte or two percent of the
/// filesystem, whichever is larger.
#[test]
fn the_free_space_margin_is_the_larger_of_a_gibibyte_and_two_percent() {
    const GIB: u64 = 1 << 30;
    assert_eq!(free_space_margin(10 * GIB), GIB);
    assert_eq!(free_space_margin(1000 * GIB), 20 * GIB);
    assert!(free_space_verdict(2 * GIB, 10 * GIB).is_ok());
    assert!(free_space_verdict(GIB - 1, 10 * GIB).is_err());
    assert!(free_space_verdict(19 * GIB, 1000 * GIB).is_err());
    assert!(free_space_verdict(21 * GIB, 1000 * GIB).is_ok());
}

#[test]
fn a_version_token_is_read_from_the_banner() {
    assert_eq!(
        version_token("ffmpeg version 8.0.1-3ubuntu2 Copyright (c) 2000-2025\n").as_deref(),
        Some("8.0.1-3ubuntu2")
    );
    assert_eq!(
        version_token("\nffprobe version n7.1 Copyright").as_deref(),
        Some("n7.1")
    );
    assert_eq!(version_token("no banner"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn network_and_fuse_filesystems_are_not_local() {
    for magic in [0x6969_u32, 0x517B, 0xFF53_4D42, 0xFE53_4D42, 0x6573_5546] {
        assert!(classify_filesystem_magic(magic).is_err(), "0x{magic:x}");
    }
    for magic in [
        0xEF53_u32,
        0x0102_1994,
        0x5846_5342,
        0x9123_683E,
        0x794C_7630,
    ] {
        assert!(classify_filesystem_magic(magic).is_ok(), "0x{magic:x}");
    }
    let here = crate::test_tempdir().expect("tempdir");
    assert!(
        local_filesystem(here.path()).is_ok(),
        "the test host's temp dir"
    );
    assert!(local_filesystem(&here.path().join("missing")).is_err());
}

// -- the latch -----------------------------------------------------------

struct LatchCase {
    _dir: tempfile::TempDir,
    root: PathBuf,
    source_path: PathBuf,
    source: std::fs::File,
    stamp: SourceStamp,
}

fn latch_case() -> LatchCase {
    let dir = crate::test_tempdir().expect("latch");
    let path = dir.path().join("source.mkv");
    std::fs::write(&path, b"source").expect("source");
    LatchCase {
        root: dir.path().join(store::STORE_DIR),
        source_path: path.clone(),
        source: std::fs::File::open(&path).expect("open"),
        stamp: stamp_of(&path),
        _dir: dir,
    }
}

fn transient(ordinal: i64, attempts: u32) -> TrackEntry {
    TrackEntry {
        attempts,
        ..settled_entry(ordinal, Verdict::Transient)
    }
}

#[tokio::test]
async fn the_latch_rides_until_every_probed_track_is_settled() {
    let case = latch_case();
    let gate = RideAlongGate::for_test(case.root.clone());
    let id = 70_001;

    let fresh = plan(&gate, id, &case.source, &[0, 2])
        .await
        .expect("no manifest rides");
    assert_eq!(fresh.tracks(), &[0, 2]);
    assert!(fresh.stage().is_dir());
    assert!(
        plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
        "one ride per file at a time"
    );
    let stage = fresh.stage().to_owned();
    drop(fresh);
    assert!(!stage.exists(), "the guard removes an unused stage");

    let dir = store::file_dir(&case.root, id);
    let stored = crate::subtitle_source::testing::kept(&dir, 0, b"PG kept");
    write_manifest(
        &case.root,
        id,
        case.stamp,
        vec![stored.clone(), settled_entry(2, Verdict::Malformed)],
    );
    assert!(
        plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
        "latched"
    );
    // Review finding 4: a `kept` entry whose file is gone — a power loss
    // before the rename was durable, a deletion — is not settled. That
    // track alone rides again, rather than missing for good.
    std::fs::remove_file(dir.join(stored.file.as_deref().expect("name"))).expect("lose it");
    let lost = plan(&gate, id, &case.source, &[0, 2])
        .await
        .expect("a kept track with no file rides again");
    assert_eq!(lost.tracks(), &[0]);
    drop(lost);

    // A transient track rides again, alone, until it has used its tries.
    for attempts in 1..TRANSIENT_ATTEMPTS {
        write_manifest(
            &case.root,
            id,
            case.stamp,
            vec![settled_entry(0, Verdict::Empty), transient(2, attempts)],
        );
        let retry = plan(&gate, id, &case.source, &[0, 2])
            .await
            .expect("transient retries");
        assert_eq!(retry.tracks(), &[2]);
        assert_eq!(retry.prior_attempts.get(&2), Some(&attempts));
        assert_eq!(retry.carried.len(), 1);
    }
    write_manifest(
        &case.root,
        id,
        case.stamp,
        vec![
            settled_entry(0, Verdict::Empty),
            transient(2, TRANSIENT_ATTEMPTS),
        ],
    );
    assert!(
        plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
        "tries used"
    );

    // A probed track the manifest has no word for.
    let grown = plan(&gate, id, &case.source, &[0, 2, 3])
        .await
        .expect("new track");
    assert_eq!(grown.tracks(), &[3]);
    drop(grown);

    // Another source identity: everything again, attempts from zero.
    for moved in [
        SourceStamp {
            size: case.stamp.size + 1,
            ..case.stamp
        },
        SourceStamp {
            mtime: case.stamp.mtime + 1,
            ..case.stamp
        },
        SourceStamp {
            ino: case.stamp.ino.map(|ino| ino + 1),
            ..case.stamp
        },
    ] {
        write_manifest(
            &case.root,
            id,
            moved,
            vec![
                settled_entry(0, Verdict::Kept),
                settled_entry(2, Verdict::Kept),
            ],
        );
        let again = plan(&gate, id, &case.source, &[0, 2])
            .await
            .expect("a moved source rides");
        assert_eq!(again.tracks(), &[0, 2]);
        assert!(again.prior_attempts.values().all(|attempts| *attempts == 0));
    }
}

/// PR 3: a ride in flight is listed with its tracks and what its stage
/// holds, and is gone once its plan is.
#[tokio::test]
async fn a_ride_in_flight_is_listed_until_its_plan_ends() {
    let case = latch_case();
    let gate = RideAlongGate::for_test(case.root.clone());
    let id = 70_020;
    let riding = plan(&gate, id, &case.source, &[0, 3]).await.expect("rides");
    std::fs::write(riding.stage().join("s0.sup"), [0_u8; 1234]).expect("written");
    let listed = active_rides()
        .into_iter()
        .find(|ride| ride.file_id == id)
        .expect("listed while it runs");
    assert_eq!(listed.tracks, 2);
    assert_eq!(listed.bytes_written, 1234);
    assert!(listed.started_at_ms > 0, "{listed:?}");
    assert!(
        (0..60_000).contains(&listed.running_ms),
        "running since the plan was made: {listed:?}"
    );
    // The settings read carries the same list, and says why the next
    // pass would not ride when the switch is off.
    let diagnostics = crate::subtitle_source::diagnostics(false, &case.root).await;
    assert!(diagnostics.riding.iter().any(|ride| ride.file_id == id));
    assert!(!diagnostics.gate.open);
    assert_eq!(
        diagnostics.gate.reason.as_deref(),
        Some("subtitles.stored_sources is off")
    );
    drop(riding);
    assert!(!active_rides().iter().any(|ride| ride.file_id == id));
}

/// A file with no eligible subtitle track has nothing to latch, and never rides.
#[tokio::test]
async fn a_file_with_no_eligible_subtitle_track_never_rides() {
    let case = latch_case();
    let gate = RideAlongGate::for_test(case.root.clone());
    for _ in 0..3 {
        assert!(plan(&gate, 70_002, &case.source, &[]).await.is_none());
    }
    assert!(
        !store::file_dir(&case.root, 70_002).exists(),
        "and writes nothing"
    );
}

/// The argv is built from the probe's ordinals alone: a track the
/// scanner once saw and the probe does not is never mapped.
#[tokio::test]
async fn a_stale_scan_time_ordinal_is_never_in_the_argv() {
    let case = latch_case();
    let gate = RideAlongGate::for_test(case.root.clone());
    let probed =
        pgs_ordinals_from_probe(&probe(&[(0, "video", "h264"), (1, "subtitle", PGS_CODEC)]));
    let plan = plan(&gate, 70_003, &case.source, &probed)
        .await
        .expect("plan");
    let args = plan.args();
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:s:0"]));
    assert!(!args.iter().any(|arg| arg == "0:s:1"), "{args:?}");
    assert!(
        !args.iter().any(|arg| arg.ends_with('?')),
        "hard maps only: {args:?}"
    );
    assert_eq!(
        args.last().map(|spec| spec.ends_with("|[f=null]-")),
        Some(true)
    );
    assert_eq!(&args[..2], ["-nostdin", "-y"]);
}

// -- the digest ----------------------------------------------------------

/// The ride-along never reaches the cluster cache key: the digest of a
/// file with PGS tracks is the digest of the same file without them, and
/// the pass's argv is the hashed argv with the ride-along appended after
/// `pipe:1`.
#[test]
fn the_pipeline_digest_is_identical_with_and_without_the_ride_along() {
    let dir = crate::test_tempdir().expect("digest");
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"x").expect("source");
    let mut file = synthetic_media_file(&source, 60_000).expect("file");
    let bare = file.clone();
    file.subtitle_streams = [(1, "subrip"), (2, "ass"), (3, PGS_CODEC)]
        .into_iter()
        .map(|(index, codec)| plurx_core::domain::SubtitleStream {
            index,
            codec: codec.to_owned(),
            language: Some("eng".into()),
            title: None,
            default: index == 1,
            forced: false,
            hearing_impaired: false,
        })
        .collect();
    let engine = "e".repeat(64);
    let mut plan = RideAlongPlan::standalone(dir.path(), file.id, stamp_of(&source), vec![0, 1, 2])
        .expect("plan");
    plan.kinds.insert(0, ProbedKind::Text);
    plan.kinds.insert(1, ProbedKind::TextStyled);

    let hashed = plurx_core::transcode::copy_index_pipe_args(&file, VIDEO);
    let without = crate::fragindex::index_argv(&file, VIDEO, None, None);
    let with = crate::fragindex::index_argv(&file, VIDEO, None, Some(&plan));
    assert_eq!(without, hashed);
    assert_eq!(
        &with[..hashed.len()],
        hashed.as_slice(),
        "appended, never inserted"
    );
    assert_eq!(hashed.last().map(String::as_str), Some("pipe:1"));
    assert!(with.len() > hashed.len());
    assert!(
        !hashed
            .iter()
            .any(|arg| arg == "tee" || arg.starts_with("0:s:")),
        "{hashed:?}"
    );

    let digest = crate::fragment_index_cluster::pipeline_digest(&file, &engine, VIDEO);
    assert_eq!(
        digest,
        crate::fragment_index_cluster::pipeline_digest(&bare, &engine, VIDEO)
    );
    // And the pin can fail: hashing the riding argv is a different key.
    let mut riding = with.clone();
    if let Some(input) = riding
        .windows(2)
        .position(|pair| pair[0] == "-i")
        .map(|at| at + 1)
    {
        riding[input] = "{attested-source-fd}".to_owned();
    }
    assert_ne!(
        Some(digest),
        plurx_core::store::cluster_fragment_index_pipeline_digest(&engine, &riding)
    );
}

// -- real passes ----------------------------------------------------------

fn fresh_cache() -> tempfile::TempDir {
    crate::test_tempdir().expect("runtime cache")
}

/// A pass, judged as the callers judge it: after the build has returned.
struct Judged {
    outcome: IndexOutcome,
    ride_along: Option<RideAlongHarvest>,
    /// Each track judged again with no stderr failures at all — what the
    /// byte arithmetic says on its own.
    by_arithmetic: Vec<(i64, Verdict, Option<String>)>,
}

async fn pass(fixture: &Fixture, cache: &Path, gate: Option<&RideAlongGate>) -> Judged {
    let source = std::fs::File::open(&fixture.source).expect("held source");
    let IndexBuild {
        outcome,
        ride_along,
        ..
    } = crate::fragindex::build_from_attested_file_with_progress(
        &fixture.file,
        &source,
        "fixture-object-v1",
        VIDEO,
        cache,
        Duration::from_secs(60),
        |_: &crate::fragindex::PassProgress| {},
        gate,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    let mut by_arithmetic = Vec::new();
    if let Some(pending) = &ride_along {
        for ordinal in pending.plan.tracks() {
            let stage = pending.stage();
            let (verdict, _, reason) = judge(
                &stage.join(format!("s{ordinal}.sup")),
                &stage.join(format!("s{ordinal}.crc")),
                [None, None],
            );
            by_arithmetic.push((*ordinal, verdict, reason));
        }
    }
    let ride_along = match ride_along {
        Some(pending) => Some(pending.judge().await),
        None => None,
    };
    Judged {
        outcome,
        ride_along,
        by_arithmetic,
    }
}

fn verdicts_of(build: &Judged) -> Vec<(i64, Verdict)> {
    build
        .ride_along
        .as_ref()
        .expect("a harvest")
        .outcomes()
        .iter()
        .map(|outcome| (outcome.ordinal, outcome.verdict))
        .collect()
}

/// (a) A corrupted first segment in one track, beside an intact track:
/// the index is exactly the baseline's — byte for byte on the pipe, and
/// row for row once indexed — the corrupted track is `malformed` and the
/// intact one `kept`.
#[tokio::test]
async fn a_corrupted_track_is_malformed_and_the_index_is_untouched() {
    let mut fixture = fixture(71_001, &[Sub::Pgs, Sub::Pgs]);
    corrupt(&mut fixture, 3);
    let cache = fresh_cache();
    let root = store::store_root(cache.path());
    let gate = RideAlongGate::for_test(root.clone());

    let baseline = pass(&fixture, cache.path(), None).await;
    assert!(
        matches!(baseline.outcome, IndexOutcome::Built(_)),
        "{:?}",
        baseline.outcome
    );
    assert!(baseline.ride_along.is_none());
    let riding = pass(&fixture, cache.path(), Some(&gate)).await;
    assert_eq!(
        riding.outcome, baseline.outcome,
        "the index rows are the baseline's"
    );
    assert_eq!(
        verdicts_of(&riding),
        vec![(0, Verdict::Kept), (1, Verdict::Malformed)]
    );
    // Review finding 2: stderr reported the corrupted track's failure, and
    // `judge` stops there. The framecrc rule is the primary check, so it
    // must catch the track by itself: with no stderr failure at all, the
    // byte arithmetic alone says `malformed` (0 bytes in the `.sup`
    // against the 2344 packet bytes framecrc counted), and the intact
    // track is still `kept`.
    assert_eq!(riding.by_arithmetic[0].1, Verdict::Kept);
    assert_eq!(riding.by_arithmetic[1].1, Verdict::Malformed);
    assert!(
        riding.by_arithmetic[1]
            .2
            .as_deref()
            .is_some_and(|reason| reason.contains("framecrc counted")),
        "{:?}",
        riding.by_arithmetic[1]
    );

    // The pipe itself, byte for byte, with the same argv.
    let plan =
        RideAlongPlan::standalone(fixture.dir.path(), 1, stamp_of(&fixture.source), vec![0, 1])
            .expect("plan");
    let input = fixture.source.to_string_lossy().into_owned();
    let bin = plurx_core::testfixtures::ffmpeg();
    let without = run_ffmpeg(
        &bin,
        &crate::fragindex::index_argv(&fixture.file, VIDEO, Some(&input), None),
        cache.path(),
    )
    .await
    .expect("baseline");
    let with = run_ffmpeg(
        &bin,
        &crate::fragindex::index_argv(&fixture.file, VIDEO, Some(&input), Some(&plan)),
        cache.path(),
    )
    .await
    .expect("riding");
    assert!(without.status_ok && with.status_ok);
    assert!(!without.stdout.is_empty());
    assert!(
        without.stdout == with.stdout,
        "the index pipe is byte-identical"
    );

    // Published, the intact track is served and the corrupted one is not.
    let manifest = riding
        .ride_along
        .expect("harvest")
        .publish()
        .await
        .expect("publish");
    assert_eq!(manifest.ordinals, vec![0, 1]);
    let access = store::StoreAccess::new(root, true);
    let live = stamp_of(&fixture.source);
    let Lookup::Kept(kept) = store::lookup(
        &access,
        Consumer::Overlay,
        &fixture.file,
        0,
        Live::Stamp(live),
    )
    .await
    else {
        panic!("the intact track is kept");
    };
    let mut stored = Vec::new();
    std::io::Read::read_to_end(
        &mut store::open_verified(&kept).await.expect("verified"),
        &mut stored,
    )
    .expect("read");
    // Byte-identical to a whole-source extraction of the same track, the
    // bytes the overlay's own demux writes (§6.2 fact 9).
    let extracted = fixture.dir.path().join("extracted.sup");
    let mut extract: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]
        .map(str::to_owned)
        .to_vec();
    extract.push(fixture.source.to_string_lossy().into_owned());
    extract.extend(["-map", "0:s:0", "-c:s", "copy", "-f", "sup"].map(str::to_owned));
    extract.push(extracted.to_string_lossy().into_owned());
    run_ffmpeg_to_completion(&extract);
    assert_eq!(
        stored,
        std::fs::read(&extracted).expect("extracted"),
        "byte-identical to the demux"
    );
    assert!(matches!(
        store::lookup(
            &access,
            Consumer::Overlay,
            &fixture.file,
            1,
            Live::Stamp(live)
        )
        .await,
        Lookup::Miss(_)
    ));
}

/// (b) Several segments per packet: kept, and the arithmetic that keeps
/// it is the ten-byte rule over walked segments.
#[tokio::test]
async fn a_multi_segment_packet_track_is_kept_by_the_ten_byte_rule() {
    let fixture = fixture(71_002, &[Sub::Pgs]);
    let cache = fresh_cache();
    let riding = pass(
        &fixture,
        cache.path(),
        Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
    )
    .await;
    assert_eq!(verdicts_of(&riding), vec![(0, Verdict::Kept)]);
    let stage = riding
        .ride_along
        .as_ref()
        .expect("harvest")
        .plan
        .stage()
        .to_owned();
    assert!(riding
        .by_arithmetic
        .iter()
        .all(|row| row.1 == Verdict::Kept));
    let sup = std::fs::read(stage.join("s0.sup")).expect("sup");
    let totals =
        crc_totals(&std::fs::read_to_string(stage.join("s0.crc")).expect("crc")).expect("totals");
    let walk = walk_sup(std::fs::File::open(stage.join("s0.sup")).expect("sup")).expect("walk");
    assert!(
        walk.segments > totals.packets,
        "more than one segment per packet"
    );
    let size = sup.len() as u64;
    assert_eq!(size - 10 * walk.segments, totals.bytes);
    assert_ne!(size - 13 * walk.segments, totals.bytes);
    assert_ne!(size - 10 * totals.packets, totals.bytes);
}

/// (c) through a real pass: a text track shifts the PGS ordinal, and a
/// scan-time track list that claims more PGS tracks than the file has is
/// never read — the index builds and only the probed track is kept.
#[tokio::test]
async fn the_real_pass_maps_only_what_the_probe_found() {
    let mut fixture = fixture(71_003, &[Sub::Srt, Sub::Pgs]);
    fixture.file.subtitle_streams = [(1, "subrip"), (2, PGS_CODEC), (3, PGS_CODEC)]
        .into_iter()
        .map(|(index, codec)| plurx_core::domain::SubtitleStream {
            index,
            codec: codec.to_owned(),
            language: None,
            title: None,
            default: false,
            forced: false,
            hearing_impaired: false,
        })
        .collect();
    let cache = fresh_cache();
    let riding = pass(
        &fixture,
        cache.path(),
        Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
    )
    .await;
    assert!(
        matches!(riding.outcome, IndexOutcome::Built(_)),
        "{:?}",
        riding.outcome
    );
    assert_eq!(
        verdicts_of(&riding),
        vec![(0, Verdict::Kept), (1, Verdict::Kept)]
    );
}

/// (d) A real PGS track with no cues is `empty`.
#[tokio::test]
async fn a_track_with_no_cues_is_empty() {
    let fixture = fixture(71_004, &[Sub::EmptyPgs, Sub::Pgs]);
    let cache = fresh_cache();
    let riding = pass(
        &fixture,
        cache.path(),
        Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
    )
    .await;
    assert_eq!(
        verdicts_of(&riding),
        vec![(0, Verdict::Empty), (1, Verdict::Kept)]
    );
    let manifest = riding
        .ride_along
        .expect("harvest")
        .publish()
        .await
        .expect("publish");
    assert!(manifest.latched());
    assert_eq!(
        manifest.track(0).map(|track| track.file.is_none()),
        Some(true)
    );
}

/// (e) A cache path with a backslash — and every other character the
/// tee tokeniser treats specially — still lands every slave in the stage.
#[tokio::test]
async fn a_stage_path_with_a_backslash_is_written_where_it_says() {
    let fixture = fixture(71_005, &[Sub::Pgs]);
    let cache = fresh_cache();
    let odd = cache.path().join("back\\slash |pipe 'quote' [bracket]");
    let root = store::store_root(&odd);
    let riding = pass(
        &fixture,
        cache.path(),
        Some(&RideAlongGate::for_test(root.clone())),
    )
    .await;
    assert_eq!(verdicts_of(&riding), vec![(0, Verdict::Kept)]);
    riding
        .ride_along
        .expect("harvest")
        .publish()
        .await
        .expect("publish");
    let dir = store::file_dir(&root, fixture.file.id);
    assert!(std::fs::read_dir(&dir)
        .expect("published")
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".sup")));
    // Nothing escaped the stage into a sibling the unescaped name names.
    assert!(!cache
        .path()
        .join("backslash |pipe 'quote' [bracket]")
        .exists());
}

// -- publish ---------------------------------------------------------------

fn harvest_of(root: &Path, file_id: i64, stamp: SourceStamp, bytes: &[u8]) -> RideAlongHarvest {
    let plan = RideAlongPlan::standalone(root, file_id, stamp, vec![0]).expect("plan");
    std::fs::write(plan.sup_path(0), bytes).expect("staged");
    RideAlongHarvest {
        outcomes: vec![TrackOutcome {
            ordinal: 0,
            verdict: Verdict::Kept,
            sha256: Some(hex::encode(Sha256::digest(bytes))),
            written: bytes.len() as u64,
            reason: None,
            representations: vec![RepresentationOutcome {
                format: RepresentationFormat::Sup,
                verdict: Verdict::Kept,
                sha256: Some(hex::encode(Sha256::digest(bytes))),
                bytes: bytes.len() as u64,
                reason: None,
            }],
        }],
        plan,
    }
}

#[tokio::test]
async fn publish_places_then_swaps_then_deletes_then_marks() {
    let case = latch_case();
    let id = 70_010;
    let first = harvest_of(&case.root, id, case.stamp, b"PG first");
    let first_stage = first.plan.stage().to_owned();
    let one = first.publish().await.expect("first");
    assert!(!first_stage.exists(), "the stage goes with the harvest");
    let dir = store::file_dir(&case.root, id);
    let first_name = one
        .track(0)
        .and_then(|track| track.file.clone())
        .expect("named");
    assert!(dir.join(&first_name).is_file());
    assert!(dir.join(".access").is_file(), "(4) the first access marker");
    assert_eq!(one.track(0).map(|track| track.attempts), Some(1));

    let two = harvest_of(&case.root, id, case.stamp, b"PG second")
        .publish()
        .await
        .expect("second");
    let second_name = two
        .track(0)
        .and_then(|track| track.file.clone())
        .expect("named");
    assert_ne!(first_name, second_name, "content-named");
    assert!(dir.join(&second_name).is_file(), "(1) placed");
    assert!(
        !dir.join(&first_name).exists(),
        "(3) the replaced track is deleted"
    );
    let on_disk = store::read_manifest(&dir).await.expect("(2) manifest");
    assert_eq!(on_disk, two);
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn a_publication_row_is_written_only_after_the_manifest_rename() {
    let case = latch_case();
    let id = 70_012;
    let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
    let manifest = harvest_of(&case.root, id, case.stamp, b"PG source")
        .publish_with_cluster(&catalog, "lab4", &"a".repeat(64), None)
        .await
        .expect("publish");
    let rows = catalog
        .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, "extracted");
    let dir = store::file_dir(&case.root, id);
    assert_eq!(store::read_manifest(&dir).await, Some(manifest.clone()));
    assert!(dir
        .join(
            manifest
                .track(0)
                .and_then(|entry| entry.file.as_deref())
                .expect("name")
        )
        .is_file());
}

#[tokio::test]
async fn cancellation_during_publish_removes_new_cluster_rows_and_local_entry() {
    let case = latch_case();
    let id = 70_013;
    let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
    let lost = CancellationToken::new();
    let result = harvest_of(&case.root, id, case.stamp, b"cancelled source")
        .publish_internal(
            Some((&catalog, "lab4", &"a".repeat(64))),
            Some(&lost),
            None,
            None,
            || async { lost.cancel() },
        )
        .await;
    assert!(result.is_err(), "a cancelled publish cannot commit");
    assert!(lost.is_cancelled());
    assert!(
        catalog
            .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
            .await
            .expect("rows")
            .is_empty(),
        "the row written at the controlled seam was compensated"
    );
    let dir = store::file_dir(&case.root, id);
    assert!(store::read_manifest(&dir).await.is_none());
    assert!(
        !dir.join(
            store::representation_file_name(
                0,
                RepresentationFormat::Sup,
                &hex::encode(Sha256::digest(b"cancelled source"))
            )
            .expect("name")
        )
        .exists(),
        "the unpublished content file is removed"
    );
}

#[tokio::test]
async fn admin_cancellation_during_publish_compensates_rows_and_manifest() {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use plurx_core::store::{LibraryStore, MediaStore, NewAnalysisRequest};

    let case = latch_case();
    let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
    let library = catalog
        .create_library(&NewLibrary {
            name: "Source".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![case.root.clone()],
            anime: false,
        })
        .await
        .expect("library");
    let item = catalog
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Source".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = catalog
        .upsert_file(
            item,
            case.source_path.to_str().expect("source path"),
            case.stamp.size as i64,
            case.stamp.mtime,
            &ProbeResult::default(),
        )
        .await
        .expect("file");
    let now = unix_ms();
    let request = catalog
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "cancel-publish".to_owned(),
            file_id: id,
            source_size: case.stamp.size as i64,
            source_mtime: case.stamp.mtime,
            component: "subtitle_source".to_owned(),
            pipeline_version: "test-pipeline".to_owned(),
            video_identity: String::new(),
            requested_generation: "cancel-publish".to_owned(),
            priority: "foreground".to_owned(),
            trigger: "playback".to_owned(),
            force_rebuild: false,
            target_node_id: String::new(),
            not_before_ms: now,
            created_at_ms: now,
        })
        .await
        .expect("enqueue");
    let claimed = catalog
        .claim_subtitle_fixture(&request.request_id, "lab4", now, now + 60_000)
        .await
        .expect("claim")
        .expect("running");
    assert_eq!(claimed.request_id, request.request_id);
    let lost = CancellationToken::new();
    assert!(!lost.is_cancelled(), "admin cancel is a separate fence");
    let result = harvest_of(&case.root, id, case.stamp, b"admin cancelled")
        .publish_internal(
            Some((&catalog, "lab4", &"a".repeat(64))),
            Some(&lost),
            Some(&claimed),
            None,
            || async {
                catalog
                    .cancel_analysis_request_admin(&claimed.request_id, now + 1)
                    .await
                    .expect("cancel");
            },
        )
        .await;
    assert!(result.is_err(), "the cancelled fence cannot publish");
    assert!(store::read_manifest(&store::file_dir(&case.root, id))
        .await
        .is_none());
    assert!(catalog
        .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
        .await
        .expect("rows")
        .is_empty());
}

/// Readers racing a stream of publishes see the old manifest or the new
/// one, and a manifest that stays on disk across a check never names a
/// file that is not there. A lookup either verifies one of the published
/// contents or misses; it never fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_mid_publish_sees_the_old_or_the_new_manifest() {
    let case = latch_case();
    let id = 70_011;
    let dir = store::file_dir(&case.root, id);
    let contents: [&[u8]; 2] = [b"PG alpha", b"PG bravo bravo"];
    harvest_of(&case.root, id, case.stamp, contents[0])
        .publish()
        .await
        .expect("seed");
    let file = synthetic_media_file(&case.root.join("..").join("source.mkv"), 1_000).expect("file");
    let file = plurx_core::domain::MediaFile { id, ..file };
    let access = store::StoreAccess::new(case.root.clone(), true);
    let done = std::sync::atomic::AtomicBool::new(false);

    let publisher = async {
        for round in 0..40 {
            harvest_of(&case.root, id, case.stamp, contents[round % 2])
                .publish()
                .await
                .expect("publish");
        }
        done.store(true, Ordering::SeqCst);
    };
    let reader = async {
        let (mut hits, mut checks) = (0, 0);
        while !done.load(Ordering::SeqCst) {
            let before =
                std::fs::read(dir.join(MANIFEST_NAME)).expect("a manifest is always there");
            let manifest = store::parse_manifest(&before).expect("never a torn manifest");
            let name = manifest
                .track(0)
                .and_then(|track| track.file.clone())
                .expect("named");
            let present = dir.join(&name).is_file();
            let after = std::fs::read(dir.join(MANIFEST_NAME)).expect("manifest");
            if before == after {
                assert!(
                    present,
                    "the manifest on disk named {name}, which was not there"
                );
                checks += 1;
            }
            match store::lookup(
                &access,
                Consumer::Overlay,
                &file,
                0,
                Live::Stamp(case.stamp),
            )
            .await
            {
                Lookup::Kept(kept) => {
                    if let Some(mut opened) = store::open_verified(&kept).await {
                        let mut bytes = Vec::new();
                        std::io::Read::read_to_end(&mut opened, &mut bytes).expect("read");
                        assert!(contents.contains(&bytes.as_slice()), "{bytes:?}");
                        hits += 1;
                    }
                }
                other => panic!("a current manifest is found: {other:?}"),
            }
            tokio::task::yield_now().await;
        }
        (hits, checks)
    };
    let ((), (hits, checks)) = tokio::join!(publisher, reader);
    assert!(hits > 0 && checks > 0, "hits={hits} checks={checks}");
}

// -- the self-test --------------------------------------------------------

#[tokio::test]
async fn the_self_test_passes_on_this_ffmpeg() {
    plurx_core::testfixtures::require_ffmpeg();
    let cache = fresh_cache();
    let root = store::store_root(cache.path());
    let report = run_self_test(
        &plurx_core::testfixtures::ffmpeg(),
        &plurx_core::testfixtures::ffprobe(),
        &root,
        cache.path(),
    )
    .await
    .expect("the self-test passes");
    assert!(report.elapsed < SELF_TEST_BUDGET);
    assert!(!report.version.is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("root")
        .filter_map(Result::ok)
        .collect();
    assert!(leftovers.is_empty(), "the self-test cleans up after itself");
}

#[cfg(unix)]
#[tokio::test]
async fn the_self_test_fails_on_an_ffmpeg_that_does_not_behave() {
    let cache = fresh_cache();
    let root = store::store_root(cache.path());
    let real = plurx_core::testfixtures::ffmpeg();
    let prober = plurx_core::testfixtures::ffprobe();
    let script = |name: &str, body: String| {
        let path = cache.path().join(name);
        crate::write_test_executable(&path, body, 0o755);
        path.to_string_lossy().into_owned()
    };
    // Answers `-version` as the real build does, then exits 0 and does
    // nothing: it "muxes" no source.
    let silent = script(
        "silent-ffmpeg",
        format!("#!/bin/sh\ncase \"$1\" in -version) exec {real} \"$@\" ;; esac\nexit 0\n"),
    );
    let reason = run_self_test(&silent, &prober, &root, cache.path())
        .await
        .expect_err("nothing was produced");
    assert!(reason.contains("synthetic source"), "{reason}");
    // Real ffmpeg for the source, a build whose tee never writes a slave
    // for the second track: the verdicts are not the expected pair.
    let partial = script(
            "partial-ffmpeg",
            format!(
                "#!/bin/sh\ncase \"$*\" in *\"-f tee \"*) exec {real} \"$@\" 2>/dev/null ;; *) exec {real} \"$@\" ;; esac\n"
            ),
        );
    let reason = run_self_test(&partial, &prober, &root, cache.path())
        .await
        .expect_err("the stderr line is not recognised when it never arrives");
    assert!(reason.contains("not recognised"), "{reason}");
    assert!(
        run_self_test("/nonexistent/ffmpeg", &prober, &root, cache.path())
            .await
            .is_err()
    );
    // Review finding 1: an ffprobe of another build is refused before
    // anything runs, and the reason names both versions.
    let other_prober = script(
        "old-ffprobe",
        "#!/bin/sh\necho \"ffprobe version 4.4.2-0ubuntu0 Copyright (c) 2007-2021\"\n".to_owned(),
    );
    let reason = run_self_test(&real, &other_prober, &root, cache.path())
        .await
        .expect_err("mismatched builds");
    assert!(reason.contains("4.4.2-0ubuntu0"), "{reason}");
}

/// The operator may enable the producer before its advisory self-test;
/// only the saved manual switch controls entry to the pass.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_ride_along_enable_is_not_gated_by_advisory_readiness() {
    plurx_core::testfixtures::require_ffmpeg();
    let cache = fresh_cache();
    let catalog = Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("catalog"));
    set_self_test_state(SelfTest::NotRun);
    assert!(RideAlongGate::open(catalog.clone(), cache.path())
        .await
        .is_some());
    set_self_test_state(SelfTest::Failed {
        reason: "test".into(),
    });
    assert!(RideAlongGate::open(catalog.clone(), cache.path())
        .await
        .is_some());
    startup_self_test(
        cache.path().to_owned(),
        tokio_util::sync::CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(self_test_state(), SelfTest::Passed { .. }),
        "{:?}",
        self_test_state()
    );
    assert!(RideAlongGate::open(catalog.clone(), cache.path())
        .await
        .is_some());
    // One switch: off stops the producer too.
    plurx_core::store::SettingsStore::put_setting(
        catalog.as_ref(),
        plurx_core::store::keys::SUBTITLE_STORED_SOURCES,
        "0",
    )
    .await
    .expect("switch off");
    assert!(RideAlongGate::open(catalog, cache.path()).await.is_none());
}

#[tokio::test]
async fn a_second_nodes_index_pass_skips_cluster_extracted_coverage_for_the_same_source() {
    use plurx_core::domain::{
        ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult, SubtitleStream,
    };
    use plurx_core::store::SqliteStore;

    let temp = crate::test_tempdir().expect("catalogue and source");
    let source_path = temp.path().join("source.mkv");
    std::fs::write(&source_path, b"portable-source-digest-fixture").expect("source");
    let catalog: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("catalogue"));
    let library = catalog
        .create_library(&NewLibrary {
            name: "Other node".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![temp.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = catalog
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Source".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let metadata = std::fs::metadata(&source_path).expect("metadata");
    let stamp = crate::fragment_index_cluster::source_stamp(&metadata);
    let file_id = catalog
        .upsert_file(
            item,
            source_path.to_str().expect("source path"),
            stamp.size as i64,
            stamp.mtime,
            &ProbeResult {
                container: Some("mkv".to_owned()),
                subtitle_streams: vec![SubtitleStream {
                    index: 0,
                    codec: "subrip".to_owned(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .expect("catalogued source");
    for key in [
        plurx_core::store::keys::SUBTITLE_STORED_SOURCES,
        plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES,
    ] {
        catalog.put_setting(key, "1").await.expect("switch");
    }
    let file = catalog
        .get_file(file_id)
        .await
        .expect("lookup")
        .expect("file");
    let digest = crate::fragment_index_cluster::attest_source("", &file, None, &|_| {})
        .await
        .expect("attest source")
        .observation
        .source_sha256;
    let mut row = SubtitleSourcePublication {
        file_id,
        source_size: stamp.size as i64,
        source_mtime: stamp.mtime,
        source_attestation: digest,
        node_id: "node-a".to_owned(),
        ordinal: 0,
        kind: "text".to_owned(),
        format: "webvtt".to_owned(),
        verdict: "empty".to_owned(),
        attempts: 1,
        origin: "extracted".to_owned(),
        sha256: String::new(),
        bytes: 0,
        published_at_ms: 1,
    };
    catalog
        .upsert_subtitle_source_publication(&row)
        .await
        .expect("node A publication");
    let gate = RideAlongGate::open(catalog.clone(), temp.path())
        .await
        .expect("ride enabled");
    let held = std::fs::File::open(&source_path).expect("held source");
    let tracks = [ProbedTrack {
        ordinal: 0,
        kind: ProbedKind::Text,
        stream_index: 1,
    }];
    assert!(plan_tracks(&gate, file_id, &held, &tracks).await.is_none());

    row.source_attestation = "f".repeat(64);
    catalog
        .upsert_subtitle_source_publication(&row)
        .await
        .expect("stale publication");
    assert!(plan_tracks(&gate, file_id, &held, &tracks).await.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn runaway_stage_files_stop_at_the_reserved_per_file_bound() {
    let base = crate::test_tempdir().expect("stage");
    let target = base.path().join("runaway.sup");
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", "head -c 16384 /dev/zero > \"$1\"", "stage-writer"])
        .arg(&target);
    super::configure_stage_file_limit(&mut command, 4096).expect("bounded child output");
    let status = command.status().await.expect("bounded child");
    assert!(
        !status.success(),
        "the OS refuses bytes beyond the reservation"
    );
    assert_eq!(
        std::fs::metadata(target).expect("bounded stage").len(),
        4096
    );
    // The child's limit must not lower the daemon's own file-size ceiling.
    let parent = base.path().join("parent");
    std::fs::write(&parent, [0_u8; 8192]).expect("parent remains unrestricted");
    assert_eq!(std::fs::metadata(parent).expect("parent file").len(), 8192);
}
