//! F+ curve fits on every store backend: FEL demand, bounded staging under
//! the attempt's token, atomic publication, and nothing partial left behind.
use super::{for_each_backend, seed_file};
use plurx_core::{
    domain::{DolbyVisionFacts, MediaFile, ProbeResult},
    scan::dv_title::{DvTitleStats, TitleConstant},
    store::{background_jobs::*, dv_refit_jobs::*, dv_title_jobs::*, Store},
    transcode::{dv_fel_qualification::FelVerdict, dv_refit::FIT_VERSION},
};
use std::sync::Arc;

fn helper() -> String {
    "c".repeat(64)
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}
fn probe(profile: i64, raw: &str) -> ProbeResult {
    ProbeResult {
        video_codec: Some("hevc".into()),
        dolby_vision: DolbyVisionFacts {
            profile: Some(profile),
            rpu_present: Some(true),
            el_present: Some(true),
            ..Default::default()
        },
        raw_json: Some(raw.into()),
        ..Default::default()
    }
}
async fn claim(store: &Arc<dyn Store>, id: &str, kind: JobKind) -> BackgroundJob {
    let candidate = store.background_job(id).await.expect("job").expect("job");
    let now = now();
    match store
        .claim_job(ClaimJob {
            job_id: id.into(),
            expected_revision: candidate.revision,
            node_id: "refit-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim")
    {
        ClaimOutcome::Claimed { job } => *job,
        other => panic!("expected a claim: {other:?}"),
    }
}
fn request(scope: &str, id: String) -> JobRequest {
    JobRequest {
        scope: scope.into(),
        request_id: id.clone(),
        request_digest: id,
        consumer_kind: scope.replace('-', "_"),
        consumer_ref: "1".into(),
        target_node_id: None,
        deadline_ms: None,
        retain_identity: false,
    }
}
/// A Profile 7 title with an enhancement layer and published complete title
/// facts; `fel` also stores the FEL route's verdict for its source.
async fn title(store: &Arc<dyn Store>, name: &str, fel: bool) -> MediaFile {
    title_with(store, name, 7, fel).await
}
/// Only a Profile 7 row takes a FEL verdict through the store; another
/// profile's probe document carries one as if a probe had written it.
async fn title_with(store: &Arc<dyn Store>, name: &str, profile: i64, fel: bool) -> MediaFile {
    let (_, id) = seed_file(store, name).await;
    let file = store.get_file(id).await.expect("file").expect("file");
    let verdict = serde_json::to_value(FelVerdict::unmeasurable(
        "d".repeat(64),
        file.size,
        file.mtime,
        1,
    ))
    .expect("verdict");
    let mut raw = serde_json::json!({"streams":[{"index":0,"codec_type":"video"}]});
    if fel && profile != 7 {
        raw[plurx_core::transcode::dv_fel_qualification::PROBE_KEY] = verdict.clone();
    }
    store
        .upsert_file(
            file.item_id,
            file.path.to_str().expect("path"),
            file.size,
            file.mtime,
            &probe(profile, &raw.to_string()),
        )
        .await
        .expect("DV probe");
    let scanner = "a".repeat(64);
    let demand = store
        .dv_title_demands(&scanner, 0)
        .await
        .expect("title demand")
        .into_iter()
        .find(|d| d.file_id == id)
        .expect("title demand");
    let revision = store
        .bind_dv_title_demand(demand.probe_revision, &scanner)
        .await
        .expect("bind")
        .expect("bound");
    let identity = format!("{:064x}", id);
    let EnqueueOutcome::Accepted { job_id, .. } = store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::DvTitleScan {
                file_id: id,
                source_size: file.size,
                source_mtime: file.mtime,
                probe_revision: revision,
                scanner_digest: scanner.clone(),
            },
            dedupe_key: format!("dv-title:{identity}"),
            priority: 0,
            not_before_ms: now(),
            now_ms: now(),
            request: request("dv-title", identity),
        })
        .await
        .expect("title job")
    else {
        panic!("title job accepted");
    };
    let job = claim(store, &job_id, JobKind::DvTitleScan).await;
    let published = store
        .publish_dv_title_job(PublishTitleJob {
            token: job.token.expect("token"),
            output: TitleOutput {
                file_id: id,
                probe_revision: revision,
                source_size: file.size,
                source_mtime: file.mtime,
                scanner_digest: scanner,
                stats: DvTitleStats {
                    schema: 1,
                    complete: true,
                    decoded_frames: 96,
                    valid_l1_frames: 96,
                    missing_l1_frames: 0,
                    invalid_l1_frames: 0,
                    max_l1_pq: Some(3900),
                    mastering_luminance: TitleConstant::Unknown,
                    mastering_primaries: TitleConstant::Unknown,
                },
            },
            now_ms: now(),
        })
        .await
        .expect("title facts");
    assert!(matches!(published, JobPublishOutcome::Published { .. }));
    if fel && profile == 7 {
        assert!(store
            .put_dv_fel_verdict(id, file.size, file.mtime, &verdict)
            .await
            .expect("FEL verdict"));
    }
    store.get_file(id).await.expect("file").expect("file")
}
async fn demand(store: &Arc<dyn Store>, file: i64, helper: &str) -> Option<RefitDemand> {
    store
        .dv_refit_demands(helper, FIT_VERSION, 0, 16)
        .await
        .expect("F+ demand")
        .into_iter()
        .find(|d| d.file_id == file)
}
async fn attempt(store: &Arc<dyn Store>, demand: &RefitDemand, helper: &str) -> BackgroundJob {
    let identity = format!(
        "{:016x}{:016x}{}",
        demand.file_id,
        demand.probe_revision,
        &helper[..32]
    );
    let id = match &demand.queued_job_id {
        Some(id) => id.clone(),
        None => match store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload: JobPayload::DvRefit {
                    file_id: demand.file_id,
                    source_size: demand.source_size,
                    source_mtime: demand.source_mtime,
                    probe_revision: demand.probe_revision,
                    helper_digest: helper.into(),
                    fit_version: FIT_VERSION.into(),
                },
                dedupe_key: format!("dv-refit:{identity}"),
                priority: 0,
                not_before_ms: now(),
                now_ms: now(),
                request: request("dv-refit", identity),
            })
            .await
            .expect("enqueue fit")
        {
            EnqueueOutcome::Accepted { job_id, .. } => job_id,
            other => panic!("fit accepted: {other:?}"),
        },
    };
    claim(store, &id, JobKind::DvRefit).await
}
fn scene(index: i64, curves: bool) -> StagedScene {
    let stats =
        serde_json::to_string(&plurx_core::transcode::dv_refit::RefitStats::new()).expect("stats");
    let identity = plurx_core::transcode::dv_refit::RefitCurves::identity();
    StagedScene {
        first_frame: index * 48,
        frames: 48,
        first_pts: index * 48 * 1001,
        end_pts: (index + 1) * 48 * 1001 - 1,
        time_base: "1/24000".into(),
        fit_json: stats.clone(),
        held_out_json: stats,
        curves_json: curves.then(|| serde_json::to_string(&identity).expect("curves")),
        reason: (!curves).then(|| "worse than identity on held-out samples (overall)".into()),
        verdict_json: None,
    }
}
fn fitted(file: &MediaFile, demand: &RefitDemand, helper: &str, scenes: i64) -> RefitTitle {
    RefitTitle {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        probe_revision: demand.probe_revision,
        fit_version: FIT_VERSION.into(),
        helper_digest: helper.into(),
        curve_digest: "b".repeat(64),
        verdict: RefitVerdict::Fitted,
        reason: None,
        gain: Some(0.5),
        scene_count: scenes,
        frames: scenes * 48,
    }
}
async fn stage(
    store: &Arc<dyn Store>,
    job: &BackgroundJob,
    file: i64,
    first: i64,
    scenes: Vec<StagedScene>,
) -> bool {
    store
        .stage_dv_refit_scenes(StageRefitScenes {
            token: job.token.clone().expect("token"),
            file_id: file,
            first_index: first,
            scenes,
            now_ms: now(),
        })
        .await
        .expect("stage")
}
async fn publish(
    store: &Arc<dyn Store>,
    job: &BackgroundJob,
    title: RefitTitle,
) -> JobPublishOutcome {
    store
        .publish_dv_refit_job(PublishRefitJob {
            token: job.token.clone().expect("token"),
            title,
            now_ms: now(),
        })
        .await
        .expect("publish")
}

#[tokio::test]
async fn refit_demand_selects_current_profile7_fel_titles() {
    for_each_backend(|store, backend| async move {
        let fel = title(&store, "refit-demand-fel", true).await;
        let mel = title(&store, "refit-demand-mel", false).await;
        let found = demand(&store, fel.id, &helper())
            .await
            .expect("FEL title demanded");
        assert!(found.probe_revision > 0, "{backend}");
        assert!(
            demand(&store, mel.id, &helper()).await.is_none(),
            "{backend}: no FEL facts"
        );
        // A source change keeps nothing; a fresh probe drops the title facts.
        store
            .upsert_file(
                fel.item_id,
                fel.path.to_str().expect("path"),
                fel.size,
                fel.mtime,
                &probe(7, "{}"),
            )
            .await
            .expect("reprobe");
        assert!(
            demand(&store, fel.id, &helper()).await.is_none(),
            "{backend}: no title facts"
        );
        let p8 = title_with(&store, "refit-demand-p8", 8, true).await;
        assert!(
            demand(&store, p8.id, &helper()).await.is_none(),
            "{backend}: not Profile 7"
        );
    })
    .await;
}

#[tokio::test]
async fn refit_publication_adopts_staged_pages_atomically_and_replaces_the_old_fit() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-publish", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        assert!(
            stage(
                &store,
                &job,
                file.id,
                0,
                vec![scene(0, true), scene(1, false)]
            )
            .await,
            "{backend}"
        );
        assert!(
            stage(&store, &job, file.id, 2, vec![scene(2, true)]).await,
            "{backend}"
        );
        // Restaging a page is idempotent.
        assert!(
            stage(&store, &job, file.id, 2, vec![scene(2, true)]).await,
            "{backend}"
        );
        // A title that does not match its staged scenes is refused.
        assert!(
            matches!(
                publish(&store, &job, fitted(&file, &found, &helper(), 4)).await,
                JobPublishOutcome::ArtifactConflict
            ),
            "{backend}"
        );
        let title_row = fitted(&file, &found, &helper(), 3);
        assert!(
            matches!(
                publish(&store, &job, title_row.clone()).await,
                JobPublishOutcome::Published { .. }
            ),
            "{backend}"
        );
        assert!(
            matches!(
                publish(&store, &job, title_row.clone()).await,
                JobPublishOutcome::AlreadyPublished { .. }
            ),
            "{backend}: idempotent replay"
        );
        let table = store
            .dv_refit_curves(file.id)
            .await
            .expect("curves")
            .expect("table");
        assert_eq!(table.title, title_row, "{backend}");
        let shape: Vec<(i64, i64, bool)> = table
            .scenes
            .iter()
            .map(|s| (s.first_frame, s.first_pts, s.curves.is_some()))
            .collect();
        assert_eq!(
            shape,
            [(0, 0, true), (48, 48 * 1001, false), (96, 96 * 1001, true)],
            "{backend}"
        );
        assert_eq!(
            table.scenes[0].curves,
            Some(plurx_core::transcode::dv_refit::RefitCurves::identity()),
            "{backend}: stored curves read back exactly"
        );
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: current"
        );
        assert_eq!(
            store
                .background_job(&job.id)
                .await
                .expect("job")
                .expect("job")
                .state,
            JobState::Succeeded
        );
        // A later helper's fit replaces this one whole, and an identity
        // title serves no curves.
        let newer = "e".repeat(64);
        let found = demand(&store, file.id, &newer)
            .await
            .expect("stale for a new helper");
        let job = attempt(&store, &found, &newer).await;
        assert!(stage(&store, &job, file.id, 0, vec![scene(0, true)]).await);
        let mut identity = fitted(&file, &found, &newer, 1);
        identity.verdict = RefitVerdict::Identity;
        identity.reason = Some("under the floor".into());
        assert!(matches!(
            publish(&store, &job, identity.clone()).await,
            JobPublishOutcome::Published { .. }
        ));
        let table = store
            .dv_refit_curves(file.id)
            .await
            .expect("curves")
            .expect("table");
        assert_eq!(table.title, identity, "{backend}");
        assert_eq!(
            table.scenes.len(),
            1,
            "{backend}: the old scenes went with the old fit"
        );
        assert!(
            table.scenes[0].curves.is_none(),
            "{backend}: identity serves no curves"
        );
        // A changed source keeps nothing.
        store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("path"),
                file.size + 1,
                file.mtime,
                &probe(7, "{}"),
            )
            .await
            .expect("source changed");
        assert!(
            store
                .dv_refit_curves(file.id)
                .await
                .expect("curves")
                .is_none(),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_cancelled_or_lost_refit_attempt_leaves_no_rows() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-cancel", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        assert!(stage(&store, &job, file.id, 0, vec![scene(0, true)]).await);
        store
            .cancel_job(CancelJob {
                job_id: job.id.clone(),
                now_ms: now(),
            })
            .await
            .expect("cancel");
        assert!(
            !stage(&store, &job, file.id, 1, vec![scene(1, true)]).await,
            "{backend}: fenced"
        );
        assert!(
            matches!(
                publish(&store, &job, fitted(&file, &found, &helper(), 1)).await,
                JobPublishOutcome::LostOwnership
            ),
            "{backend}"
        );
        assert!(
            store
                .dv_refit_curves(file.id)
                .await
                .expect("curves")
                .is_none(),
            "{backend}"
        );
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: a cancel stops demand"
        );
    })
    .await;
}

#[tokio::test]
async fn a_refit_refusal_is_a_recorded_title_without_scenes() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-refused", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        let mut refused = fitted(&file, &found, &helper(), 0);
        refused.verdict = RefitVerdict::Refused;
        refused.reason = Some("not a FEL title".into());
        refused.gain = None;
        assert!(matches!(
            publish(&store, &job, refused.clone()).await,
            JobPublishOutcome::Published { .. }
        ));
        let table = store
            .dv_refit_curves(file.id)
            .await
            .expect("curves")
            .expect("table");
        assert_eq!((table.title, table.scenes.len()), (refused, 0), "{backend}");
    })
    .await;
}
