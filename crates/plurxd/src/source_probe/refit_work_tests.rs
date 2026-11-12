//! The F+ job against a real store and a fake helper child: demand, the
//! helper contract through the bounded child owner, publication, refusal,
//! cancellation.
use super::super::dv_refit::{classify, spawn, HelperRun, RunError};
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
    transcode::{dv_fel_qualification::FelVerdict, dv_refit::RefitStats},
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

/// Helper output in the milestone 2 contract: one line per scene, then the
/// summary. `median` is the RPU's own median scene length (under 24 frames
/// the title is degenerate).
fn recorded(scenes: &[(u64, RefitStats, RefitStats)], median: f64) -> String {
    let mut lines = Vec::new();
    let mut frame = 0u64;
    let pts = |frame: u64| 125 + frame as i64 * 42;
    for (frames, fit, held_out) in scenes {
        lines.push(
            serde_json::json!({"kind":"refit_scene","first_frame":frame,"frames":frames,
                "first_pts":pts(frame),"end_pts":pts(frame + frames) - 1,"time_base":"1/1000",
                "fit":fit,"held_out":held_out})
            .to_string(),
        );
        frame += frames;
    }
    let fit: f64 = scenes.iter().map(|s| s.1.samples()).sum();
    let held: f64 = scenes.iter().map(|s| s.2.samples()).sum();
    lines.push(
        serde_json::json!({"kind":"refit_summary","frames":frame,"scenes":scenes.len(),
            "rpu_scenes":scenes.len() * 2,"median_scene_frames":median,
            "degenerate":median < 24.0,"fit_samples":fit as u64,"held_out_samples":held as u64,
            "first_pts":pts(0),"end_pts":pts(frame) - 1,"time_base":"1/1000"})
        .to_string(),
    );
    lines.join("\n") + "\n"
}

/// The helper's own output for the Michael5400 window (milestone 2).
const MICHAEL5400: &str =
    include_str!("../../../plurx-core/tests/fixtures/dv-refit/helper-run-michael5400.jsonl");

/// Two scenes the fit helps; one whose held-out samples refuse its fit.
fn mixed() -> String {
    recorded(
        &[
            (48, stats(12, 0, 0), stats(12, 0, 1)),
            (96, stats(-10, 0, 2), stats(-10, 0, 3)),
            (72, stats(30, 0, 4), stats(0, 0, 5)),
        ],
        48.0,
    )
}

/// A fake helper: prints `stdout` (from a file), then runs `tail`.
fn fake(dir: &Path, name: &str, stdout: &str, tail: &str) -> PathBuf {
    let data = dir.join(format!("{name}.jsonl"));
    std::fs::write(&data, stdout).expect("recorded output");
    let script = dir.join(format!("{name}.sh"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --refit-stats ] || exit 64\n[ \"$BUNDLE\" = kept ] || exit 65\ncat '{}'\n{tail}\n",
            data.display()
        ),
    )
    .expect("fake helper");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("mode");
    script
}

async fn run_fake(script: &Path) -> Result<HelperRun, RunError> {
    // A just-written script can briefly be busy for exec in a threaded test.
    for _ in 0..20 {
        let output = spawn(
            script,
            &[("BUNDLE".into(), "kept".into())],
            "/dev/null",
            0,
            |_| {},
        )
        .await;
        if !matches!(&output, Err(e) if e.raw_os_error() == Some(libc::ETXTBSY)) {
            return classify(output);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("fake helper stayed busy");
}

struct Fixture {
    _dir: tempfile::TempDir,
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
        _dir: dir,
        root,
        store,
        library,
    }
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

async fn demand(store: &Arc<dyn Store>, file: i64) -> Option<RefitDemand> {
    store
        .dv_refit_demands(&helper(), FIT_VERSION, 0, 16)
        .await
        .expect("F+ demand")
        .into_iter()
        .find(|d| d.file_id == file)
}

/// Enqueue and claim the file's fit, as `RefitTask::start` does.
async fn attempt(
    store: &Arc<dyn Store>,
    demand: &RefitDemand,
) -> (BackgroundJob, ActiveBackgroundJob) {
    let EnqueueOutcome::Accepted { job_id, .. } = store
        .enqueue_job(enqueue_request(demand, &helper(), now_ms()))
        .await
        .expect("enqueue")
    else {
        panic!("fit accepted");
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

/// One whole attempt with a fake helper's answer; returns whether it published.
async fn fit_with(
    store: &Arc<dyn Store>,
    demand: &RefitDemand,
    run: Result<HelperRun, RunError>,
) -> (BackgroundJob, Option<JobSettlement>) {
    let (job, active) = attempt(store, demand).await;
    let fence = active.fence();
    let settlement = settle_run(&fence, &job, demand, &helper(), run).await;
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
    assert!(found.queued_job_id.is_none());
    assert!(
        demand(&fx.store, mel.id).await.is_none(),
        "no FEL facts: MEL or unknown"
    );
    assert!(demand(&fx.store, p8.id).await.is_none(), "not Profile 7");
    // Another helper or fit version is stale; publishing makes it current.
    assert!(!fx
        .store
        .dv_refit_demands(&"f".repeat(64), FIT_VERSION, 0, 16)
        .await
        .expect("other helper")
        .is_empty());
    let run = run_fake(&fake(&fx.root, "demand", &mixed(), "")).await;
    let (job, settlement) = fit_with(&fx.store, &found, run).await;
    assert!(settlement.is_none(), "{settlement:?}");
    assert_eq!(state(&fx.store, &job).await, JobState::Succeeded);
    assert!(
        demand(&fx.store, fel.id).await.is_none(),
        "current after publication"
    );
    assert!(
        fx.store
            .dv_refit_demands(&"f".repeat(64), FIT_VERSION, 0, 16)
            .await
            .expect("other helper")
            .iter()
            .any(|d| d.file_id == fel.id),
        "a new helper makes the fit stale"
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

#[tokio::test]
async fn a_fake_helper_run_publishes_solved_and_refused_scenes() {
    let fx = fixture().await;
    let file = title(&fx, "mixed", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let run = run_fake(&fake(&fx.root, "mixed", &mixed(), "")).await;
    assert!(matches!(run, Ok(HelperRun::Stats(_))), "{run:?}");
    let (_, settlement) = fit_with(&fx.store, &found, run).await;
    assert!(settlement.is_none(), "{settlement:?}");
    let table = fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .expect("a current curve table");
    assert_eq!(table.title.verdict, RefitVerdict::Fitted);
    assert_eq!(table.title.fit_version, FIT_VERSION);
    assert_eq!(table.title.helper_digest, helper());
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
    // The stored curves and keys reproduce the stored digest.
    let composer: Vec<_> = table
        .scenes
        .iter()
        .map(|s| {
            s.curves.as_ref().map(|c| {
                plurx_core::transcode::dv_refit::servable(c)
                    .expect("stored curves are servable")
                    .curves()
                    .clone()
            })
        })
        .collect();
    let digest = dv_refit_title::curve_digest(
        table
            .scenes
            .iter()
            .zip(&composer)
            .map(|(s, c)| (s.first_frame, s.first_pts, s.time_base.as_str(), c.as_ref())),
    );
    assert_eq!(digest, table.title.curve_digest);
}

#[tokio::test]
async fn degenerate_and_low_gain_titles_store_identity_curves() {
    let fx = fixture().await;
    for (name, output, reason) in [
        (
            "degenerate",
            recorded(&[(48, stats(12, 0, 0), stats(12, 0, 1))], 12.0),
            "scene_refresh_flag",
        ),
        (
            "low-gain",
            recorded(
                &[
                    (48, stats(2, 20, 0), stats(2, 20, 1)),
                    (48, stats(2, 20, 2), stats(2, 20, 3)),
                ],
                48.0,
            ),
            "floor",
        ),
    ] {
        let file = title(&fx, name, 7, true).await;
        let found = demand(&fx.store, file.id).await.expect("demand");
        let run = run_fake(&fake(&fx.root, name, &output, "")).await;
        let (_, settlement) = fit_with(&fx.store, &found, run).await;
        assert!(settlement.is_none(), "{name}: {settlement:?}");
        let table = fx
            .store
            .dv_refit_curves(file.id)
            .await
            .expect("curves")
            .expect("table");
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
        let found = demand(&fx.store, file.id).await.expect("demand");
        let run = run_fake(&fake(&fx.root, name, MICHAEL5400, "")).await;
        fit_with(&fx.store, &found, run).await;
        digests.push(
            fx.store
                .dv_refit_curves(file.id)
                .await
                .expect("curves")
                .expect("table")
                .title
                .curve_digest,
        );
    }
    assert_eq!(digests[0], digests[1]);
}

#[tokio::test]
async fn a_helper_refusal_is_recorded_and_ends_demand() {
    let fx = fixture().await;
    let file = title(&fx, "refused", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    // The helper refuses a full-resolution EL with one stderr line.
    let reason = "refit-stats: full-resolution enhancement layer is not supported";
    let run = run_fake(&fake(
        &fx.root,
        "refused",
        "",
        &format!("echo '{reason}' >&2; exit 3"),
    ))
    .await;
    assert!(
        matches!(&run, Ok(HelperRun::Refused(r)) if r == reason),
        "{run:?}"
    );
    let (_, settlement) = fit_with(&fx.store, &found, run).await;
    assert!(settlement.is_none());
    let table = fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .expect("table");
    assert_eq!(table.title.verdict, RefitVerdict::Refused);
    assert_eq!(table.title.reason.as_deref(), Some(reason));
    assert!(table.scenes.is_empty());
    assert!(demand(&fx.store, file.id).await.is_none());
    // A crash is not a refusal.
    let crash = run_fake(&fake(
        &fx.root,
        "crash",
        "",
        "echo one >&2; echo two >&2; exit 1",
    ))
    .await;
    assert!(matches!(crash, Err(RunError::Transient(_))), "{crash:?}");
    let killed = run_fake(&fake(&fx.root, "killed", "", "kill -9 $$")).await;
    assert!(matches!(killed, Err(RunError::Transient(_))), "{killed:?}");
}

#[tokio::test]
async fn malformed_or_truncated_output_fails_with_no_rows() {
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
        let found = demand(&fx.store, file.id).await.expect("demand");
        let run = run_fake(&fake(&fx.root, name, &output, "")).await;
        assert!(
            matches!(run, Err(RunError::Malformed(_))),
            "{name}: {run:?}"
        );
        let (job, settlement) = fit_with(&fx.store, &found, run).await;
        assert!(
            matches!(settlement, Some(JobSettlement::Fail { .. })),
            "{name}"
        );
        assert_eq!(state(&fx.store, &job).await, JobState::Failed, "{name}");
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
    }
}

#[tokio::test]
async fn cancellation_leaves_no_rows() {
    let fx = fixture().await;
    // Cancelled while the helper runs: the child is reaped, nothing staged.
    let file = title(&fx, "cancel-run", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let (job, active) = attempt(&fx.store, &found).await;
    let script = fake(&fx.root, "slow", "", "exec sleep 60");
    let token = CancellationToken::new();
    let stop = token.clone();
    let running = tokio::spawn(async move {
        plurx_core::process::bounded::cancellable(stop, run_fake(&script)).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    fx.store
        .cancel_job(CancelJob {
            job_id: job.id.clone(),
            now_ms: now_ms(),
        })
        .await
        .expect("cancel from the jobs page");
    token.cancel();
    let run = tokio::time::timeout(std::time::Duration::from_secs(10), running)
        .await
        .expect("reaped")
        .expect("task");
    assert!(matches!(run, Err(RunError::Transient(_))), "{run:?}");
    active.finish().await;
    assert!(fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .is_none());
    assert!(matches!(
        state(&fx.store, &job).await,
        JobState::Cancelling | JobState::Cancelled
    ));
    // Cancelled after the helper answered: publication is fenced, no rows.
    let file = title(&fx, "cancel-publish", 7, true).await;
    let found = demand(&fx.store, file.id).await.expect("demand");
    let (job, active) = attempt(&fx.store, &found).await;
    fx.store
        .cancel_job(CancelJob {
            job_id: job.id.clone(),
            now_ms: now_ms(),
        })
        .await
        .expect("cancel");
    let run = run_fake(&fake(&fx.root, "cancel-publish", &mixed(), "")).await;
    let settlement = settle_run(&active.fence(), &job, &found, &helper(), run).await;
    assert!(settlement.is_some(), "a cancelled attempt does not publish");
    active.finish().await;
    assert!(fx
        .store
        .dv_refit_curves(file.id)
        .await
        .expect("curves")
        .is_none());
    assert!(
        demand(&fx.store, file.id).await.is_none(),
        "a cancel stops automatic demand"
    );
}
