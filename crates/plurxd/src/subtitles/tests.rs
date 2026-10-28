/// The measurement this rule exists for, kept where it can be checked.
///
/// A subtitle-only extraction reads the container from its start to the
/// end of the requested span, so cost tracks the window's *position*, not
/// its length. Measured on a one-hour MKV with a 200-second window:
/// 7% of the file at the head, 23% at ten minutes, 57% at the midpoint,
/// 90% at fifty minutes, and 101% — the whole file, the same as extracting
/// the whole track — near the end.
///
/// Half is therefore where a window stops paying for itself, and past it
/// the whole-track warm is strictly better: same bytes, authoritative
/// result instead of a disposable one.
#[test]
fn a_window_is_declined_once_it_costs_what_the_whole_track_costs() {
    const HEALTHY: super::WholeTrackProgress = super::WholeTrackProgress::Healthy;
    let hour = 3_600;
    let w = super::WINDOW_SECONDS_DEFAULT;
    assert!(super::windowing_is_worthwhile(0, hour, w, HEALTHY));
    assert!(super::windowing_is_worthwhile(600, hour, w, HEALTHY));
    assert!(super::windowing_is_worthwhile(1_799, hour, w, HEALTHY));
    // The midpoint is where the measured cost reaches the whole track's
    // neighbourhood; at and past it, decline.
    assert!(!super::windowing_is_worthwhile(1_800, hour, w, HEALTHY));
    assert!(!super::windowing_is_worthwhile(3_000, hour, w, HEALTHY));
    assert!(!super::windowing_is_worthwhile(3_600, hour, w, HEALTHY));
    // A duration we do not know is not a duration of zero: there is no
    // midpoint to compare against, so there is no saving to claim.
    assert!(!super::windowing_is_worthwhile(0, 0, w, HEALTHY));
    assert!(!super::windowing_is_worthwhile(10, -1, w, HEALTHY));
    // A file barely longer than one window has no head to bridge — the
    // window would extract the whole track alongside the warm doing the
    // same scan, and publish the loser under a disposable key.
    assert!(!super::windowing_is_worthwhile(0, w, w, HEALTHY));
    assert!(!super::windowing_is_worthwhile(0, w * 2, w, HEALTHY));
    assert!(super::windowing_is_worthwhile(0, w * 2 + 1, w, HEALTHY));
    // A position before the file is a client bug. It is clamped rather
    // than trusted, in both the grid and the warmer, so neither a negative
    // filename nor a negative `-ss` can be produced.
    assert_eq!(super::window_anchor_seconds(-500, w), 0);

    // The midpoint rule's reason is that the whole-track warm reads the
    // same bytes and publishes the authoritative answer. When that warm
    // has failed, or has been grinding for longer than the window it is
    // being compared against, there is nothing left to be redundant with
    // — and declining means the back half of the film never gets
    // subtitles at all.
    let stalled = super::WholeTrackProgress::Stalled;
    assert!(super::windowing_is_worthwhile(1_800, hour, w, stalled));
    assert!(super::windowing_is_worthwhile(3_000, hour, w, stalled));
    // The other clause is not about the whole track and stays absolute: a
    // file barely longer than one window has no head to bridge however
    // the whole-track warm is doing.
    assert!(!super::windowing_is_worthwhile(0, w * 2, w, stalled));
    assert!(!super::windowing_is_worthwhile(0, 0, w, stalled));
}

/// The base of a windowed extraction depends on the ffmpeg build, and the
/// build is deliberately unpinned.
///
/// Measured on `-i src -ss A -to B`: ffmpeg 7.0.2 and 6.1.1 return
/// absolute cue times, 4.4.2 returns them rebased to zero. The fleet runs
/// 7.1+ — the Dockerfile fails the build without `dovi_rpu` — so the
/// shipped path is the absolute one and normalization is a no-op there.
/// Two reviewers measuring on different builds reached opposite
/// conclusions, which is why the code no longer depends on either.
///
/// Detection is exact rather than heuristic: anchors are grid multiples,
/// so a non-zero anchor is at least one window long, and a rebased
/// window's cues in `[0, span)` cannot overlap an absolute window's in
/// `[anchor, anchor + span)`.
#[test]
fn a_window_is_normalized_to_absolute_time_whatever_ffmpeg_emitted() {
    let rebased = b"WEBVTT\n\n00:00:10.000 --> 00:00:12.000\nfirst\n\n00:00:50.000 --> 00:00:52.000\nsecond\n";
    let absolute = b"WEBVTT\n\n00:03:30.000 --> 00:03:32.000\nfirst\n\n00:04:10.000 --> 00:04:12.000\nsecond\n";

    // A rebased window at anchor 200 is shifted onto the source clock.
    assert_eq!(
        super::normalize_window_cues(rebased, 200),
        absolute.to_vec(),
        "a rebased window must be shifted by its anchor"
    );
    // An absolute window is already right and must not be shifted twice —
    // the failure that would produce is invisible, because the result is
    // still well-formed WebVTT.
    assert_eq!(
        super::normalize_window_cues(absolute, 200),
        absolute.to_vec(),
        "an absolute window must be left alone"
    );
    // At anchor zero the two bases are the same answer.
    assert_eq!(super::normalize_window_cues(rebased, 0), rebased.to_vec());

    // Cue settings ride after the end timestamp and must survive.
    let styled = b"WEBVTT\n\n00:00:10.000 --> 00:00:12.000 line:90% align:center\nx\n";
    let shifted = super::normalize_window_cues(styled, 200);
    let shifted = String::from_utf8(shifted).expect("utf8");
    assert!(
        shifted.contains("00:03:30.000 --> 00:03:32.000 line:90% align:center"),
        "settings must survive the shift: {shifted}"
    );

    // A window with no cues at all is not evidence of either base, and
    // must not be corrupted on the way through.
    assert_eq!(
        super::normalize_window_cues(b"WEBVTT\n\n", 200),
        b"WEBVTT\n\n".to_vec()
    );
}

/// Requests inside one window must share a cache key, or every segment
/// request starts its own extraction and the dedup registry never sees
/// that two of them want the same bytes.
#[test]
fn demand_positions_inside_one_window_share_an_anchor() {
    assert_eq!(super::window_anchor_seconds(0, 200), 0);
    assert_eq!(super::window_anchor_seconds(199, 200), 0);
    assert_eq!(super::window_anchor_seconds(200, 200), 200);
    assert_eq!(super::window_anchor_seconds(399, 200), 200);
    assert_eq!(super::window_anchor_seconds(1_000, 200), 1_000);
    // The configured length is clamped before it is used as a grid, so a
    // nonsense setting cannot produce a nonsense anchor or a divide by
    // zero.
    assert_eq!(super::bounded_window_seconds(0), super::WINDOW_SECONDS_MIN);
    assert_eq!(
        super::bounded_window_seconds(100_000),
        super::WINDOW_SECONDS_MAX
    );
    assert_eq!(super::window_anchor_seconds(100, 0), 90);
    assert_eq!(super::bounded_window_seconds(200), 200);
}

/// The prune must be able to tell the two shapes apart, or it evicts the
/// expensive sidecars to make room for the cheap ones.
#[test]
fn a_window_name_is_distinguishable_from_a_whole_track_name() {
    assert!(super::is_window_name("f7-s0-11-13-w200-200.vtt"));
    assert!(super::is_window_name("f7-s0-11-13-w0-30.vtt"));
    // The whole-track shape ends in the source mtime: no `w`, one fewer
    // separator. Misreading one as a window would put it on the small
    // budget and evict it early.
    assert!(!super::is_window_name("f7-s0-11-13.vtt"));
    assert!(!super::is_window_name("f7-s0-12345-678.vtt"));
    assert!(!super::is_window_name("f7-s0-11-13-w200-200.txt"));
    assert!(!super::is_window_name(".tmp-abc.vtt"));
    assert!(!super::is_window_name("nonsense"));
    // And the real names agree with the predicate.
    let dir = std::path::Path::new("/tmp/subs");
    let file = media_file(PathBuf::from("/tmp/source.mkv"));
    let window = super::vtt_window_path(dir, &file, 0, 200, 200);
    let whole = super::vtt_path(dir, &file, 0);
    assert!(super::is_window_name(
        &window
            .file_name()
            .expect("window path has a file name")
            .to_string_lossy()
    ));
    assert!(!super::is_window_name(
        &whole
            .file_name()
            .expect("whole-track path has a file name")
            .to_string_lossy()
    ));
}

/// Two spans of the same track are two answers, not one answer to
/// overwrite — the configured length can change under a running server.
#[test]
fn a_window_sidecar_is_keyed_by_its_span() {
    let dir = std::path::Path::new("/tmp/subs");
    let file = media_file(PathBuf::from("/tmp/source.mkv"));
    let at = |anchor, window| super::vtt_window_path(dir, &file, 0, anchor, window);
    assert_ne!(at(0, 200), at(200, 200));
    assert_ne!(at(0, 200), at(0, 400));
    assert_eq!(at(200, 200), at(200, 200));
    // And never collides with the whole-track key for the same source.
    assert_ne!(at(0, 200), super::vtt_path(dir, &file, 0));
}

use super::*;

#[tokio::test]
async fn ready_window_to_whole_track_changes_revision_without_a_warming_edge() {
    let dir = crate::test_tempdir().expect("readiness identity fixture");
    let file = media_file(dir.path().join("source.mkv"));
    let access = crate::subtitle_source::StoreAccess::off();
    let observe =
        |position| readiness_revision_with_store(dir.path(), &file, 0, position, 200, &access);
    assert_eq!(observe(0).await, (SidecarState::Absent, None));
    let vtt = b"WEBVTT\n\n00:00:01.000 --> 00:00:03.000\ncaption\n\n";
    tokio::fs::write(vtt_window_path(dir.path(), &file, 0, 0, 200), vtt)
        .await
        .expect("window");
    let first = observe(0).await;
    assert_eq!(first.0, SidecarState::Ready);
    assert_eq!(
        first,
        observe(100).await,
        "cadence in the same window is stable"
    );
    tokio::fs::write(vtt_window_path(dir.path(), &file, 0, 200, 200), vtt)
        .await
        .expect("next window");
    let next = observe(200).await;
    assert_eq!(next.0, SidecarState::Ready);
    assert_ne!(first.1, next.1);
    tokio::fs::write(vtt_path(dir.path(), &file, 0), vtt)
        .await
        .expect("whole track");
    let whole = observe(200).await;
    assert_eq!(whole.0, SidecarState::Ready);
    assert_ne!(
        next.1, whole.1,
        "a full track can repair previously empty buffered ranges"
    );
    assert_eq!(
        whole,
        observe(600).await,
        "a whole track does not retry on every window"
    );
}

#[tokio::test]
async fn cached_readiness_revision_delivers_exact_window_without_whole_extraction() {
    let dir = crate::test_tempdir().expect("revision fixture");
    let file = media_file(dir.path().join("absent-source.mkv"));
    let vtt = b"WEBVTT\n\n00:03:21.000 --> 00:03:23.000\nwindow caption\n\n";
    let window = vtt_window_name(file.id, 0, file.size, file.mtime, 200, 200);
    tokio::fs::write(vtt_window_path(dir.path(), &file, 0, 200, 200), vtt)
        .await
        .expect("window");
    assert_eq!(
        read_cached_revision(dir.path(), &file, 0, &window)
            .await
            .expect("read"),
        Some((vtt.to_vec(), false))
    );
    let whole = vtt_name(file.id, 0, file.size, file.mtime);
    assert!(read_cached_revision(dir.path(), &file, 0, &whole)
        .await
        .expect("read")
        .is_none());
    assert!(
        !vtt_path(dir.path(), &file, 0).exists(),
        "delivery never starts whole extraction"
    );
    for invalid in [
        "../outside.vtt".to_owned(),
        window.replace("-w200-", "-w201-"),
        window.replace("-w200-", "-w0200-"),
        vtt_window_name(file.id, 0, file.size, file.mtime, 200, 0),
    ] {
        assert!(read_cached_revision(dir.path(), &file, 0, &invalid)
            .await
            .expect("read")
            .is_none());
    }
    assert!(read_cached_revision(dir.path(), &file, 1, &window)
        .await
        .expect("other track")
        .is_none());
    let mut replaced = file.clone();
    replaced.mtime += 1;
    assert!(read_cached_revision(dir.path(), &replaced, 0, &window)
        .await
        .expect("other source")
        .is_none());
    tokio::fs::write(vtt_path(dir.path(), &file, 0), vtt)
        .await
        .expect("whole");
    assert_eq!(
        read_cached_revision(dir.path(), &file, 0, &whole)
            .await
            .expect("whole read"),
        Some((vtt.to_vec(), true))
    );
}

#[tokio::test]
async fn same_window_new_track_or_source_does_not_join_obsolete_extraction() {
    let dir = crate::test_tempdir().expect("subtitle identity fixture");
    let mut file = media_file(dir.path().join("source.mkv"));
    file.container = Some("mkv".into());
    file.duration_ms = Some(3_600_000);
    file.subtitle_streams = vec![
        plurx_core::domain::SubtitleStream {
            codec: "subrip".into(),
            ..Default::default()
        },
        plurx_core::domain::SubtitleStream {
            codec: "subrip".into(),
            ..Default::default()
        },
    ];
    for replace_source in [false, true] {
        let session = uuid::Uuid::new_v4().to_string();
        assert!(
            warm_vtt_window_with(
                &session,
                Some(1),
                dir.path(),
                &file,
                0,
                0,
                200,
                |_, _, _, _, _| std::future::pending::<Result<(), String>>()
            )
            .await
        );
        let old = vtt_window_path(dir.path(), &file, 0, 0, 200);
        let index = if replace_source {
            file.mtime += 1;
            0
        } else {
            1
        };
        let new = vtt_window_path(dir.path(), &file, index, 0, 200);
        assert!(
            warm_vtt_window_with(
                &session,
                Some(2),
                dir.path(),
                &file,
                index,
                0,
                200,
                |tmp, _, _, _, _| async move {
                    tokio::fs::write(
                        tmp,
                        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nselected track\n\n",
                    )
                    .await
                    .map_err(|e| e.to_string())
                }
            )
            .await
        );
        let published = tokio::time::timeout(Duration::from_secs(2), async {
            while !new.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        release_session_window(&session).await;
        assert!(
            published.is_ok(),
            "a changed track/source was incorrectly joined to the old window"
        );
        assert!(!old.exists(), "superseded extraction must not publish");
        assert!(peak_window_flights_for_test(&session) <= 1);
        tokio::fs::remove_file(new)
            .await
            .expect("remove fixture window");
    }
}

#[tokio::test]
async fn downloaded_captions_rebuild_cache_without_embedded_streams_or_provider() {
    let dir = crate::test_tempdir().expect("downloaded caption fixture");
    let mut file = media_file(dir.path().join("not-an-embedded-subtitle.mkv"));
    let vtt =
        "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nFirst\n\n00:02:00.000 --> 00:02:05.000\nLater\n";
    file.subtitle_streams = vec![plurx_core::domain::SubtitleStream {
        index: 0,
        codec: "webvtt".into(),
        ..Default::default()
    }];
    file.downloaded_subtitles = vec![plurx_core::domain::DownloadedSubtitle {
        text_translation: None,
        source_size: file.size,
        source_mtime: file.mtime,
        provider_file_id: 1,
        transcription: None,
        language: "en".into(),
        title: "Example".into(),
        hearing_impaired: false,
        forced: false,
        vtt: vtt.into(),
    }];
    assert_eq!(
        read_cached_vtt(dir.path(), &file, 0)
            .await
            .expect("downloaded caption fixture")
            .expect("downloaded caption fixture"),
        vtt.as_bytes()
    );
    assert!(matches!(
        sidecar_state(dir.path(), &file, 0).await,
        SidecarState::Ready
    ));
    assert_eq!(
        ensure_vtt_bytes(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
            .await
            .expect("downloaded caption fixture"),
        vtt.as_bytes()
    );
    tokio::fs::remove_file(vtt_path(dir.path(), &file, 0))
        .await
        .expect("downloaded caption fixture");
    assert_eq!(
        ensure_vtt_bytes(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
            .await
            .expect("downloaded caption fixture"),
        vtt.as_bytes()
    );
    let window = dir.path().join("window.vtt");
    extract_vtt_window(&window, &file, 0, 200, 200)
        .await
        .expect("downloaded caption fixture");
    assert_eq!(
        tokio::fs::read(window)
            .await
            .expect("downloaded caption fixture"),
        vtt.as_bytes(),
        "absolute cues are never shifted to a later seek window"
    );
}

fn media_file(path: PathBuf) -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 77,
        item_id: 1,
        path,
        size: 12_345,
        mtime: 678,
        duration_ms: Some(60_000),
        container: Some("mkv".into()),
        video_codec: Some("h264".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(8_000_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

#[test]
fn production_window_future_keeps_copy_buffers_off_the_poll_stack() {
    // Construct the real decoder and range-preparing owner futures without
    // polling them: this checks their production state layout without IO.
    // The inline 64 KiB copy buffer made nested debug poll frames exceed a
    // 2 MiB worker stack. Leave ample room for bounded metadata and handles,
    // while refusing even one such buffer embedded in future state.
    const FUTURE_BUDGET: usize = 64 * 1024;
    let dir = Path::new("unpolled-window-cache");
    let tmp = dir.join("unpolled-window.vtt");
    let file = media_file(PathBuf::from("unpolled-source.mkv"));
    let extraction = extract_vtt_window(&tmp, &file, 0, 0, 200);
    let access = crate::subtitle_source::StoreAccess::off();
    let dir_owned = dir.to_owned();
    let (_sender, cancel) = tokio::sync::oneshot::channel();
    let owned = ensure_window_owned(
        tmp.clone(),
        dir,
        &file,
        0,
        ExtractionLimits::default(),
        cancel,
        move |tmp, file, ordinal| async move {
            crate::subtitle_ranges::prepare(&access, &dir_owned, &tmp, &file, ordinal, 0, 200).await
        },
    );
    let extraction_bytes = std::mem::size_of_val(&extraction);
    let owned_bytes = std::mem::size_of_val(&owned);
    println!("production window future bytes: extraction={extraction_bytes}, owner={owned_bytes}");
    assert!(
        extraction_bytes < FUTURE_BUDGET,
        "decoder future holds {extraction_bytes} bytes, budget {FUTURE_BUDGET}"
    );
    assert!(
        owned_bytes < FUTURE_BUDGET,
        "range-preparing window owner holds {owned_bytes} bytes, budget {FUTURE_BUDGET}"
    );
}

/// The midpoint rule declines a window because the whole-track warm reads
/// the same bytes and publishes the authoritative answer instead. That
/// reason is about the warm being alive: once it has failed there is
/// nothing left to be redundant with, and declining meant the back half
/// of the film had no subtitles for good.
///
/// Driven through `warm_vtt_window_with`, which is where the rule is
/// actually consulted — the HTTP boundary only forwards the anchor.
#[tokio::test]
async fn a_failed_whole_track_lets_a_window_past_the_midpoint_start() {
    let dir = tempfile::tempdir().expect("cache dir");
    let mut file = media_file(dir.path().join("past-midpoint.mkv"));
    // An hour, so the midpoint is well clear of the window span below.
    file.duration_ms = Some(3_600_000);
    let session = &format!("past-midpoint-{}", uuid::Uuid::new_v4());
    let anchor = 3_000; // past 1,800 s
    let window = 200;

    let never = |_tmp: PathBuf, _f: MediaFile, _i: i64, _a: i64, _w: i64| async move {
        std::future::pending::<()>().await;
        Ok(())
    };

    assert!(
        !warm_vtt_window_with(session, None, dir.path(), &file, 0, anchor, window, never).await,
        "a healthy whole-track warm keeps the midpoint rule"
    );
    assert_eq!(
        peak_window_flights_for_test(session),
        0,
        "and starts nothing at all"
    );

    remember_whole_track_failure_for_test(
        dir.path(),
        &file,
        0,
        "the source could not be read",
        Duration::from_secs(90),
    )
    .await;

    assert!(
        warm_vtt_window_with(session, None, dir.path(), &file, 0, anchor, window, never).await,
        "a dead whole-track warm is not a reason to leave the second half blank"
    );
    // The flight is owned the moment `warm_vtt_window_with` returns true;
    // the producer itself starts one poll later, which is what this yield
    // is for. The count is the contract — one per playback, never two.
    tokio::task::yield_now().await;
    assert_eq!(
        peak_window_flights_for_test(session),
        1,
        "and it is still one flight per playback"
    );

    release_session_window(session).await;
    forget_failure(&vtt_path(dir.path(), &file, 0)).await;
}

#[test]
fn cache_key_changes_with_source_identity_and_track() {
    let first = vtt_name(42, 2, 1_000, 200);
    assert_eq!(first, "f42-s2-1000-200.vtt");
    assert_ne!(first, vtt_name(42, 3, 1_000, 200));
    assert_ne!(first, vtt_name(42, 2, 1_000, 201));
}

#[tokio::test]
async fn subtitle_readiness_names_only_the_window_that_can_serve_it() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let whole = vtt_path(dir.path(), &file, 0);
    let wanted = vtt_window_path(dir.path(), &file, 0, 0, 200);
    let unrelated = vtt_window_path(dir.path(), &file, 0, 200, 200);

    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Absent
    );

    tokio::fs::write(&unrelated, b"WEBVTT\n\nunrelated\n")
        .await
        .expect("unrelated window");
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Absent,
        "a neighbouring window cannot make this demand ready"
    );

    warmups().lock().await.insert(whole.clone());
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Warming
    );
    warmups().lock().await.remove(&whole);

    extractions().lock().await.insert(
        wanted.clone(),
        Arc::new(Extraction {
            result: tokio::sync::Mutex::new(None),
            ready: tokio::sync::Notify::new(),
        }),
    );
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Warming
    );
    extractions().lock().await.remove(&wanted);

    remember_failure(&whole, "failed", Duration::from_secs(60)).await;
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Failed
    );
    forget_failure(&whole).await;

    tokio::fs::write(&wanted, b"WEBVTT\n\n00:00:12.000 --> 00:00:13.000\nready\n")
        .await
        .expect("wanted window");
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Ready
    );
    tokio::fs::remove_file(&wanted)
        .await
        .expect("remove window");

    tokio::fs::write(&whole, b"WEBVTT\n\nwhole\n")
        .await
        .expect("whole track");
    assert_eq!(
        sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
        SidecarState::Ready
    );
}

#[tokio::test]
async fn whole_track_publication_prunes_only_its_fingerprint_windows() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let mut other = file.clone();
    other.mtime += 1;
    let first = vtt_window_path(dir.path(), &file, 0, 0, 200);
    let second = vtt_window_path(dir.path(), &file, 0, 200, 200);
    let other_window = vtt_window_path(dir.path(), &other, 0, 0, 200);
    for path in [&first, &second, &other_window] {
        tokio::fs::write(path, b"WEBVTT\n\nwindow\n")
            .await
            .expect("window sidecar");
    }

    ensure_vtt_with(dir.path(), &file, 0, |tmp, _, _| async move {
        tokio::fs::write(tmp, b"WEBVTT\n\nwhole\n")
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .expect("publish whole track");

    assert!(tokio::fs::metadata(&first).await.is_err());
    assert!(tokio::fs::metadata(&second).await.is_err());
    assert!(
        tokio::fs::metadata(&other_window).await.is_ok(),
        "another source fingerprint keeps its windows"
    );
}

#[tokio::test]
async fn cold_extraction_is_deduplicated_and_survives_waiter_cancellation() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();

    let first = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            ensure_vtt_with(&dir, &file, 0, move |tmp, _, _| async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = started_tx.send(());
                let _permit = release.acquire().await.expect("release");
                tokio::fs::write(tmp, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
        })
    };
    started_rx.await.expect("extraction started");

    // A second cold request joins the same extraction. Dropping the first
    // simulates AVPlayer timing out while ffmpeg is still reading the MKV.
    let second = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        tokio::spawn(async move {
            ensure_vtt_with(&dir, &file, 0, move |_, _, _| async move {
                runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                Err("a deduplicated runner must never execute".into())
            })
            .await
        })
    };
    first.abort();
    release.add_permits(1);

    // A LIVENESS bound, not a latency one: the assertion is that an
    // aborted waiter does not strand the extraction, and any finite
    // deadline proves that. Two seconds did not — this suite runs on
    // 2-core CI runners, and a loaded box missed it while the code was
    // perfectly correct. Generous enough to never lie, short enough that
    // a genuine hang still fails the run instead of hanging it.
    let published = tokio::time::timeout(std::time::Duration::from_secs(30), second)
        .await
        .expect("an aborted waiter must not strand the extraction")
        .expect("waiter task")
        .expect("published VTT");
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only the owner runs extraction"
    );
    assert_eq!(published, vtt_path(dir.path(), &file, 0));
    assert!(tokio::fs::metadata(&published).await.is_ok());

    let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read cache");
    while let Some(entry) = entries.next_entry().await.expect("entry") {
        assert!(
            !entry.file_name().to_string_lossy().starts_with(".tmp-"),
            "a completed extraction must not remain unpublished"
        );
    }
}

#[tokio::test]
async fn concurrent_invalid_sidecar_repair_cannot_remove_the_replacement() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let cached = vtt_path(dir.path(), &file, 0);
    tokio::fs::write(&cached, vec![b'x'; 65])
        .await
        .expect("oversized stale sidecar");
    let limits = ExtractionLimits {
        max_sidecar_bytes: 64,
        ..ExtractionLimits::default()
    };
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let first = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            ensure_vtt_bounded(&dir, &file, 0, limits, move |tmp, _, _| async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = started_tx.send(());
                let _permit = release.acquire().await.expect("release repair");
                tokio::fs::write(tmp, b"WEBVTT\n")
                    .await
                    .map_err(|error| error.to_string())
            })
            .await
        })
    };
    started_rx.await.expect("repair started");
    let second = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        tokio::spawn(async move {
            ensure_vtt_bounded(&dir, &file, 0, limits, move |_, _, _| async move {
                runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                Err("concurrent repair must join the owner".to_owned())
            })
            .await
        })
    };
    tokio::task::yield_now().await;
    release.add_permits(1);
    assert_eq!(
        first.await.expect("first waiter").expect("first repair"),
        cached
    );
    assert_eq!(
        second.await.expect("second waiter").expect("joined repair"),
        cached
    );
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        tokio::fs::read(&cached).await.expect("published sidecar"),
        b"WEBVTT\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn http_sidecar_reads_reject_a_symlink_replacement() {
    let dir = crate::test_tempdir().expect("cache");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    std::fs::write(outside.path(), b"outside subtitle bytes").expect("outside bytes");
    let file = media_file(dir.path().join("source.mkv"));
    let cached = vtt_path(dir.path(), &file, 0);
    tokio::fs::write(&cached, b"WEBVTT\n\noriginal\n")
        .await
        .expect("warm sidecar");
    tokio::fs::remove_file(&cached)
        .await
        .expect("remove warm path");
    std::os::unix::fs::symlink(outside.path(), &cached).expect("replace with symlink");

    let observed = read_cached_vtt(dir.path(), &file, 0).await;
    assert!(
        !matches!(observed, Ok(Some(bytes)) if bytes == b"outside subtitle bytes"),
        "the HTTP path must never follow a replacement sidecar symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn burn_in_keeps_the_exact_bounded_sidecar_handle() {
    use std::io::{Read, Seek};

    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let cached = vtt_path(dir.path(), &file, 0);
    let original = b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\noriginal\n";
    tokio::fs::write(&cached, original)
        .await
        .expect("warm sidecar");
    let mut held = ensure_vtt_file(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
        .await
        .expect("held subtitle handle");
    let replacement = dir.path().join("replacement.vtt");
    tokio::fs::write(&replacement, b"WEBVTT\n\nreplacement\n")
        .await
        .expect("replacement bytes");
    tokio::fs::rename(&replacement, &cached)
        .await
        .expect("replace sidecar path");

    held.rewind().expect("rewind held sidecar");
    let mut bytes = Vec::new();
    held.read_to_end(&mut bytes).expect("read held sidecar");
    assert_eq!(bytes, original);
    assert_ne!(
        tokio::fs::read(&cached).await.expect("replacement"),
        original
    );
}

#[cfg(unix)]
#[tokio::test]
async fn extraction_never_publishes_a_symlink_swapped_temp_file() {
    let dir = crate::test_tempdir().expect("cache");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    std::fs::write(outside.path(), b"WEBVTT\n\noutside\n").expect("outside bytes");
    let file = media_file(dir.path().join("source.mkv"));
    let outside_path = outside.path().to_owned();
    let result = ensure_vtt_bounded(
        dir.path(),
        &file,
        0,
        ExtractionLimits::default(),
        move |tmp, _, _| async move {
            std::os::unix::fs::symlink(outside_path, tmp).map_err(|error| error.to_string())
        },
    )
    .await;

    assert!(result.is_err());
    assert!(tokio::fs::symlink_metadata(vtt_path(dir.path(), &file, 0))
        .await
        .is_err());
    assert_eq!(
        std::fs::read(outside.path()).expect("outside survives"),
        b"WEBVTT\n\noutside\n"
    );
}

/// Real time, deliberately: the bounds under test are injected small, so
/// the suite proves them in milliseconds without a paused clock — which
/// here would be a trap, since every task in this test parks in file IO
/// and the runtime would leap straight to the extraction deadline.
fn wedged_limits() -> ExtractionLimits {
    ExtractionLimits {
        timeout: Duration::from_millis(150),
        negative_ttl: Duration::from_secs(60),
        ..ExtractionLimits::default()
    }
}

/// A cold sidecar on a large source is not a failure and must not be paid
/// for by whoever asked first.
///
/// The extraction reads the whole container — 79.5 GB for file 5208, 402 s
/// measured, to produce 18,866 bytes — so a caller that waits for it
/// inherits a runtime no start budget can contain. The wait is bounded and
/// the *extraction* is not: it must still finish, publish, and be there
/// for the next caller.
#[tokio::test]
async fn a_cold_sidecar_gives_the_caller_its_budget_back_and_keeps_extracting() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let limits = || ExtractionLimits {
        join_budget: Duration::from_millis(50),
        ..ExtractionLimits::default()
    };
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let finished_for_run = Arc::clone(&finished);
    let slow = ensure_vtt_bounded(
        dir.path(),
        &file,
        0,
        limits(),
        move |tmp, _, _| async move {
            // Longer than the join budget, shorter than the extraction's own.
            let _ = release_rx.await;
            tokio::fs::write(&tmp, b"WEBVTT\n\ncold\n")
                .await
                .map_err(|error| error.to_string())?;
            finished_for_run.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    )
    .await;

    let why = slow.expect_err("a caller must not inherit a full-source read");
    assert!(
        is_sidecar_pending_error(&why),
        "the refusal names a pending artifact rather than a failure, got {why}"
    );

    // Nothing was cancelled: releasing the extraction still publishes.
    release_tx.send(()).expect("release the extraction");
    for _ in 0..200 {
        if finished.load(std::sync::atomic::Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        finished.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "bounding the wait must not abandon the extraction"
    );

    // And the next caller finds it warm, which is the whole point of
    // letting the detached extraction run on. This one waits on the
    // unbounded budget on purpose: it is asserting that the work was not
    // abandoned, not racing the publish that proves it.
    let warm = ensure_vtt_bounded(
        dir.path(),
        &file,
        0,
        ExtractionLimits::default(),
        move |_, _, _| async move { Err("a published sidecar must not be re-extracted".into()) },
    )
    .await
    .expect("the published sidecar is served to the next caller");
    assert_eq!(
        tokio::fs::read(&warm).await.expect("published sidecar"),
        b"WEBVTT\n\ncold\n"
    );
}

/// P1-4: a stalled NAS read used to park every waiter forever on
/// `notified()`, and each following segment request relaunched the same
/// doomed multi-GB scan. Both waiters must come back with a bounded error,
/// and the request after them must be answered from the memo.
#[tokio::test]
async fn a_wedged_extraction_times_out_and_the_failure_is_memoized() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();

    let first = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        tokio::spawn(async move {
            ensure_vtt_bounded(&dir, &file, 0, wedged_limits(), move |_, _, _| async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = started_tx.send(());
                // The wedge: an ffmpeg that never returns and never errors.
                tokio::time::sleep(Duration::from_secs(3_600)).await;
                Ok(())
            })
            .await
        })
    };
    started_rx.await.expect("extraction started");

    // Joins the same flight, the way a second segment request would.
    let second = {
        let dir = dir.path().to_owned();
        let file = file.clone();
        let runs = Arc::clone(&runs);
        tokio::spawn(async move {
            ensure_vtt_bounded(&dir, &file, 0, wedged_limits(), move |_, _, _| async move {
                runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                Err("a deduplicated runner must never execute".into())
            })
            .await
        })
    };

    for waiter in [first, second] {
        let observed = tokio::time::timeout(Duration::from_secs(10), waiter)
            .await
            .expect("a wedged producer must not park its waiters")
            .expect("waiter task");
        let why = observed.expect_err("a wedged extraction cannot publish");
        assert!(
            why.contains("timed out"),
            "waiters observe the bounded error, got {why}"
        );
    }

    // The memo is written before the flight is dropped, so by the time a
    // waiter has its answer a fresh request is already covered.
    let runs_for_repeat = Arc::clone(&runs);
    let repeat = ensure_vtt_bounded(
        dir.path(),
        &file,
        0,
        wedged_limits(),
        move |_, _, _| async move {
            runs_for_repeat.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
            Err("a memoized failure must not relaunch the scan".into())
        },
    )
    .await;
    assert!(repeat.is_err(), "the memo answers with the same failure");
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one scan per memo window, not one per request"
    );
    forget_failure(&vtt_path(dir.path(), &file, 0)).await;
}

/// The other side of the TTL comparison: an expired memo must not become a
/// permanent tomb for a track whose cause has since been fixed, and the
/// success that follows must clear it outright rather than leave a corpse
/// in the map.
#[tokio::test]
async fn an_expired_memo_relaunches_and_success_clears_it() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let key = vtt_path(dir.path(), &file, 0);
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // A zero TTL is a memo that is written and already expired: the
    // deterministic version of "the window has passed", with no wall clock
    // in the test.
    let expired = ExtractionLimits {
        negative_ttl: Duration::ZERO,
        ..ExtractionLimits::default()
    };

    let runs_for_failure = Arc::clone(&runs);
    let failed = ensure_vtt_bounded(dir.path(), &file, 0, expired, move |_, _, _| async move {
        runs_for_failure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("no such stream".into())
    })
    .await;
    assert!(failed.is_err(), "the first attempt fails");
    assert!(
        negative_memos().lock().await.contains_key(&key),
        "the failure was memoized, expired or not"
    );

    let runs_for_success = Arc::clone(&runs);
    let published =
        ensure_vtt_bounded(dir.path(), &file, 0, expired, move |tmp, _, _| async move {
            runs_for_success.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::fs::write(tmp, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .expect("an expired memo cannot block a working extraction");

    assert_eq!(published, key);
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the expired memo let the second attempt run"
    );
    assert!(
        !negative_memos().lock().await.contains_key(&key),
        "a published sidecar invalidates the memo"
    );
}

#[tokio::test]
async fn an_empty_sidecar_is_rejected_and_negative_memoized() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let cached = vtt_path(dir.path(), &file, 0);
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let limits = ExtractionLimits {
        negative_ttl: Duration::from_secs(60),
        ..ExtractionLimits::default()
    };

    let runs_for_first = Arc::clone(&runs);
    let why = ensure_vtt_bounded(dir.path(), &file, 0, limits, move |tmp, _, _| async move {
        runs_for_first.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::fs::write(tmp, b"")
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .expect_err("an empty sidecar is not publishable");
    assert!(why.contains("empty"), "the error identifies empty output");
    assert!(!cached.exists(), "empty output must not be published");

    let runs_for_repeat = Arc::clone(&runs);
    let repeated = ensure_vtt_bounded(dir.path(), &file, 0, limits, move |_, _, _| async move {
        runs_for_repeat.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(repeated.is_err(), "the negative memo suppresses a rescan");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    forget_failure(&cached).await;
}

/// P2-4: the sidecar is re-read whole for every segment request, so a
/// pathological track must be refused at publish — and refused the way any
/// other failure is, leaving neither a cache entry nor a temp file.
#[tokio::test]
async fn an_oversized_sidecar_is_rejected_and_leaves_no_file() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let limits = ExtractionLimits {
        max_sidecar_bytes: 64,
        negative_ttl: Duration::ZERO,
        ..ExtractionLimits::default()
    };

    let why = ensure_vtt_bounded(dir.path(), &file, 0, limits, |tmp, _, _| async move {
        tokio::fs::write(tmp, "WEBVTT\n".to_string() + &"x".repeat(4_096))
            .await
            .map_err(|e| e.to_string())
    })
    .await
    .expect_err("an oversized sidecar is not publishable");
    assert!(why.contains("cap"), "the error names the cap, got {why}");

    assert!(
        tokio::fs::metadata(vtt_path(dir.path(), &file, 0))
            .await
            .is_err(),
        "a rejected sidecar must never appear under its cache name"
    );
    let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read cache");
    while let Some(entry) = entries.next_entry().await.expect("entry") {
        assert!(
            !entry.file_name().to_string_lossy().starts_with(".tmp-"),
            "and its temp file must be gone too"
        );
    }
}

/// The interleaving the test above only hits by luck, stated directly.
///
/// A request reads the cache, misses, and reaches the registry a moment
/// later — by which time the owner has renamed its sidecar into place and
/// retired its entry. There is no flight left to join, and the stale miss
/// says nothing is there. Owning a second extraction at that point is the
/// bug; [`enlist`] must read the cache again under the lock and hand back
/// the published answer.
#[tokio::test]
async fn a_sidecar_published_after_the_cache_read_is_joined_not_re_extracted() {
    let dir = crate::test_tempdir().expect("cache");
    let file = media_file(dir.path().join("source.mkv"));
    let cached = vtt_path(dir.path(), &file, 0);
    tokio::fs::write(&cached, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
        .await
        .expect("publish");

    // Exactly the state a request finds in that window: sidecar on disk,
    // registry empty for this key.
    assert!(
        !extractions().lock().await.contains_key(&cached),
        "no flight is registered for a retired extraction"
    );
    assert!(
        matches!(enlist(&cached, MAX_SIDECAR_BYTES).await, Flight::Published),
        "a published sidecar must never be re-owned"
    );
    assert!(
        !extractions().lock().await.contains_key(&cached),
        "and deciding must not leave a flight behind"
    );

    // The other two arms still work: an unpublished key is owned once and
    // joined thereafter.
    let missing = vtt_path(dir.path(), &file, 1);
    assert!(matches!(
        enlist(&missing, MAX_SIDECAR_BYTES).await,
        Flight::Own(_)
    ));
    assert!(matches!(
        enlist(&missing, MAX_SIDECAR_BYTES).await,
        Flight::Join(_)
    ));
    extractions().lock().await.remove(&missing);
}
