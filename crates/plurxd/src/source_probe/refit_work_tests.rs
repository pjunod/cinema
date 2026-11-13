//! The F+ job against a real store and a fake helper child: demand, the
//! helper contract through the bounded child owner, classification,
//! publication, refusal, the spool and resume, preemption and cancellation
//! through `execute`, and the durable terminal outcome.
use super::super::dv_refit::{classify, settle_child, spawn, HelperRun, RunError, Spool};
use super::*;
use plurx_core::{
    domain::{
        DolbyVisionFacts, ItemKind, LibraryKind, MediaFile, NewItem, NewLibrary, ProbeResult,
    },
    scan::dv_title::{DvTitleStats, TitleConstant},
    store::{
        background_jobs::{BackgroundJob, CancelJob, JobState},
        dv_refit_jobs::RefitVerdict,
        dv_title_jobs::{PublishTitleJob, TitleOutput},
        SqliteStore, Store,
    },
    transcode::{
        dv_fel_qualification::FelVerdict,
        dv_refit::{summarise, RefitScene, RefitStats},
        dv_refit_title::DigestScene,
    },
};
use std::path::{Path, PathBuf};

fn helper() -> String {
    "c".repeat(64)
}

/// Samples of a composition that is the base layer moved by `offset` codes,
/// ±`noise` alternating, with chroma passed through.
fn stats(offset: i32, noise: i32, phase: u32) -> RefitStats {
    let mut stats = RefitStats::new();
    for i in 0..4000u32 {
        let y = 64 + (i * 7 + phase) % 896;
        let u = 256 + (i * 37 + phase * 3) % 512;
        let v = 256 + (i * 91 + phase * 5) % 512;
        let sign = if i % 2 == 0 { 1 } else { -1 };
        let composed = (4 * y as i32 + offset + sign * noise).clamp(0, 4095) as u16;
        stats.add(
            [y as u16, u as u16, v as u16],
            [composed, (4 * u) as u16, (4 * v) as u16],
        );
    }
    stats
}

/// Scene lines in the helper's contract, each pooled from RPU scenes of
/// `rpu_scene` frames (the last takes the remainder), starting at `frame`.
fn scene_lines(scenes: &[(u64, RefitStats, RefitStats)], rpu_scene: u64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut frame = 0u64;
    let pts = |frame: u64| 125 + frame as i64 * 42;
    for (frames, fit, held_out) in scenes {
        let mut lengths = vec![rpu_scene; (*frames / rpu_scene).max(1) as usize];
        *lengths.last_mut().expect("one") += frames - lengths.iter().sum::<u64>();
        lines.push(
            serde_json::json!({"kind":"refit_scene","first_frame":frame,"frames":frames,
                "first_pts":pts(frame),"end_pts":pts(frame + frames) - 1,"time_base":"1/1000",
                "rpu_scene_frames":lengths,"fit":fit,"held_out":held_out})
            .to_string(),
        );
        frame += frames;
    }
    lines
}

/// `lines` and the summary an uninterrupted (or resumed) run writes after them.
fn with_summary(lines: &[String]) -> String {
    let scenes: Vec<RefitScene> = lines
        .iter()
        .map(|l| RefitScene::from_line(l).expect("scene"))
        .collect();
    let summary = serde_json::to_string(&summarise(&scenes).expect("summary")).expect("json");
    let mut text = lines.join("\n");
    text.push('\n');
    text.push_str(&summary);
    text.push('\n');
    text
}

fn recorded(scenes: &[(u64, RefitStats, RefitStats)], rpu_scene: u64) -> String {
    with_summary(&scene_lines(scenes, rpu_scene))
}

/// The real helper's output for the Michael5400 window.
const MICHAEL5400: &str =
    include_str!("../../../plurx-core/tests/fixtures/dv-refit/helper-run-michael5400.jsonl");

fn mixed_scenes() -> Vec<(u64, RefitStats, RefitStats)> {
    vec![
        (48, stats(12, 0, 0), stats(12, 0, 1)),
        (96, stats(-10, 0, 2), stats(-10, 0, 3)),
        (72, stats(30, 0, 4), stats(0, 0, 5)),
    ]
}

/// Two scenes the fit helps; one whose held-out samples refuse its fit.
fn mixed() -> String {
    recorded(&mixed_scenes(), 48)
}

fn executable(path: &Path, script: &str) {
    std::fs::write(path, script).expect("fake helper");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
}

/// A fake helper: checks the refit arguments, prints `stdout` (from a
/// file), then runs `tail`.
fn fake(dir: &Path, name: &str, stdout: &str, tail: &str) -> PathBuf {
    let data = dir.join(format!("{name}.jsonl"));
    std::fs::write(&data, stdout).expect("recorded output");
    let script = dir.join(format!("{name}.sh"));
    executable(
        &script,
        &format!(
            "#!/bin/sh\n[ \"$1\" = --refit-stats ] && [ \"$4\" = --threads ] && [ \"$5\" = 2 ] || exit 64\n[ \"$BUNDLE\" = kept ] || exit 65\ncat '{}'\n{tail}\n",
            data.display()
        ),
    );
    script
}

fn spool_in(dir: &Path, name: &str) -> Spool {
    Spool::new(dir.join("spool"), 1, 2, 3, 0, name)
}

/// One run of a fake helper through the bounded child owner, its stdout in
/// a fresh spool, as `dv_refit::run` settles it.
async fn run_fake(dir: &Path, script: &Path) -> Result<HelperRun, RunError> {
    let spool = spool_in(dir, &script.display().to_string());
    // A just-written script can briefly be busy for exec in a threaded test.
    for _ in 0..20 {
        let stdout = spool.open_run().await.expect("run file");
        let output = spawn(
            script,
            &[("BUNDLE".into(), "kept".into())],
            "/dev/null",
            0,
            None,
            stdout,
            |_| {},
        )
        .await;
        if !matches!(&output, Err(e) if e.raw_os_error() == Some(libc::ETXTBSY)) {
            return settle_child(&spool, Vec::new(), None, classify(&output)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("fake helper stayed busy");
}

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    store: Arc<dyn Store>,
    library: i64,
}

async fn fixture() -> Fixture {
    let dir = crate::test_tempdir().expect("scratch");
    let root = dir.path().to_path_buf();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let library = store
        .create_library(&NewLibrary {
            name: "refit".into(),
            kind: LibraryKind::Movies,
            paths: vec![root.clone()],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    Fixture {
        dir,
        root,
        store,
        library,
    }
}

fn app(fx: &Fixture) -> AppState {
    let dir = fx.dir.path();
    AppState::new(
        "test".into(),
        Arc::clone(&fx.store),
        crate::state::Dirs {
            artwork: dir.join("artwork"),
            transcode: dir.join("transcode"),
            cache: dir.join("cache"),
            subs: dir.join("subs"),
            runtime_cache: dir.join("runtime"),
            renditions: dir.join("renditions"),
        },
        "refit-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(16)),
    )
}

/// A Profile `profile` title with an enhancement layer whose complete title
/// facts are published; `fel` also stores the FEL route's verdict for it.
async fn title(fx: &Fixture, name: &str, profile: i64, fel: bool) -> MediaFile {
    let store = &fx.store;
    let item = store
        .insert_item(&NewItem {
            library_id: fx.library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: name.into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = fx.root.join(format!("{name}.mkv"));
    tokio::fs::write(&path, name.as_bytes())
        .await
        .expect("source");
    let (size, mtime) = snapshot(&path).await.expect("snapshot");
    let verdict = serde_json::to_value(FelVerdict::unmeasurable("d".repeat(64), size, mtime, 1))
        .expect("verdict");
    // Only a Profile 7 row takes a FEL verdict through the store; another
    // profile's probe document carries one as if a probe had written it.
    let mut raw = serde_json::json!({"streams":[{"index":0,"codec_type":"video"}]});
    if fel && profile != 7 {
        raw[plurx_core::transcode::dv_fel_qualification::PROBE_KEY] = verdict.clone();
    }
    let id = store
        .upsert_file(
            item,
            path.to_str().expect("path"),
            size,
            mtime,
            &ProbeResult {
                video_codec: Some("hevc".into()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(profile),
                    rpu_present: Some(true),
                    el_present: Some(true),
                    ..Default::default()
                },
                raw_json: Some(raw.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("DV file");
    let scanner = "a".repeat(64);
    let demand = store
        .dv_title_demands(&scanner, 0)
        .await
        .expect("title demand")
        .into_iter()
        .find(|d| d.file_id == id)
        .expect("title demand for the file");
    let revision = store
        .bind_dv_title_demand(demand.probe_revision, &scanner)
        .await
        .expect("bind")
        .expect("bound");
    let now = now_ms();
    let EnqueueOutcome::Accepted { job_id, .. } = store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::DvTitleScan {
                file_id: id,
                source_size: size,
                source_mtime: mtime,
                probe_revision: revision,
                scanner_digest: scanner.clone(),
            },
            dedupe_key: format!("dv-title:{name}"),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "dv-title".into(),
                request_id: format!("title-{name}"),
                request_digest: "e".repeat(64),
                consumer_kind: "dv_title_scan".into(),
                consumer_ref: id.to_string(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("title job")
    else {
        panic!("title job accepted");
    };
    let job = claim(store, &job_id, JobKind::DvTitleScan).await;
    let stats = DvTitleStats {
        schema: 1,
        complete: true,
        decoded_frames: 216,
        valid_l1_frames: 216,
        missing_l1_frames: 0,
        invalid_l1_frames: 0,
        max_l1_pq: Some(3900),
        mastering_luminance: TitleConstant::Unknown,
        mastering_primaries: TitleConstant::Unknown,
        el_kind: if fel && profile == 7 {
            TitleConstant::Consistent(plurx_core::scan::dv_title::TitleElKind::Fel)
        } else {
            TitleConstant::Consistent(plurx_core::scan::dv_title::TitleElKind::Mel)
        },
    };
    store
        .publish_dv_title_job(PublishTitleJob {
            token: job.token.expect("token"),
            output: TitleOutput {
                file_id: id,
                probe_revision: revision,
                source_size: size,
                source_mtime: mtime,
                scanner_digest: scanner,
                stats,
            },
            now_ms: now_ms(),
        })
        .await
        .expect("title facts");
    if fel && profile == 7 {
        assert!(store
            .put_dv_fel_verdict(id, size, mtime, &verdict)
            .await
            .expect("FEL verdict"));
    }
    store.get_file(id).await.expect("file").expect("file")
}

async fn claim(store: &Arc<dyn Store>, id: &str, kind: JobKind) -> BackgroundJob {
    let candidate = store.background_job(id).await.expect("job").expect("job");
    let now = now_ms();
    crate::background_jobs::claim_with_resolution(
        store.as_ref(),
        &candidate,
        ClaimJob {
            job_id: id.into(),
            expected_revision: candidate.revision,
            node_id: "refit-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        },
    )
    .await
    .expect("claim")
    .expect("claimed")
    .0
}

async fn demand_for(store: &Arc<dyn Store>, file: i64, helper: &str) -> Option<RefitDemand> {
    store
        .dv_refit_demands(helper, FIT_VERSION, true, 0, 16)
        .await
        .expect("F+ demand")
        .into_iter()
        .find(|d| d.file_id == file)
}

async fn demand(store: &Arc<dyn Store>, file: i64) -> Option<RefitDemand> {
    demand_for(store, file, &helper()).await
}

fn source(demand: &RefitDemand, helper: &str) -> RefitSource {
    source_of(demand, helper).expect("a selected video stream")
}

/// Enqueue (or reuse the queued job) and claim the file's fit, as
/// `RefitTask::start` does.
async fn attempt(
    store: &Arc<dyn Store>,
    source: &RefitSource,
) -> (BackgroundJob, ActiveBackgroundJob) {
    let job_id = match store
        .enqueue_job(enqueue_request(source, now_ms()))
        .await
        .expect("enqueue")
    {
        EnqueueOutcome::Accepted { job_id, .. }
        | EnqueueOutcome::Existing {
            job_id,
            cancelled: false,
            ..
        } => job_id,
        other => panic!("fit accepted: {other:?}"),
    };
    let candidate = store
        .background_job(&job_id)
        .await
        .expect("job")
        .expect("job");
    let now = now_ms();
    let (job, deadline) = crate::background_jobs::claim_with_resolution(
        store.as_ref(),
        &candidate,
        ClaimJob {
            job_id,
            expected_revision: candidate.revision,
            node_id: "refit-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::DvRefit,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        },
    )
    .await
    .expect("claim")
    .expect("claimed");
    let active = ActiveBackgroundJob::start(
        Arc::clone(store),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        job.token.clone().expect("token"),
        deadline,
        JobKind::DvRefit,
    )
    .await
    .expect("active");
    (job, active)
}

/// One whole attempt with a fake helper's answer.
async fn fit_with(
    store: &Arc<dyn Store>,
    source: &RefitSource,
    run: Result<HelperRun, RunError>,
) -> (BackgroundJob, Option<JobSettlement>) {
    let (job, active) = attempt(store, source).await;
    let fence = active.fence();
    let settlement = settle_run(&fence, &job, source, run).await;
    if let Some(settlement) = settlement.clone() {
        fence.settle(settlement).await.expect("settle");
    }
    active.finish().await;
    (job, settlement)
}

async fn state(store: &Arc<dyn Store>, job: &BackgroundJob) -> JobState {
    store
        .background_job(&job.id)
        .await
        .expect("job")
        .expect("job")
        .state
}

async fn table(
    store: &Arc<dyn Store>,
    file: i64,
) -> plurx_core::store::dv_refit_jobs::RefitCurveTable {
    store
        .dv_refit_curves(file)
        .await
        .expect("curves")
        .expect("a current curve table")
}

fn digest_of(table: &plurx_core::store::dv_refit_jobs::RefitCurveTable) -> String {
    let served: Vec<_> = table
        .scenes
        .iter()
        .map(|s| s.curves.as_ref().map(|c| c.quantised()))
        .collect();
    dv_refit_title::curve_digest(table.scenes.iter().zip(&served).map(|(s, q)| DigestScene {
        first_frame: s.first_frame as u64,
        frames: s.frames as u64,
        first_pts: s.first_pts,
        end_pts: s.end_pts,
        time_base: &s.time_base,
        coefficients: q.as_ref(),
    }))
}

#[tokio::test]
async fn demand_selects_current_fel_titles_only() {
    let fx = fixture().await;
    let fel = title(&fx, "fel", 7, true).await;
    let mel = title(&fx, "mel", 7, false).await;
    let p8 = title(&fx, "p8", 8, true).await;
    let found = demand(&fx.store, fel.id)
        .await
        .expect("a P7 FEL title is demanded");
    assert_eq!(
        (found.source_size, found.source_mtime),
        (fel.size, fel.mtime)
    );
    assert!(found.queued.is_none() && found.stored_video_index.is_none());
    assert!(
        demand(&fx.store, mel.id).await.is_none(),
        "no FEL facts: MEL or unknown"
    );
    assert!(demand(&fx.store, p8.id).await.is_none(), "not Profile 7");
    let src = source(&found, &helper());
    assert_eq!(src.video_index, 0);
    let run = run_fake(&fx.root, &fake(&fx.root, "demand", &mixed(), "")).await;
    let (job, settlement) = fit_with(&fx.store, &src, run).await;
    assert!(settlement.is_none(), "{settlement:?}");
    assert_eq!(state(&fx.store, &job).await, JobState::Succeeded);
    assert!(
        demand(&fx.store, fel.id).await.is_none(),
        "current after publication"
    );
    assert!(
        demand_for(&fx.store, fel.id, &"f".repeat(64))
            .await
            .is_some(),
        "another helper identity makes the fit stale"
    );
    // A fresh probe moves the title generation and drops the published
    // title facts: no demand until the title scan publishes them again.
    fx.store
        .upsert_file(
            fel.item_id,
            fel.path.to_str().expect("path"),
            fel.size,
            fel.mtime,
            &ProbeResult {
                video_codec: Some("hevc".into()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(7),
                    rpu_present: Some(true),
                    el_present: Some(true),
                    ..Default::default()
                },
                raw_json: Some("{}".into()),
                ..Default::default()
            },
        )
        .await
        .expect("reprobe");
    assert!(demand(&fx.store, fel.id).await.is_none());
}

/// Decision C: the job runs only while one platform's switch is on.
#[tokio::test]
async fn demand_runs_only_while_a_platform_switch_is_on() {
    let fx = fixture().await;
    let state = app(&fx);
    assert!(!enabled(&state).await.expect("settings"), "default off");
    for key in plurx_core::store::keys::DV_REFIT_PLATFORMS {
        fx.store.put_setting(key, "1").await.expect("on");
        assert!(enabled(&state).await.expect("settings"), "{key}");
        fx.store.put_setting(key, "0").await.expect("off");
    }
    assert!(!enabled(&state).await.expect("settings"));
    let mut after = 0;
    assert!(RefitTask::start(&state, "boot", &mut after)
        .await
        .expect("start")
        .is_none());
}

#[tokio::test]
async fn a_fake_helper_run_publishes_solved_and_refused_scenes() {
    let fx = fixture().await;
    let file = title(&fx, "mixed", 7, true).await;
    let src = source(
        &demand(&fx.store, file.id).await.expect("demand"),
        &helper(),
    );
    let run = run_fake(&fx.root, &fake(&fx.root, "mixed", &mixed(), "")).await;
    assert!(matches!(run, Ok(HelperRun::Stats(_))), "{run:?}");
    let (_, settlement) = fit_with(&fx.store, &src, run).await;
    assert!(settlement.is_none(), "{settlement:?}");
    let table = table(&fx.store, file.id).await;
    assert_eq!(table.title.verdict, RefitVerdict::Fitted);
    assert_eq!(table.title.fit_version, FIT_VERSION);
    assert_eq!(table.title.helper_identity, helper());
    assert!(table.title.gain.is_some_and(|g| g > 0.05));
    let served: Vec<bool> = table.scenes.iter().map(|s| s.curves.is_some()).collect();
    assert_eq!(served, [true, true, false]);
    // Scenes are keyed by presentation time, [first_ptsᵢ, first_ptsᵢ₊₁).
    let keys: Vec<(i64, i64, i64)> = table
        .scenes
        .iter()
        .map(|s| (s.first_frame, s.first_pts, s.end_pts))
        .collect();
    assert_eq!(keys, [(0, 125, 2140), (48, 2141, 6172), (144, 6173, 9196)]);
    // The stored integers and keys reproduce the stored digest.
    assert_eq!(digest_of(&table), table.title.curve_digest);
    // And the same table by its digest.
    assert_eq!(
        fx.store
            .dv_refit_curves_by_digest(file.id, &table.title.curve_digest)
            .await
            .expect("by digest"),
        Some(table)
    );
}

#[tokio::test]
async fn degenerate_and_low_gain_titles_store_identity_curves() {
    let fx = fixture().await;
    for (name, output, reason) in [
        (
            "degenerate",
            recorded(&[(48, stats(12, 0, 0), stats(12, 0, 1))], 12),
            "scene_refresh_flag",
        ),
        (
            "low-gain",
            recorded(
                &[
                    (48, stats(2, 20, 0), stats(2, 20, 1)),
                    (48, stats(2, 20, 2), stats(2, 20, 3)),
                ],
                48,
            ),
            "floor",
        ),
    ] {
        let file = title(&fx, name, 7, true).await;
        let src = source(
            &demand(&fx.store, file.id).await.expect("demand"),
            &helper(),
        );
        let run = run_fake(&fx.root, &fake(&fx.root, name, &output, "")).await;
        let (_, settlement) = fit_with(&fx.store, &src, run).await;
        assert!(settlement.is_none(), "{name}: {settlement:?}");
        let table = table(&fx.store, file.id).await;
        assert_eq!(table.title.verdict, RefitVerdict::Identity, "{name}");
        assert!(
            table
                .title
                .reason
                .as_deref()
                .is_some_and(|r| r.contains(reason)),
            "{name}"
        );
        assert!(
            !table.scenes.is_empty() && table.scenes.iter().all(|s| s.curves.is_none()),
            "{name}"
        );
    }
}

#[tokio::test]
async fn the_digest_is_stable_across_two_identical_runs() {
    let fx = fixture().await;
    let mut digests = Vec::new();
    for name in ["first", "second"] {
        let file = title(&fx, name, 7, true).await;
        let src = source(
            &demand(&fx.store, file.id).await.expect("demand"),
            &helper(),
        );
        let run = run_fake(&fx.root, &fake(&fx.root, name, MICHAEL5400, "")).await;
        fit_with(&fx.store, &src, run).await;
        digests.push(table(&fx.store, file.id).await.title.curve_digest);
    }
    assert_eq!(digests[0], digests[1]);
    let recorded = dv_refit_title::fit(dv_refit_title::parse(MICHAEL5400.as_bytes()).expect("run"));
    assert_eq!(digests[0], recorded.curve_digest);
}

/// Decision D through the real child owner: exit 3 is a content refusal,
/// stored as refused; exit 1 and a signal are retried, whatever stderr says.
#[tokio::test]
async fn a_content_refusal_is_recorded_and_other_failures_retry() {
    let fx = fixture().await;
    let file = title(&fx, "refused", 7, true).await;
    let src = source(
        &demand(&fx.store, file.id).await.expect("demand"),
        &helper(),
    );
    let reason = "full-resolution enhancement layer is not supported";
    let run = run_fake(
        &fx.root,
        &fake(
            &fx.root,
            "refused",
            "",
            &format!("echo 'a decoder note' >&2; echo 'Segment refused: {reason}' >&2; exit 3"),
        ),
    )
    .await;
    assert!(
        matches!(&run, Ok(HelperRun::Refused(r)) if r == reason),
        "{run:?}"
    );
    let (_, settlement) = fit_with(&fx.store, &src, run).await;
    assert!(settlement.is_none());
    let table = table(&fx.store, file.id).await;
    assert_eq!(table.title.verdict, RefitVerdict::Refused);
    assert_eq!(table.title.reason.as_deref(), Some(reason));
    assert!(table.scenes.is_empty());
    assert!(demand(&fx.store, file.id).await.is_none());
    for (name, tail) in [
        (
            "io",
            "echo 'Segment refused: cannot read the source' >&2; exit 1",
        ),
        ("silent", "exit 1"),
        ("killed", "kill -9 $$"),
    ] {
        let run = run_fake(&fx.root, &fake(&fx.root, name, "", tail)).await;
        assert!(
            matches!(run, Err(RunError::Transient(_))),
            "{name}: {run:?}"
        );
        let file = title(&fx, name, 7, true).await;
        let src = source(
            &demand(&fx.store, file.id).await.expect("demand"),
            &helper(),
        );
        let (job, settlement) = fit_with(&fx.store, &src, run).await;
        assert!(
            matches!(settlement, Some(JobSettlement::Retry { .. })),
            "{name}: {settlement:?}"
        );
        assert_eq!(state(&fx.store, &job).await, JobState::Queued, "{name}");
    }
}

/// Decisions E and I: malformed output fails, deterministically; the
/// identity stays terminal; the jobs page offers Retry, which lifts it.
#[tokio::test]
async fn malformed_output_fails_terminally_until_retried() {
    let fx = fixture().await;
    let good = mixed();
    for (name, output) in [
        ("truncated", good[..good.len() / 2].to_owned()),
        (
            "no-summary",
            good.lines().take(3).collect::<Vec<_>>().join("\n"),
        ),
        ("garbage", "not json\n".to_owned()),
    ] {
        let file = title(&fx, name, 7, true).await;
        let src = source(
            &demand(&fx.store, file.id).await.expect("demand"),
            &helper(),
        );
        let run = run_fake(&fx.root, &fake(&fx.root, name, &output, "")).await;
        assert!(
            matches!(run, Err(RunError::Malformed(_))),
            "{name}: {run:?}"
        );
        let (job, settlement) = fit_with(&fx.store, &src, run).await;
        assert!(
            matches!(settlement, Some(JobSettlement::Fail { .. })),
            "{name}"
        );
        let failed = fx
            .store
            .background_job(&job.id)
            .await
            .expect("job")
            .expect("job");
        assert_eq!(failed.state, JobState::Failed, "{name}");
        assert_eq!(
            crate::http::background_jobs::summary(&failed, now_ms())["retry_supported"],
            serde_json::Value::Bool(true),
            "{name}: Retry is offered"
        );
        assert!(
            fx.store
                .dv_refit_curves(file.id)
                .await
                .expect("curves")
                .is_none(),
            "{name}"
        );
        assert!(
            demand(&fx.store, file.id).await.is_none(),
            "{name}: no automatic re-decode"
        );
        if name == "garbage" {
            fx.store
                .retry_background_job(&job.id, &uuid::Uuid::new_v4().to_string(), 1, now_ms())
                .await
                .expect("retry")
                .expect("retried");
            assert_eq!(
                demand(&fx.store, file.id)
                    .await
                    .and_then(|d| d.queued)
                    .map(|q| q.identity),
                Some(identity(&src)),
                "Retry clears the outcome and queues the same identity"
            );
        }
    }
}

/// Decision G: a newer title-scan generation that selects the same stream
/// rebinds the stored table instead of fitting again.
#[tokio::test]
async fn a_newer_generation_of_the_same_stream_rebinds() {
    let fx = fixture().await;
    let file = title(&fx, "rebind", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let src = source(&found, &helper());
    let run = run_fake(&fx.root, &fake(&fx.root, "rebind", &mixed(), "")).await;
    fit_with(&fx.store, &src, run).await;
    // As if the title had been read under an older generation.
    let mut older = rebind_of(
        &RefitDemand {
            stored_video_index: Some(0),
            ..found.clone()
        },
        &src,
    )
    .expect("a known stream rebinds");
    older.probe_revision += 7;
    fx.store.rebind_dv_refit(older).await.expect("rebind");
    let stale = demand(&fx.store, file.id)
        .await
        .expect("generation differs");
    assert_eq!(stale.stored_video_index, Some(0));
    let rebind = rebind_of(&stale, &source(&stale, &helper())).expect("rebind, no fit");
    fx.store.rebind_dv_refit(rebind).await.expect("rebind");
    assert!(demand(&fx.store, file.id).await.is_none());
    // Another stream is another identity: no rebind.
    let other = RefitSource {
        video_index: 1,
        ..source(&stale, &helper())
    };
    assert!(rebind_of(&stale, &other).is_none());
}

/// Yields are bounded: one that kept no new scene adds to a stall count,
/// and past the bound the attempt retries (counting against its limit).
#[tokio::test]
async fn stalled_yields_are_bounded() {
    let fx = fixture().await;
    let file = title(&fx, "stalled", 7, true).await;
    let src = source(
        &demand(&fx.store, file.id).await.expect("demand"),
        &helper(),
    );
    let (mut job, active) = attempt(&fx.store, &src).await;
    let stalled = |settlement: JobSettlement| match settlement {
        JobSettlement::Yield { checkpoint, .. } => checkpoint.and_then(|c| {
            c.get("refit_stalled_yields")
                .and_then(serde_json::Value::as_i64)
        }),
        _ => None,
    };
    assert_eq!(stalled(interrupted(&job, 0, 0)), Some(1));
    job.checkpoint = Some(serde_json::json!({ "refit_stalled_yields": 3 }));
    assert_eq!(
        stalled(interrupted(&job, 2, 5)),
        Some(0),
        "progress resets it"
    );
    assert_eq!(stalled(interrupted(&job, 2, 2)), Some(4));
    job.checkpoint = Some(serde_json::json!({ "refit_stalled_yields": MAX_STALLED_YIELDS }));
    assert!(matches!(
        interrupted(&job, 2, 2),
        JobSettlement::Retry { error_code, .. } if error_code == "refit_preempted"
    ));
    active.finish().await;
}

/// A complete spool answers without running the helper: a fit-method
/// change re-solves the title from it.
#[tokio::test]
async fn a_complete_spool_re_solves_without_decoding() {
    let fx = fixture().await;
    let file = title(&fx, "spooled", 7, true).await;
    let spool = Spool::new(
        Spool::root(&fx.root.join("runtime")),
        file.id,
        file.size,
        file.mtime,
        0,
        &helper(),
    );
    spool
        .keep_complete(MICHAEL5400.to_owned())
        .await
        .expect("spooled");
    let never = fx.root.join("never.sh");
    executable(&never, "#!/bin/sh\nexit 1\n");
    let tools = ToolSet::refit_fixture(never).expect("tools");
    let run = super::super::dv_refit::run(&file, 0, &tools, &spool).await;
    let Ok(HelperRun::Stats(output)) = run else {
        panic!("{run:?}");
    };
    assert_eq!(
        output,
        dv_refit_title::parse(MICHAEL5400.as_bytes()).expect("run")
    );
}

/// A node crash bypasses `settle_child`; the next attempt recovers whole
/// initial or resumed lines before `open_run` truncates the old file.
#[tokio::test]
async fn a_crashed_fit_recovers_its_initial_and_resumed_scene_lines() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("scratch");
    let spool = spool_in(dir.path(), "crashed-helper");
    let lines: Vec<_> = MICHAEL5400
        .lines()
        .filter(|line| line.contains("refit_scene"))
        .map(str::to_owned)
        .collect();
    assert!(lines.len() >= 5, "fixture exercises both crash boundaries");
    let mut run = spool.open_run().await.expect("run file");
    write!(
        run,
        "{}\n{}",
        lines[..3].join("\n"),
        &lines[3][..lines[3].len() / 2]
    )
    .expect("interrupted initial output");
    drop(run);
    spool
        .recover_interrupted()
        .await
        .expect("initial crash recovered");
    assert_eq!(
        spool
            .partial()
            .await
            .into_iter()
            .map(|(line, _)| line)
            .collect::<Vec<_>>(),
        lines[..3]
    );
    let mut run = spool.open_run().await.expect("resumed run file");
    write!(
        run,
        "{}\n{}",
        lines[2..4].join("\n"),
        &lines[4][..lines[4].len() / 2]
    )
    .expect("interrupted resumed output");
    drop(run);
    spool
        .recover_interrupted()
        .await
        .expect("resumed crash recovered");
    assert_eq!(
        spool
            .partial()
            .await
            .into_iter()
            .map(|(line, _)| line)
            .collect::<Vec<_>>(),
        lines[..4]
    );
    drop(spool.open_run().await.expect("empty run file"));
    spool
        .recover_interrupted()
        .await
        .expect("crash before first line");
    assert_eq!(
        spool.partial().await.len(),
        4,
        "empty output preserves previous progress"
    );
}

/// Decision F through `execute`: a preemption mid-run keeps the helper's
/// whole scene lines and yields; the next attempt resumes at the last kept
/// scene's start, and the published table is the uninterrupted run's.
#[tokio::test]
async fn a_preempted_fit_resumes_and_equals_an_uninterrupted_one() {
    let fx = fixture().await;
    let state = app(&fx);
    std::fs::create_dir_all(fx.root.join("runtime")).expect("runtime");
    let file = title(&fx, "resumed", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let scenes: Vec<_> = (0..5)
        .map(|i| {
            (
                48 + 8 * i as u64,
                stats(12 - 3 * i, 1, i as u32),
                stats(12 - 3 * i, 1, 10 + i as u32),
            )
        })
        .collect();
    let lines = scene_lines(&scenes, 24);
    let whole = with_summary(&lines);
    let parsed: Vec<RefitScene> = lines
        .iter()
        .map(|l| RefitScene::from_line(l).expect("scene"))
        .collect();
    // The uninterrupted answer, for comparison.
    let expected = dv_refit_title::fit(dv_refit_title::parse(whole.as_bytes()).expect("whole"));
    // First run: three whole lines and half of a fourth, then it hangs.
    let first = fx.root.join("first.jsonl");
    std::fs::write(
        &first,
        format!(
            "{}\n{}",
            lines[..3].join("\n"),
            &lines[3][..lines[3].len() / 2]
        ),
    )
    .expect("first");
    // Second run: only from the third scene's start, as `--resume` asks.
    let rest = fx.root.join("rest.jsonl");
    std::fs::write(&rest, with_summary(&lines[2..])).expect("rest");
    let script = fx.root.join("resumable.sh");
    executable(
        &script,
        &format!(
            "#!/bin/sh\n[ \"$1\" = --refit-stats ] || exit 64\nif [ \"$6\" = --resume ]; then\n [ \"$7\" = {} ] && [ \"$8\" = {} ] || exit 66\n cat '{}'\n exit 0\nfi\ncat '{}'\nexec sleep 60\n",
            parsed[2].first_frame,
            parsed[2].first_pts,
            rest.display(),
            first.display()
        ),
    );
    let tools = Arc::new(ToolSet::refit_fixture(script).expect("tools"));
    let src = source(&found, &tools.refit_helper_identity());
    let spool = Spool::new(
        Spool::root(state.transcode.runtime_cache_dir()),
        src.file_id,
        src.source_size,
        src.source_mtime,
        src.video_index,
        &src.helper_identity,
    );
    let admit = || async {
        state
            .transcode
            .admit_dv_refit(super::super::dv_refit::RESERVED_THREADS)
            .await
            .unwrap_or_else(crate::transcode::FragmentAdmission::probe_only)
    };
    // Preempted once the helper has written its lines.
    let (job, active) = attempt(&fx.store, &src).await;
    let shutdown = CancellationToken::new();
    let work = tokio::spawn(execute(
        state.clone(),
        file.clone(),
        src.clone(),
        Arc::clone(&tools),
        admit().await,
        job.clone(),
        active,
        shutdown.clone(),
    ));
    let run_file = Spool::root(state.transcode.runtime_cache_dir());
    for _ in 0..200 {
        let written = std::fs::read_dir(&run_file)
            .ok()
            .and_then(|mut d| {
                d.find_map(|e| {
                    e.ok()
                        .filter(|e| e.path().extension().is_some_and(|x| x == "run"))
                })
            })
            .and_then(|e| std::fs::read(e.path()).ok())
            .map_or(0, |b| b.len());
        if written > lines[..3].join("\n").len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    shutdown.cancel();
    let published = tokio::time::timeout(std::time::Duration::from_secs(20), work)
        .await
        .expect("execute returns")
        .expect("task");
    assert!(!published);
    let yielded = fx
        .store
        .background_job(&job.id)
        .await
        .expect("job")
        .expect("job");
    assert_eq!(yielded.state, JobState::Queued, "a preemption yields");
    assert_eq!(
        yielded
            .checkpoint
            .as_ref()
            .and_then(|c| c.get("refit_stalled_yields")),
        Some(&serde_json::json!(0)),
        "progress was kept"
    );
    let kept = spool.partial().await;
    assert_eq!(kept.len(), 3, "the torn fourth line is not kept");
    assert!(kept
        .iter()
        .zip(&lines)
        .all(|((line, _), want)| line == want));
    // The next attempt resumes and publishes the uninterrupted table, once
    // the yield's short back-off has passed.
    let wait = yielded.not_before_ms.saturating_sub(now_ms()).max(0) as u64 + 50;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    let again = demand(&fx.store, file.id).await.expect("still demanded");
    assert_eq!(again.queued.as_ref().map(|q| &q.job_id), Some(&job.id));
    let (job, active) = attempt(&fx.store, &src).await;
    let published = execute(
        state.clone(),
        file.clone(),
        src.clone(),
        Arc::clone(&tools),
        admit().await,
        job.clone(),
        active,
        CancellationToken::new(),
    )
    .await;
    assert!(published, "{:?}", fx.store.background_job(&job.id).await);
    let table = table(&fx.store, file.id).await;
    assert_eq!(table.title.curve_digest, expected.curve_digest);
    assert_eq!(table.scenes.len(), 5);
    assert!(
        spool.partial().await.is_empty(),
        "the partial lines became the complete spool"
    );
    assert!(spool.complete().await.is_some());
    let listed = super::super::dv_refit::list(&Spool::root(state.transcode.runtime_cache_dir()));
    assert_eq!(listed.iter().filter(|e| e.complete).count(), 1);
}

/// A cancel from the jobs page while the helper runs: the child is reaped,
/// nothing is published, and the identity stays terminal.
#[tokio::test]
async fn a_cancelled_fit_publishes_nothing_and_stays_terminal() {
    let fx = fixture().await;
    let state = app(&fx);
    std::fs::create_dir_all(fx.root.join("runtime")).expect("runtime");
    let file = title(&fx, "cancelled", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let script = fx.root.join("hang.sh");
    executable(&script, "#!/bin/sh\nexec sleep 60\n");
    let tools = Arc::new(ToolSet::refit_fixture(script).expect("tools"));
    let src = source(&found, &tools.refit_helper_identity());
    let (job, active) = attempt(&fx.store, &src).await;
    let shutdown = CancellationToken::new();
    let admission = state
        .transcode
        .admit_dv_refit(super::super::dv_refit::RESERVED_THREADS)
        .await
        .unwrap_or_else(crate::transcode::FragmentAdmission::probe_only);
    let work = tokio::spawn(execute(
        state.clone(),
        file.clone(),
        src.clone(),
        Arc::clone(&tools),
        admission,
        job.clone(),
        active,
        shutdown.clone(),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    fx.store
        .cancel_job(CancelJob {
            job_id: job.id.clone(),
            now_ms: now_ms(),
        })
        .await
        .expect("cancel from the jobs page");
    shutdown.cancel();
    let published = tokio::time::timeout(std::time::Duration::from_secs(20), work)
        .await
        .expect("reaped")
        .expect("task");
    assert!(!published);
    assert!(fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .is_none());
    assert!(matches!(
        self::state(&fx.store, &job).await,
        JobState::Cancelling | JobState::Cancelled
    ));
    assert!(
        demand_for(&fx.store, file.id, &src.helper_identity)
            .await
            .is_none(),
        "a cancel stops automatic demand"
    );
}

/// A publication whose rows fail their own check is not sent, and the
/// attempt fails rather than retrying the same rows.
#[tokio::test]
async fn rows_that_fail_their_check_fail_the_attempt() {
    let fx = fixture().await;
    let file = title(&fx, "invalid", 7, true).await;
    let src = source(
        &demand(&fx.store, file.id).await.expect("demand"),
        &helper(),
    );
    let output = dv_refit_title::parse(mixed().as_bytes()).expect("run");
    let (mut title_row, scenes) = dv_refit_title::fit(output).rows(&src).expect("rows");
    title_row.curve_digest = "0".repeat(64);
    let (job, active) = attempt(&fx.store, &src).await;
    let fence = active.fence();
    assert!(matches!(
        fence.publish_dv_refit(title_row, scenes).await,
        Ok(RefitPublication::Invalid(_))
    ));
    assert!(fence
        .settle(JobSettlement::Fail {
            error_code: "refit_rows_invalid".into()
        })
        .await
        .expect("settle"));
    active.finish().await;
    assert_eq!(state(&fx.store, &job).await, JobState::Failed);
    assert!(fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .is_none());
}

#[tokio::test]
async fn disabling_all_platforms_yields_active_catalogue_work_until_reenabled() {
    let fx = fixture().await;
    let state = app(&fx);
    let mut task = RefitTask {
        work: Box::pin(async { false }),
        cancel: CancellationToken::new(),
        job_id: "paused-refit".into(),
        file_id: 1,
        probe_pipeline: "probe".into(),
        probe_after: None,
    };
    assert!(task.should_yield(&state).await.expect("disabled"));
    fx.store
        .put_setting(plurx_core::store::keys::DV_REFIT_PLATFORMS[0], "1")
        .await
        .expect("enable");
    assert!(!task
        .should_yield(&state)
        .await
        .expect("enabled without preempting probes"));
}

#[tokio::test]
async fn a_queued_same_source_fit_publishes_under_its_claimed_probe_generation() {
    let fx = fixture().await;
    let file = title(&fx, "queued-generation", 7, true).await;
    let old = source(
        &demand(&fx.store, file.id).await.expect("demand"),
        &helper(),
    );
    fx.store
        .enqueue_job(enqueue_request(&old, now_ms()))
        .await
        .expect("enqueue old generation");
    let fresh = RefitSource {
        probe_revision: old.probe_revision + 7,
        ..old.clone()
    };
    let (job, active) = attempt(&fx.store, &fresh).await;
    let claimed = claimed_source(fresh, &job).expect("supported fit payload");
    assert_eq!(claimed.probe_revision, old.probe_revision);
    let run = run_fake(&fx.root, &fake(&fx.root, "queued-generation", &mixed(), "")).await;
    let fence = active.fence();
    let settlement = settle_run(&fence, &job, &claimed, run).await;
    if let Some(settlement) = settlement {
        fence.settle(settlement).await.expect("settle");
    }
    active.finish().await;
    assert_eq!(state(&fx.store, &job).await, JobState::Succeeded);
}
