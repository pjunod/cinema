use super::*;
use crate::subtitle_source::testing::{kept, settled, stamp_of, write_manifest};
use crate::subtitle_source::{
    self as store, RepresentationEntry, RepresentationFormat, RepresentationOrigin, StoreAccess,
    TrackEntry, TrackKind, Verdict,
};
use sha2::Digest;

fn file_at(id: i64, path: PathBuf) -> MediaFile {
    let metadata = std::fs::metadata(&path).expect("source metadata");
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id,
        item_id: 1,
        size: metadata.len() as i64,
        mtime: crate::fragment_index_cluster::source_stamp(&metadata).mtime,
        path,
        duration_ms: Some(90_000),
        container: Some("mkv".into()),
        video_codec: Some("mpeg4".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(320),
        height: Some(180),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![plurx_core::domain::SubtitleStream {
            index: 1,
            codec: "hdmv_pgs_subtitle".into(),
            ..Default::default()
        }],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

fn run(command: &mut std::process::Command, what: &str) -> std::process::Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A PGS track whose first cue is at 60 s, muxed under a video whose
/// container starts at `start` seconds. `-copyts` so the times survive the
/// mux; the design's §6.2 fact 10 was only true of fixtures that kept them.
fn source_with_first_cue_at_sixty(dir: &Path, start: &str) -> PathBuf {
    let authored = dir.join("authored.sup");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/mkpgs");
    run(
        std::process::Command::new(script)
            .args(["1920", "1080"])
            .arg(&authored),
        "author the PGS track",
    );
    let source = dir.join(format!("source-{start}.mkv"));
    run(
        std::process::Command::new(ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-copyts",
                "-f",
                "lavfi",
                "-itsoffset",
                start,
                "-i",
                "color=c=black:s=320x180:r=10:d=90",
                // The authored track's first cue is at 1 s.
                "-itsoffset",
                "59",
                "-i",
            ])
            .arg(&authored)
            .args([
                "-map", "0:v:0", "-map", "1:s:0", "-c:v", "mpeg4", "-c:s", "copy",
            ])
            .arg(&source),
        "mux the fixture",
    );
    source
}

/// The `.sup` exactly as the producer will write it: `-c:s copy -f sup`,
/// and no `-copyts`.
fn ride_along_sup(dir: &Path, source: &Path) -> Vec<u8> {
    let sup = dir.join("ride-along.sup");
    run(
        std::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
            .arg(source)
            .args(["-map", "0:s:0", "-c:s", "copy", "-f", "sup"])
            .arg(&sup),
        "the ride-along extraction",
    );
    std::fs::read(sup).expect("ride-along bytes")
}

fn probe(path: &Path, entries: &[&str]) -> String {
    let output = run(
        std::process::Command::new(crate::ffmpeg::ffprobe_bin())
            .args(["-v", "error"])
            .args(entries)
            .args(["-of", "csv=p=0"])
            .arg(path),
        "ffprobe",
    );
    String::from_utf8(output.stdout).expect("utf8")
}

fn container_start(path: &Path) -> f64 {
    probe(path, &["-show_entries", "format=start_time"])
        .trim()
        .parse()
        .expect("start time")
}

/// Every subtitle packet's PTS, in order, as ffprobe prints it.
fn cue_times(path: &Path) -> Vec<String> {
    probe(
        path,
        &["-select_streams", "s:0", "-show_entries", "packet=pts_time"],
    )
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

fn burned(answer: BurnSource, to: &Path) -> Vec<String> {
    let BurnSource::File(mut handle) = answer else {
        panic!("expected a sidecar, the store answered nothing to burn");
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut handle, &mut bytes).expect("sidecar bytes");
    std::fs::write(to, bytes).expect("sidecar copy");
    cue_times(to)
}

fn on(root: &Path) -> StoreAccess {
    StoreAccess::new(root.to_owned(), true)
}

fn text_store(base: &Path, file_id: i64, verdict: Verdict) -> (MediaFile, StoreAccess) {
    let source = base.join("source.mkv");
    std::fs::write(&source, b"source bytes").expect("source");
    let file = file_at(file_id, source.clone());
    let root = base.join("runtime").join(store::STORE_DIR);
    let dir = store::file_dir(&root, file_id);
    std::fs::create_dir_all(&dir).expect("store dir");
    let bytes = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n";
    let sha256 = hex::encode(sha2::Sha256::digest(bytes));
    let name =
        store::representation_file_name(0, RepresentationFormat::Webvtt, &sha256).expect("name");
    if verdict == Verdict::Kept {
        std::fs::write(dir.join(&name), bytes).expect("VTT");
    }
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![TrackEntry {
            ordinal: 0,
            kind: TrackKind::Text,
            representations: vec![RepresentationEntry {
                format: RepresentationFormat::Webvtt,
                origin: RepresentationOrigin::Extracted,
                verdict,
                attempts: 1,
                file: (verdict == Verdict::Kept).then_some(name),
                sha256: (verdict == Verdict::Kept).then_some(sha256),
                bytes: if verdict == Verdict::Kept {
                    bytes.len() as u64
                } else {
                    0
                },
            }],
            verdict,
            attempts: 1,
            file: None,
            sha256: None,
        }],
    );
    (file, on(&root))
}

#[tokio::test]
async fn store_answer_does_not_start_vtt_flight() {
    let base = crate::test_tempdir().expect("fixture");
    let (file, access) = text_store(base.path(), 93_001, Verdict::Kept);
    let cache = base.path().join("subs");
    let cached = vtt_path(&cache, &file, 0);
    remember_failure(&cached, "a flight would hit this memo", NEGATIVE_TTL).await;
    let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect("stored VTT");
    assert_eq!(
        std::fs::read(path).expect("sidecar"),
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n"
    );
    assert_eq!(remembered_failure(&cached).await, None);
    assert!(!extractions().lock().await.contains_key(&cached));
}

#[tokio::test]
async fn webvtt_store_lookup_never_targets_burn_or_window_key() {
    let base = crate::test_tempdir().expect("fixture");
    let (file, access) = text_store(base.path(), 93_002, Verdict::Kept);
    let cache = base.path().join("subs");
    ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect("stored VTT");
    assert!(!vtt_window_path(&cache, &file, 0, 0, 200).exists());
    assert!(!cache.join(format!("f{}-s0-burn-v2.mks", file.id)).exists());
    assert_eq!(std::fs::read_dir(cache).expect("cache").count(), 1);
}

#[tokio::test]
async fn empty_stored_text_publishes_webvtt_without_flight() {
    let base = crate::test_tempdir().expect("fixture");
    let (file, access) = text_store(base.path(), 93_003, Verdict::Empty);
    let cache = base.path().join("subs");
    let cached = vtt_path(&cache, &file, 0);
    remember_failure(&cached, "a flight would hit this memo", NEGATIVE_TTL).await;
    let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect("empty VTT");
    assert_eq!(std::fs::read(path).expect("sidecar"), b"WEBVTT\n\n");
    assert_eq!(remembered_failure(&cached).await, None);
    assert!(!extractions().lock().await.contains_key(&cached));
}

#[tokio::test]
async fn styled_ass_stored_matroska_burn_matches_source_cues_and_style() {
    crate::transcode::require_ffmpeg();
    // Successful stored copies increment the same Burn hit counter as
    // joined flights; keep this writer inside the existing fixture guard.
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let authored = base.path().join("styled.ass");
    std::fs::write(&authored, "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 180\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.50,0:00:03.50,Default,,0,0,0,,{\\pos(120,90)}Positioned caption\n").expect("ASS");
    let source = base.path().join("styled.mkv");
    run(
        std::process::Command::new(ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=320x180:r=10:d=5",
                "-i",
            ])
            .arg(&authored)
            .args([
                "-map", "0:v:0", "-map", "1:s:0", "-c:v", "mpeg4", "-c:s", "ass",
            ])
            .arg(&source),
        "mux styled ASS fixture",
    );
    let file = file_at(93_004, source.clone());
    let direct_cache = base.path().join("direct");
    let direct = ensure_burn_source(
        &direct_cache,
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &StoreAccess::off(),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("direct burn");
    let BurnSource::File(mut direct) = direct else {
        panic!("direct burn file")
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut direct, &mut bytes).expect("direct bytes");
    assert!(bytes
        .windows(b"Positioned caption".len())
        .any(|part| part == b"Positioned caption"));
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    let name =
        store::representation_file_name(0, RepresentationFormat::Matroska, &sha256).expect("name");
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let local = store::file_dir(&root, file.id);
    std::fs::create_dir_all(&local).expect("local store");
    std::fs::write(local.join(&name), &bytes).expect("stored Matroska");
    write_manifest(
        &root,
        file.id,
        stamp_of(&source),
        vec![TrackEntry {
            ordinal: 0,
            kind: TrackKind::TextStyled,
            representations: vec![RepresentationEntry {
                format: RepresentationFormat::Matroska,
                origin: RepresentationOrigin::Extracted,
                verdict: Verdict::Kept,
                attempts: 1,
                file: Some(name),
                sha256: Some(sha256),
                bytes: bytes.len() as u64,
            }],
            verdict: Verdict::Kept,
            attempts: 1,
            file: None,
            sha256: None,
        }],
    );
    let stored_cache = base.path().join("stored");
    let stored = ensure_burn_source(
        &stored_cache,
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &on(&root),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("stored burn");
    let BurnSource::File(mut stored) = stored else {
        panic!("stored burn file")
    };
    let mut from_store = Vec::new();
    std::io::Read::read_to_end(&mut stored, &mut from_store).expect("stored bytes");
    assert_eq!(from_store, bytes, "styled Matroska is copied intact");
}

/// The design's required fixture (§6.2 fact 10, §6.7 item 1).
///
/// The derived `.mks` must put every cue exactly where today's source
/// extraction does, on a zero and a non-zero container start — and the
/// `-start_at_zero` variant, the source path's own argv and the obvious
/// thing to "harmonise" the derivation to, must not, so this cannot pass
/// on a fixture that happens to start its first cue at zero.
#[tokio::test]
async fn a_derived_burn_sidecar_keeps_the_source_extraction_cue_times() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    for (file_id, start) in [(91_001, "0"), (91_002, "7.5")] {
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), start);
        let offset = container_start(&source);
        if start == "0" {
            assert_eq!(offset, 0.0, "the zero-start fixture starts at zero");
        } else {
            assert!(offset > 7.0, "the fixture starts late: {offset}");
        }
        let file = file_at(file_id, source.clone());

        let today = burned(
            ensure_burn_source(
                &base.path().join("today"),
                &file,
                0,
                None,
                SIDECAR_JOIN_UNBOUNDED,
                &StoreAccess::off(),
                crate::process_control::ChildClass::Background,
            )
            .await
            .expect("today's extraction"),
            &base.path().join("today.mks"),
        );
        assert_eq!(
            today.len(),
            4,
            "two cues, each shown and cleared: {today:?}"
        );
        let first: f64 = today[0].parse().expect("first cue");
        assert!(
            (first - (60.0 - offset)).abs() < 0.001,
            "today's first cue sits at 60 s less the container start: {today:?}"
        );

        let root = base.path().join("runtime").join(store::STORE_DIR);
        let ride = ride_along_sup(base.path(), &source);
        let dir = store::file_dir(&root, file_id);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, &ride)],
        );
        let derived = burned(
            ensure_burn_source(
                &base.path().join("derived"),
                &file,
                0,
                None,
                SIDECAR_JOIN_UNBOUNDED,
                &on(&root),
                crate::process_control::ChildClass::Background,
            )
            .await
            .expect("derived sidecar"),
            &base.path().join("derived.mks"),
        );
        assert_eq!(
            burn_routes_for_test(file_id),
            vec!["source", "store"],
            "the second sidecar came from the store, not the source"
        );
        assert_eq!(derived, today, "container start {start}");

        // The variant this must not become.
        let stored = base.path().join("stored.sup");
        std::fs::write(&stored, &ride).expect("stored copy");
        let mut args = burn_derivation_args(&stored);
        let copyts = args
            .iter()
            .position(|arg| arg == "-copyts")
            .expect("the derivation keeps -copyts");
        assert!(
            !args.iter().any(|arg| arg == "-start_at_zero"),
            "the derivation must not carry -start_at_zero"
        );
        args.insert(copyts + 1, "-start_at_zero".into());
        let variant = base.path().join("start-at-zero.mks");
        *args.last_mut().expect("output") = variant.clone().into_os_string();
        run(
            std::process::Command::new(ffmpeg_bin()).args(&args),
            "the -start_at_zero variant",
        );
        let shifted = cue_times(&variant);
        assert_ne!(
            shifted, today,
            "with -start_at_zero every cue moves earlier by the first cue's time"
        );
        assert_eq!(shifted.first().map(String::as_str), Some("0.000000"));
    }
}

/// `empty` answers "nothing to burn" without reading the source, and the
/// off switch makes the same store invisible.
#[tokio::test]
async fn an_empty_stored_track_burns_nothing_without_reading_the_source() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let source = source_with_first_cue_at_sixty(base.path(), "0");
    let file_id = 91_003;
    let file = file_at(file_id, source.clone());
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let dir = write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![settled(0, Verdict::Empty)],
    );
    let cache = base.path().join("subs");

    let answer = ensure_burn_source(
        &cache,
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &on(&root),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("an answer");
    assert!(matches!(answer, BurnSource::Nothing));
    assert!(
        burn_routes_for_test(file_id).is_empty(),
        "nothing was extracted or derived"
    );
    assert!(
        !cache.exists() || std::fs::read_dir(&cache).expect("cache").next().is_none(),
        "and nothing was published"
    );
    assert!(dir.join(".access").exists(), "an answer marks its access");

    let answer = ensure_burn_source(
        &cache,
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &StoreAccess::new(root, false),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("off reads the source");
    assert!(matches!(answer, BurnSource::File(_)));
    assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
}

/// Off, and an MPEG-TS source, both ignore a valid kept track and a valid
/// empty one alike.
#[tokio::test]
async fn off_and_mpegts_never_use_the_store() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let source = source_with_first_cue_at_sixty(base.path(), "0");
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let ride = ride_along_sup(base.path(), &source);

    // Off, with a kept track.
    let file_id = 91_004;
    let file = file_at(file_id, source.clone());
    let dir = store::file_dir(&root, file_id);
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, &ride)],
    );
    let answer = ensure_burn_source(
        &base.path().join("off"),
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &StoreAccess::new(root.clone(), false),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("off");
    assert!(matches!(answer, BurnSource::File(_)));
    assert_eq!(burn_routes_for_test(file_id), vec!["source"]);

    // MPEG-TS, with a kept track and then an empty one.
    let file_id = 91_005;
    let mut file = file_at(file_id, source.clone());
    file.container = Some("m2ts".into());
    let dir = store::file_dir(&root, file_id);
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, &ride)],
    );
    let before = store::misses_for_test(store::Consumer::Burn, store::MissReason::Mpegts);
    let answer = ensure_burn_source(
        &base.path().join("mpegts-kept"),
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &on(&root),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("mpegts");
    assert!(matches!(answer, BurnSource::File(_)));
    assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
    assert!(store::misses_for_test(store::Consumer::Burn, store::MissReason::Mpegts) > before);

    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![settled(0, Verdict::Empty)],
    );
    let answer = ensure_burn_source(
        &base.path().join("mpegts-empty"),
        &file,
        0,
        None,
        SIDECAR_JOIN_UNBOUNDED,
        &on(&root),
        crate::process_control::ChildClass::Background,
    )
    .await
    .expect("mpegts empty");
    assert!(
        matches!(answer, BurnSource::File(_)),
        "an MPEG-TS source never takes the store's word that there is nothing"
    );
}

/// A derivation that fails falls back to the source inside the same
/// call, and the failure is never remembered: a memo written for it would
/// refuse the very extraction that answered.
#[tokio::test]
async fn a_failed_derivation_reads_the_source_in_the_same_flight_and_leaves_no_memo() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let source = source_with_first_cue_at_sixty(base.path(), "0");
    let file_id = 91_006;
    let file = file_at(file_id, source.clone());
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let dir = store::file_dir(&root, file_id);
    // Hash-valid, so it is opened and handed to ffmpeg, and not PGS, so
    // the derivation fails. (ffmpeg exits 0 on it with a cue-less
    // Matroska; the derivation counts what it printed as a failure.)
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, b"these bytes are not a PGS stream")],
    );
    let cache = base.path().join("subs");
    let before = store::fallbacks_for_test(store::Fallback::DeriveFailed);

    let today_cues = {
        let answer = ensure_burn_source(
            &cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &on(&root),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("the source answers in the same call");
        burned(answer, &base.path().join("fallback.mks"))
    };
    assert_eq!(
        burn_routes_for_test(file_id),
        vec!["source"],
        "the store was tried, failed, and the source ran in the same flight"
    );
    assert!(store::fallbacks_for_test(store::Fallback::DeriveFailed) > before);
    assert_eq!(
        today_cues.len(),
        4,
        "the source's cues, not an empty sidecar"
    );

    let version = {
        use sha2::{Digest, Sha256};
        let fence = crate::fragment_index_cluster::open_source_fence(&file, None)
            .await
            .expect("fence");
        hex::encode(Sha256::digest(fence.object_version().as_bytes()))
    };
    let cached = cache.join(format!("f{file_id}-s0-{version}-burn-v2.mks"));
    // The route assertion above is what catches a failed derivation
    // being made terminal; a memo check here could not fail, because the
    // successful source extraction clears the memo before it returns.
    // `a_double_failure_remembers_the_source_error_and_runs_neither_again`
    // is where the memo's contents are pinned.
    assert!(cached.exists(), "the source extraction was published");
    let mut entries = std::fs::read_dir(&cache).expect("cache");
    assert!(
        !entries.any(|entry| entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")),
        "and no temp file"
    );
}

/// The published burn sidecar's path for `file`, as the flight keys it.
async fn cached_burn(cache: &Path, file: &MediaFile) -> PathBuf {
    use sha2::{Digest, Sha256};
    let fence = crate::fragment_index_cluster::open_source_fence(file, None)
        .await
        .expect("fence");
    cache.join(format!(
        "f{}-s0-{}-burn-v2.mks",
        file.id,
        hex::encode(Sha256::digest(fence.object_version().as_bytes()))
    ))
}

/// A derivation that never returns is killed at its own bound and the
/// source answers in the same call. Without that bound it would run the
/// flight's timeout out — shrunk to five seconds here — and *that*
/// timeout is memoised, refusing the track for the memo's whole TTL: the
/// one route by which a derivation failure could reach the memo.
#[tokio::test]
async fn a_hung_derivation_is_bounded_and_falls_back_to_the_source() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let source = source_with_first_cue_at_sixty(base.path(), "0");
    let file_id = 91_007;
    let file = file_at(file_id, source.clone());
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let dir = store::file_dir(&root, file_id);
    let ride = ride_along_sup(base.path(), &source);
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, &ride)],
    );
    let cache = base.path().join("subs");
    let before = store::fallbacks_for_test(store::Fallback::TimedOut);

    let answer = Box::pin(ensure_burn_source_with(
        &cache,
        &file,
        0,
        None,
        &on(&root),
        ExtractionLimits {
            max_sidecar_bytes: MAX_BURN_BYTES,
            join_budget: SIDECAR_JOIN_UNBOUNDED,
            timeout: Duration::from_secs(5),
            ..Default::default()
        },
        Derivation {
            timeout: Duration::from_millis(200),
            hang: true,
        },
        crate::process_control::ChildClass::Background,
    ))
    .await
    .expect("the source answers once the derivation is cut off");
    assert!(matches!(answer, BurnSource::File(_)));
    assert_eq!(
        burn_routes_for_test(file_id),
        vec!["source"],
        "the hung derivation gave way to the source in the same flight"
    );
    assert!(store::fallbacks_for_test(store::Fallback::TimedOut) > before);
    let cached = cached_burn(&cache, &file).await;
    assert!(cached.exists(), "the source extraction was published");
}

/// A derivation that fails and a source extraction that then fails too:
/// the memo holds the *source's* error — the flight's final answer — and
/// the next call is refused from it without running either.
#[tokio::test]
async fn a_double_failure_remembers_the_source_error_and_runs_neither_again() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    // A source with no subtitle stream at all, so `-map 0:s:0` fails.
    let source = base.path().join("no-subtitles.mkv");
    run(
        std::process::Command::new(ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=320x180:r=1:d=5",
                "-c:v",
                "mpeg4",
            ])
            .arg(&source),
        "mux a source without subtitles",
    );
    let file_id = 91_008;
    let file = file_at(file_id, source.clone());
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let dir = store::file_dir(&root, file_id);
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, b"these bytes are not a PGS stream")],
    );
    let cache = base.path().join("subs");
    let limits = ExtractionLimits {
        max_sidecar_bytes: MAX_BURN_BYTES,
        join_budget: SIDECAR_JOIN_UNBOUNDED,
        negative_ttl: Duration::from_secs(60),
        ..Default::default()
    };
    let derivations_before = store::fallbacks_for_test(store::Fallback::DeriveFailed);

    let why = match Box::pin(ensure_burn_source_with(
        &cache,
        &file,
        0,
        None,
        &on(&root),
        limits,
        Derivation::default(),
        crate::process_control::ChildClass::Background,
    ))
    .await
    {
        Err(why) => why,
        Ok(_) => panic!("neither route can produce a sidecar"),
    };
    assert!(
        why.starts_with("burn-track extraction failed"),
        "the caller sees the source's error, not the derivation's: {why}"
    );
    assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
    let derivations = store::fallbacks_for_test(store::Fallback::DeriveFailed);
    assert_eq!(derivations, derivations_before + 1, "one derivation ran");
    let cached = cached_burn(&cache, &file).await;
    let remembered = remembered_failure(&cached).await.expect("a memo");
    assert!(
        remembered.starts_with("burn-track extraction failed"),
        "the memo is the source's error: {remembered}"
    );

    let again = Box::pin(ensure_burn_source_with(
        &cache,
        &file,
        0,
        None,
        &on(&root),
        limits,
        Derivation::default(),
        crate::process_control::ChildClass::Background,
    ))
    .await;
    assert!(matches!(again, Err(ref why) if why == &remembered));
    assert_eq!(
        burn_routes_for_test(file_id),
        vec!["source"],
        "the memo refused the call before either route ran"
    );
    assert_eq!(
        store::fallbacks_for_test(store::Fallback::DeriveFailed),
        derivations,
        "and no second derivation"
    );
    forget_failure(&cached).await;
}

/// A `kept` lookup is counted once per call even when the caller joins a
/// flight someone else owns and never opens the stored bytes itself.
#[tokio::test]
async fn a_kept_lookup_that_joins_another_flight_is_still_counted() {
    crate::transcode::require_ffmpeg();
    let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
    let base = crate::test_tempdir().expect("fixture");
    let source = source_with_first_cue_at_sixty(base.path(), "0");
    let file_id = 91_009;
    let file = file_at(file_id, source.clone());
    let root = base.path().join("runtime").join(store::STORE_DIR);
    let dir = store::file_dir(&root, file_id);
    let ride = ride_along_sup(base.path(), &source);
    write_manifest(
        &root,
        file_id,
        stamp_of(&source),
        vec![kept(&dir, 0, &ride)],
    );
    let cache = base.path().join("subs");
    let access = on(&root);
    let before = store::hits_for_test(store::Consumer::Burn);

    let (first, second) = tokio::join!(
        Box::pin(ensure_burn_source(
            &cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &access,
            crate::process_control::ChildClass::Background
        )),
        Box::pin(ensure_burn_source(
            &cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &access,
            crate::process_control::ChildClass::Background
        )),
    );
    assert!(matches!(first, Ok(BurnSource::File(_))));
    assert!(matches!(second, Ok(BurnSource::File(_))));
    assert_eq!(
        burn_routes_for_test(file_id),
        vec!["store"],
        "one derivation served both callers"
    );
    assert_eq!(
        store::hits_for_test(store::Consumer::Burn),
        before + 2,
        "both lookups counted, the joiner's included"
    );
}
