//! The copy-index ride on the real pass: the production probe, the real
//! ffmpeg child with its packet clock on descriptor 7, and the engine's own
//! observer. Each pass is one ffprobe and one ffmpeg child over a generated
//! fixture title.
//!
//! What is not real: no claim is held and nothing is handed to the Analysis
//! worker. The rows a ride returns are compared with an engine fill's, sealed
//! and encoded, in `copy_analysis::gap_fill_tests`; the worker's fold of them
//! is in `copy_analysis::quarantine_tests`.
use super::*;
use crate::copy_ride_along::{CopyRidePlan, ProbedCopyClock};

const VIDEO: transcode::CopyVideoOptions = transcode::CopyVideoOptions::new(false, false);
const BUDGET: Duration = Duration::from_secs(120);

/// The catalogue row of a generated fixture: `clean-cra` (HEVC) or `h264`.
pub(crate) fn fixture_file(kind: &str) -> MediaFile {
    let path = plurx_core::testfixtures::source(kind);
    let metadata = std::fs::metadata(&path).expect("fixture metadata");
    let hevc = kind != "h264";
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path,
        size: metadata.len() as i64,
        mtime: metadata
            .modified()
            .expect("fixture mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix mtime")
            .as_secs() as i64,
        duration_ms: Some(12_000),
        container: Some("mkv".into()),
        video_codec: Some(if hevc { "hevc" } else { "h264" }.into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some(if hevc { "Main" } else { "High" }.into()),
        width: Some(640),
        height: Some(360),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000_000),
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    }
}

/// One pass through the production entry: the held-descriptor probe, the
/// ride planned from it when `copy_ride`, and the child reading descriptor 3.
pub(crate) async fn pass(file: &MediaFile, cache: &Path, copy_ride: bool) -> IndexBuild {
    let held = std::fs::File::open(&file.path).expect("held fixture");
    first_build(
        build_attested_pass(
            file,
            &held,
            "fixture-object-v1",
            &[VIDEO],
            cache,
            BUDGET,
            None,
            None,
            None,
            copy_ride,
        )
        .await,
    )
}

/// What the production probe says about the fixture's completion and clocks.
async fn probed(file: &MediaFile) -> (VideoCompletionExpectation, ProbedCopyClock) {
    let held = std::fs::File::open(&file.path).expect("held fixture");
    let raw = crate::ffmpeg::held_source_index_probe_json(&held, BUDGET, None)
        .await
        .expect("the index metadata probe");
    let expectation = match completion_expectation_from_probe(&raw, "fixture-object-v1") {
        Ok(expectation) => expectation,
        Err(error) => panic!("fixture completion expectation: {error:?}"),
    };
    (
        expectation,
        ProbedCopyClock::from_probe(&raw).expect("the fixture's probed clock"),
    )
}

/// A pass over the fixture's path, as the lower-level tests drive one, with
/// `ride` as its copy ride.
async fn pass_with(file: &MediaFile, cache: &Path, ride: Option<CopyRidePlan>) -> IndexBuild {
    let (expectation, _) = probed(file).await;
    let mut pass = index_pass(
        file,
        VIDEO,
        Some(&file.path.to_string_lossy()),
        expectation,
        None,
    )
    .expect("a describable pass");
    pass.copy_ride = ride;
    build_with_args(
        file,
        pass,
        None,
        None,
        cache,
        BUDGET,
        Instant::now(),
        None,
        None,
    )
    .await
}

/// The acceptance the ride rests on: the node-local index is the same value
/// with and without it, on the HEVC and the H.264 fixture, and the ride's
/// rows are that index's rows, the whole title from its first tick.
#[tokio::test]
async fn the_pass_builds_the_same_index_with_and_without_the_copy_ride() {
    plurx_core::testfixtures::require_ffmpeg();
    for kind in ["clean-cra", "h264"] {
        let file = fixture_file(kind);
        let cache = crate::test_tempdir().expect("cache");
        let baseline = pass(&file, cache.path(), false).await;
        let IndexOutcome::Built(index) = &baseline.outcome else {
            panic!(
                "{kind}: the baseline pass must index: {:?}",
                baseline.outcome
            );
        };
        assert!(
            baseline.copy_ride.is_none(),
            "{kind}: no ride was asked for"
        );

        let riding = pass(&file, cache.path(), true).await;
        assert_eq!(
            riding.outcome, baseline.outcome,
            "{kind}: the ride changes nothing about the index"
        );
        let ride = riding
            .copy_ride
            .unwrap_or_else(|| panic!("{kind}: the pass kept no copy rows"));
        assert_eq!(ride.rows.rows, index.rows, "{kind}: one stream, one table");
        assert!(ride.rows.boundaries.begins_title && ride.rows.boundaries.eof);
        assert_eq!(ride.rows.canonical_title_start, 0);
        assert_eq!(
            ride.rows.row_inspections.len(),
            if kind == "h264" { 0 } else { index.rows.len() },
            "{kind}: every HEVC row is inspected, no other row is"
        );
        assert_eq!(ride.rows.inspection.is_some(), kind != "h264");
    }
}

/// The ride's clock output follows the index outputs and nothing else in
/// the argv moves, so the argv every cluster cache key hashes is unchanged:
/// no global option, and nothing inside `copy_index_pipe_args*`.
#[tokio::test]
async fn the_copy_ride_appends_one_output_and_leaves_the_hashed_argv_alone() {
    plurx_core::testfixtures::require_ffmpeg();
    let file = fixture_file("clean-cra");
    let (expectation, clock) = probed(&file).await;
    let input = file.path.to_string_lossy().into_owned();
    let pass = index_pass(&file, VIDEO, Some(&input), expectation, None).expect("a pass");
    let hashed = transcode::copy_index_pipe_args_with_input(&file, &input, VIDEO);
    assert_eq!(
        pass.args, hashed,
        "a pass that does not ride is the hashed argv"
    );

    let ride = CopyRidePlan::for_pass(&file, VIDEO, Some(clock)).expect("a ride");
    let riding = pass.riding_args(&file, &ride);
    assert_eq!(
        riding[..hashed.len()],
        hashed[..],
        "the hashed argv, whole and first"
    );
    let appended = &riding[hashed.len()..];
    assert_eq!(appended, &ride.args(&file)[..]);
    assert_eq!(appended.last().map(String::as_str), Some("pipe:7"));
    assert!(appended.windows(2).any(|pair| pair == ["-f", "framecrc"]));
    assert!(
        !riding.iter().any(|arg| arg == "-copyts"),
        "the pass takes no global option for its ride"
    );
    // `fragment_index_cluster::pipeline_digest` hashes exactly that
    // function's argv for the file's own path, which no ride can reach.
    assert_eq!(
        transcode::copy_index_pipe_args(&file, VIDEO),
        hashed,
        "the argv every cluster cache key hashes"
    );

    // A title whose clock cannot be placed does not ride at all.
    let mut transport = file.clone();
    transport.container = Some("ts".into());
    assert!(CopyRidePlan::for_pass(&transport, VIDEO, Some(clock)).is_none());
    assert!(CopyRidePlan::for_pass(&file, VIDEO, None).is_none());
}

/// A ride that fails costs the index nothing: here the probed clock's
/// witness disagrees with the child's first packet, so the observer refuses
/// at the origin, and the pass still builds the baseline's index.
#[tokio::test]
async fn a_failed_copy_ride_leaves_the_index_built() {
    plurx_core::testfixtures::require_ffmpeg();
    let file = fixture_file("clean-cra");
    let cache = crate::test_tempdir().expect("cache");
    let baseline = pass_with(&file, cache.path(), None).await;
    assert!(matches!(baseline.outcome, IndexOutcome::Built(_)));

    let (_, clock) = probed(&file).await;
    let ride = CopyRidePlan::for_pass(&file, VIDEO, Some(clock.with_start_pts_for_test(1)))
        .expect("a ride");
    let riding = pass_with(&file, cache.path(), Some(ride)).await;
    assert_eq!(
        riding.outcome, baseline.outcome,
        "IndexOutcome::Built, the same"
    );
    assert!(
        riding.copy_ride.is_none(),
        "an unplaced clock keeps no rows"
    );
}

/// A consumer of descriptor 7 that never takes anything, for longer than the
/// pass runs: the clock reader keeps draining, the backlog bound abandons
/// the ride, and the pass ends when its child does with the baseline's
/// index.
#[tokio::test]
async fn a_stalled_copy_ride_consumer_does_not_hold_the_pass() {
    plurx_core::testfixtures::require_ffmpeg();
    let file = fixture_file("clean-cra");
    let cache = crate::test_tempdir().expect("cache");
    let baseline = pass_with(&file, cache.path(), None).await;
    assert!(matches!(baseline.outcome, IndexOutcome::Built(_)));

    let (_, clock) = probed(&file).await;
    let ride = CopyRidePlan::for_pass(&file, VIDEO, Some(clock))
        .expect("a ride")
        .stalled_for_test(4 * 1024);
    let stalled = tokio::time::timeout(BUDGET, pass_with(&file, cache.path(), Some(ride)))
        .await
        .expect("a stalled consumer must not keep the pass from ending");
    assert_eq!(stalled.outcome, baseline.outcome);
    assert!(stalled.copy_ride.is_none());
}
