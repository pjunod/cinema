use super::*;
use crate::subtitle_ride_along::testing::{fixture, Sub};
use crate::subtitle_ride_along::RideAlongPlan;
use crate::subtitle_source::testing::stamp_of;
use crate::subtitle_source::Verdict;

const VIDEO: transcode::CopyVideoOptions = transcode::CopyVideoOptions::new(false, false);

fn expectation() -> VideoCompletionExpectation {
    VideoCompletionExpectation {
        stream_index: 0,
        duration_num: 16,
        duration_den: 1,
        provenance: CompletionProvenance::StreamSeconds,
        source_object_version: "fixture".to_owned(),
    }
}

fn pass_over(source: &Path, file: &MediaFile, plan: Option<RideAlongPlan>) -> IndexPass {
    index_pass(
        file,
        VIDEO,
        Some(&source.to_string_lossy()),
        expectation(),
        plan,
    )
    .expect("a describable pass")
}

/// (f) A regression for the tee's own truncation: a file already sitting
/// where a slave will write — a stale `.sup` and a stale `.crc` whose
/// packet count would pass for a verdict — is truncated by the slave that
/// opens it, so the track is judged on what this pass wrote, and the index
/// is the baseline's. The tee opens its slaves itself (`avio_open`), with
/// or without `-y`; the index output is `pipe:1`, so the leftover-file
/// refusal ffmpeg applies to a named output (§6.2 fact 4) cannot reach it.
/// This test therefore does not prove `-y` necessary — it pins the
/// truncation the verdict relies on. The fresh stage per attempt is the
/// other half: every attempt gets a directory of its own.
#[tokio::test]
async fn a_leftover_in_the_stage_cannot_empty_the_index() {
    let fixture = fixture(72_001, &[Sub::Pgs]);
    let cache = crate::test_tempdir().expect("cache");
    let started = Instant::now();
    let baseline = build_with_args(
        &fixture.file,
        pass_over(&fixture.source, &fixture.file, None),
        None,
        None,
        cache.path(),
        Duration::from_secs(60),
        started,
        None,
        None,
    )
    .await;
    assert!(matches!(baseline.outcome, IndexOutcome::Built(_)));

    let plan = RideAlongPlan::standalone(
        cache.path(),
        fixture.file.id,
        stamp_of(&fixture.source),
        vec![0],
    )
    .expect("plan");
    let other = RideAlongPlan::standalone(
        cache.path(),
        fixture.file.id,
        stamp_of(&fixture.source),
        vec![0],
    )
    .expect("another plan");
    assert_ne!(
        plan.stage(),
        other.stage(),
        "every attempt has its own stage"
    );
    std::fs::write(plan.stage().join("s0.sup"), b"left over by someone else").expect("leftover");
    std::fs::write(
        plan.stage().join("s0.crc"),
        b"#left over\n0, 0, 0, 0, 5, 0x0\n",
    )
    .expect("leftover");
    let riding = build_with_args(
        &fixture.file,
        pass_over(&fixture.source, &fixture.file, Some(plan)),
        None,
        None,
        cache.path(),
        Duration::from_secs(60),
        Instant::now(),
        None,
        None,
    )
    .await;
    assert_eq!(
        riding.outcome, baseline.outcome,
        "the same index, not an empty one"
    );
    let harvest = riding.ride_along.expect("a harvest").judge().await;
    assert_eq!(
        harvest.outcomes()[0].verdict,
        Verdict::Kept,
        "{:?}",
        harvest.outcomes()
    );
}

/// Review finding 1: a riding pass whose own argv breaks the index — here
/// a hard map to a subtitle ordinal the file does not have, as an
/// `ffprobe` of another build could produce — is remembered, and the
/// file's next attempt is a plain index pass that builds.
#[tokio::test]
async fn after_a_riding_pass_fails_the_next_attempt_does_not_ride() {
    let fixture = fixture(72_003, &[Sub::Pgs]);
    let cache = crate::test_tempdir().expect("cache");
    let root = crate::subtitle_source::store_root(cache.path());
    let plan = RideAlongPlan::standalone(
        &root,
        fixture.file.id,
        stamp_of(&fixture.source),
        vec![0, 5],
    )
    .expect("plan");
    let broken = build_with_args(
        &fixture.file,
        pass_over(&fixture.source, &fixture.file, Some(plan)),
        None,
        None,
        cache.path(),
        Duration::from_secs(60),
        Instant::now(),
        None,
        None,
    )
    .await;
    assert!(
        matches!(broken.outcome, IndexOutcome::Failed(_)),
        "a map to a missing ordinal fails the pass: {:?}",
        broken.outcome
    );
    assert!(broken.ride_along.is_none());

    let gate = crate::subtitle_ride_along::RideAlongGate::for_test(root.clone());
    let source = std::fs::File::open(&fixture.source).expect("held source");
    assert!(
        crate::subtitle_ride_along::plan(&gate, fixture.file.id, &source, &[0])
            .await
            .is_none(),
        "the file does not ride again on the same source"
    );
    let retry = build_from_attested_file_with_progress(
        &fixture.file,
        &source,
        "fixture-object-v1",
        VIDEO,
        cache.path(),
        Duration::from_secs(60),
        |_: &crate::fragindex::PassProgress| {},
        Some(&gate),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(retry.outcome, IndexOutcome::Built(_)),
        "the plain retry builds the index: {:?}",
        retry.outcome
    );
    assert!(retry.ride_along.is_none(), "and did not ride");
}

/// PR 3: a riding pass reports, with its progress, how many PGS tracks
/// it keeps and what its stage holds — re-measured at most once a second —
/// so the analysis row can say so while the pass runs.
///
/// The fixture's track is a few kilobytes, which the `sup` slave holds in
/// its I/O buffer until the trailer; a film's track is megabytes and
/// reaches the disk as it goes. So the test puts known bytes into the
/// stage mid-pass and requires a later report to count them: the meter
/// re-measures the stage during the pass rather than once at its start.
#[tokio::test]
async fn a_riding_pass_reports_its_tracks_and_bytes_with_its_progress() {
    const MARKER: usize = 1234;
    let fixture = fixture(72_004, &[Sub::Pgs]);
    let cache = crate::test_tempdir().expect("cache");
    let plan = RideAlongPlan::standalone(
        cache.path(),
        fixture.file.id,
        stamp_of(&fixture.source),
        vec![0],
    )
    .expect("plan");
    let stage = plan.stage().to_owned();
    let mut pass = pass_over(&fixture.source, &fixture.file, Some(plan));
    // Read at the media's own pace, so the pass is still running while
    // the test watches its reports.
    let input = pass
        .args
        .iter()
        .position(|arg| arg == "-i")
        .expect("an input");
    pass.args.insert(input, "-re".to_owned());
    let seen = Arc::new(std::sync::Mutex::new(Vec::<PassProgress>::new()));
    let recorder = Arc::clone(&seen);
    let progress: SharedIndexProgress = Arc::new(move |report: &PassProgress| {
        recorder.lock().expect("seen").push(*report);
    });
    let mut build = Box::pin(build_with_args(
        &fixture.file,
        pass,
        None,
        None,
        cache.path(),
        Duration::from_secs(60),
        Instant::now(),
        Some(progress),
        None,
    ));
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut marked = false;
    loop {
        let reports = seen.lock().expect("seen").clone();
        if let Some(first) = reports.first() {
            assert_eq!(first.pgs_tracks, 1, "every report names the track");
            assert!(reports.iter().all(|report| report.pgs_tracks == 1));
            if !marked {
                std::fs::write(stage.join("meter-marker"), [0_u8; MARKER])
                    .expect("a marker in the stage");
                marked = true;
            } else if reports
                .iter()
                .any(|report| report.pgs_bytes_written >= MARKER as u64)
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no report counted the stage's bytes: {reports:?}"
        );
        if tokio::time::timeout(Duration::from_millis(50), &mut build)
            .await
            .is_ok()
        {
            panic!("the paced pass finished before a report counted the stage: {reports:?}");
        }
    }
    drop(build);
}

/// A standalone pass must stop a running source read when a viewer
/// arrives, remove its partial stage, and permit a later retry.
#[tokio::test]
async fn standalone_index_yields_to_foreground_and_removes_partial_stage() {
    let fixture = fixture(72_002, &[Sub::Pgs]);
    let cache = crate::test_tempdir().expect("cache");
    let store = Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let transcode = crate::transcode::TranscodeManager::new(
        store,
        cache.path().join("transcode"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    );
    let plan = RideAlongPlan::standalone(
        cache.path(),
        fixture.file.id,
        stamp_of(&fixture.source),
        vec![0],
    )
    .expect("plan");
    let stage = plan.stage().to_owned();
    let mut pass = pass_over(&fixture.source, &fixture.file, Some(plan));
    // Read at the media's own pace, so sixteen seconds of it are still
    // being read when the future is dropped.
    let input = pass
        .args
        .iter()
        .position(|arg| arg == "-i")
        .expect("an input");
    pass.args.insert(input, "-re".to_owned());
    let admission = transcode
        .admit_fragment()
        .await
        .expect("the pass's heavy-lane admission");
    let mut build = Box::pin(crate::state::local_index_until_foreground(
        &transcode,
        &admission,
        build_with_args(
            &fixture.file,
            pass,
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            None,
            None,
        ),
    ));
    // Mid-pass: the tee has opened its slaves in the stage.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !stage.join("s0.crc").exists() {
        assert!(
            Instant::now() < deadline,
            "the pass never opened its slaves"
        );
        if tokio::time::timeout(Duration::from_millis(20), &mut build)
            .await
            .is_ok()
        {
            panic!("the paced pass finished before it could be dropped");
        }
    }
    assert!(stage.is_dir());
    let waiting = transcode.test_mark_live_waiting();
    let outcome = tokio::time::timeout(Duration::from_secs(2), build)
        .await
        .expect("foreground playback preempts the running source read");
    assert!(outcome.is_none(), "preemption is not an index refusal");
    assert!(!stage.exists(), "the interrupted stage must be removed");
    drop(waiting);
    let retry = crate::state::local_index_until_foreground(
        &transcode,
        &admission,
        build_with_args(
            &fixture.file,
            pass_over(&fixture.source, &fixture.file, None),
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            None,
            None,
        ),
    )
    .await
    .expect("the file may retry after the viewer leaves");
    assert!(matches!(retry.outcome, IndexOutcome::Built(_)));
}
