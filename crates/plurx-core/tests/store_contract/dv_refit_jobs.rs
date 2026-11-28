//! F+ curve fits on every store backend: FEL demand, the one-write fenced
//! publication, digest-addressed tables and their seven-day retention, the
//! durable terminal outcome of an identity, the generation rebind, the
//! Developer card's counts, and the versioned M7 reopen those fits feed.
use super::{for_each_backend, seed_file};
use plurx_core::{
    domain::{DolbyVisionFacts, MediaFile, ProbeResult},
    scan::dv_title::{DvTitleStats, TitleConstant},
    store::{
        background_jobs::*,
        dv_m7_outcomes::{M7OutcomeKind, M7TitleOutcome},
        dv_refit_jobs::*,
        dv_title_jobs::*,
        Store,
    },
    transcode::{
        dv_fel_qualification::FelVerdict,
        dv_refit::{servable, QuantisedCurves, RefitCurves, FIT_VERSION},
        dv_refit_title::{curve_digest, empty_digest, DigestScene},
    },
};
use std::sync::Arc;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

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
        .dv_title_demands(&scanner, 0, now())
        .await
        .expect("title demand")
        .into_iter()
        .find(|d| d.file_id == id)
        .expect("title demand");
    let revision = store
        .bind_dv_title_demand(demand.probe_revision, &scanner, 1, 1_000)
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
                    el_kind: if fel && profile == 7 {
                        TitleConstant::Consistent(plurx_core::scan::dv_title::TitleElKind::Fel)
                    } else {
                        TitleConstant::Consistent(plurx_core::scan::dv_title::TitleElKind::Mel)
                    },
                },
            },
            m7: None,
            now_ms: now(),
        })
        .await
        .expect("title facts");
    assert!(matches!(published, JobPublishOutcome::Published { .. }));
    if fel && profile == 7 && !name.contains("no-recipe") {
        assert!(store
            .put_dv_fel_verdict(id, file.size, file.mtime, &verdict)
            .await
            .expect("FEL verdict"));
    }
    store.get_file(id).await.expect("file").expect("file")
}
async fn demand(store: &Arc<dyn Store>, file: i64, helper: &str) -> Option<RefitDemand> {
    store
        .dv_refit_demands(helper, FIT_VERSION, true, 0, 16)
        .await
        .expect("F+ demand")
        .into_iter()
        .find(|d| d.file_id == file)
}
/// The fit identity's dedupe key, as plurxd names it (here a stand-in that
/// covers the same fields: file, helper; the fixture's source and stream do
/// not change).
fn identity(file: i64, helper: &str) -> String {
    format!("dv-refit:{file:032x}{}", &helper[..32])
}
fn payload(demand: &RefitDemand, helper: &str) -> JobPayload {
    JobPayload::DvRefit {
        file_id: demand.file_id,
        source_size: demand.source_size,
        source_mtime: demand.source_mtime,
        video_index: 0,
        probe_revision: demand.probe_revision,
        helper_identity: helper.into(),
        fit_version: FIT_VERSION.into(),
    }
}
async fn attempt(store: &Arc<dyn Store>, demand: &RefitDemand, helper: &str) -> BackgroundJob {
    let key = identity(demand.file_id, helper);
    let id = match &demand.queued {
        Some(queued) if queued.identity == key => queued.job_id.clone(),
        _ => match store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload: payload(demand, helper),
                dedupe_key: key.clone(),
                priority: 0,
                not_before_ms: now(),
                now_ms: now(),
                request: request("dv-refit", key.trim_start_matches("dv-refit:").into()),
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
fn identity_curves() -> QuantisedCurves {
    servable(&RefitCurves::identity())
        .expect("identity is servable")
        .quantised()
}
/// A table of `count` 48-frame scenes, every other one serving the identity
/// coefficients when the title is fitted. `shift` moves every scene's
/// presentation time, so two tables of one file have different digests.
fn table_rows(
    file: &MediaFile,
    demand: &RefitDemand,
    helper: &str,
    count: i64,
    verdict: RefitVerdict,
    shift: i64,
) -> (RefitTitle, Vec<RefitSceneRow>) {
    let scenes: Vec<RefitSceneRow> = (0..count)
        .map(|i| {
            let served = verdict == RefitVerdict::Fitted && i % 2 == 0;
            RefitSceneRow {
                first_frame: i * 48,
                frames: 48,
                first_pts: shift + i * 48 * 1001,
                end_pts: shift + (i + 1) * 48 * 1001 - 1,
                time_base: "1/24000".into(),
                coefficients: served.then(identity_curves),
                reason: (i % 2 == 1)
                    .then(|| "worse than identity on held-out samples (overall)".into()),
                identity_error: 1234.5 + i as f64 / 3.0,
                fitted_error: (i % 2 == 0).then_some(0.1 + i as f64 / 7.0),
                gain: (i % 2 == 0).then_some(0.9 - 1e-17),
            }
        })
        .collect();
    let digest = if scenes.is_empty() {
        empty_digest()
    } else {
        curve_digest(scenes.iter().map(|s| DigestScene {
            first_frame: s.first_frame as u64,
            frames: s.frames as u64,
            first_pts: s.first_pts,
            end_pts: s.end_pts,
            time_base: &s.time_base,
            coefficients: s.coefficients.as_ref(),
        }))
    };
    let title = RefitTitle {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        video_index: 0,
        probe_revision: demand.probe_revision,
        helper_identity: helper.into(),
        fit_version: FIT_VERSION.into(),
        curve_digest: digest,
        verdict,
        reason: (verdict != RefitVerdict::Fitted).then(|| "under the floor".into()),
        gain: (verdict != RefitVerdict::Refused).then_some(0.123_456_789_012_345_67),
        scene_count: count,
        frames: count * 48,
    };
    (title, scenes)
}
async fn publish(
    store: &Arc<dyn Store>,
    job: &BackgroundJob,
    rows: (RefitTitle, Vec<RefitSceneRow>),
) -> JobPublishOutcome {
    publish_at(store, job, rows, now()).await
}
async fn publish_at(
    store: &Arc<dyn Store>,
    job: &BackgroundJob,
    (title, scenes): (RefitTitle, Vec<RefitSceneRow>),
    now_ms: i64,
) -> JobPublishOutcome {
    store
        .publish_dv_refit_job(PublishRefitJob {
            token: job.token.clone().expect("token"),
            title,
            scenes,
            now_ms,
        })
        .await
        .expect("publish")
}
async fn current(store: &Arc<dyn Store>, file: i64) -> Option<RefitCurveTable> {
    store.dv_refit_curves(file).await.expect("curves")
}
async fn by_digest(store: &Arc<dyn Store>, file: i64, digest: &str) -> Option<RefitCurveTable> {
    store
        .dv_refit_curves_by_digest(file, digest)
        .await
        .expect("curves by digest")
}

#[tokio::test]
async fn refit_demand_selects_current_profile7_fel_titles() {
    for_each_backend(|store, backend| async move {
        let fel = title(&store, "refit-demand-fel", true).await;
        let mel = title(&store, "refit-demand-mel", false).await;
        let no_recipe = title(&store, "refit-demand-no-recipe", true).await;
        assert!(
            demand(&store, no_recipe.id, &helper()).await.is_some(),
            "{backend}: complete FEL fact does not require a legacy recipe"
        );
        let found = demand(&store, fel.id, &helper())
            .await
            .expect("FEL title demanded");
        assert!(found.probe_revision > 0, "{backend}");
        assert_eq!(
            (
                found.stored_video_index,
                found.terminal.len(),
                found.queued.is_none()
            ),
            (None, 0, true),
            "{backend}"
        );
        assert!(
            demand(&store, mel.id, &helper()).await.is_none(),
            "{backend}: no FEL facts"
        );
        let progress = store.dv_refit_progress().await.expect("progress");
        assert!(progress.fel_titles >= 1, "{backend}: {progress:?}");
        let p8 = title_with(&store, "refit-demand-p8", 8, true).await;
        assert!(
            demand(&store, p8.id, &helper()).await.is_none(),
            "{backend}: not Profile 7"
        );
        // The complete title aggregate is a precondition: a fresh probe
        // drops the title facts and the demand with them.
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
    })
    .await;
}

/// Decision A and amendment 2: one replicated write per title, exact error
/// numbers, the previous table superseded but addressable by digest for
/// seven days, then gone.
#[tokio::test]
async fn refit_publication_supersedes_and_keeps_old_tables_for_seven_days() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-publish", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        let first = table_rows(&file, &found, &helper(), 3, RefitVerdict::Fitted, 0);
        let published_at = now();
        assert!(
            matches!(
                publish_at(&store, &job, first.clone(), published_at).await,
                JobPublishOutcome::Published { .. }
            ),
            "{backend}"
        );
        assert!(
            matches!(
                publish(&store, &job, first.clone()).await,
                JobPublishOutcome::AlreadyPublished { .. }
            ),
            "{backend}: idempotent replay"
        );
        let waiter_ref = store
            .background_job(&job.id)
            .await
            .expect("job")
            .expect("job");
        assert_eq!(waiter_ref.state, JobState::Succeeded, "{backend}");
        let table = current(&store, file.id).await.expect("table");
        assert_eq!(
            table.title, first.0,
            "{backend}: exact title, gain included"
        );
        assert_eq!(table.superseded_at_ms, None);
        let shape: Vec<(i64, i64, i64, bool)> = table
            .scenes
            .iter()
            .map(|s| (s.first_frame, s.first_pts, s.end_pts, s.curves.is_some()))
            .collect();
        assert_eq!(
            shape,
            [
                (0, 0, 48 * 1001 - 1, true),
                (48, 48 * 1001, 96 * 1001 - 1, false),
                (96, 96 * 1001, 144 * 1001 - 1, true)
            ],
            "{backend}"
        );
        assert_eq!(
            table.scenes[0].curves.as_ref().map(|c| c.quantised()),
            Some(identity_curves()),
            "{backend}: stored coefficients read back exactly"
        );
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: current"
        );
        // A later helper's table supersedes it; an identity title serves no curves.
        let newer = "e".repeat(64);
        let found = demand(&store, file.id, &newer)
            .await
            .expect("stale for a new helper");
        assert_eq!(found.stored_video_index, None, "{backend}: other helper");
        let job = attempt(&store, &found, &newer).await;
        let second = table_rows(&file, &found, &newer, 1, RefitVerdict::Identity, 7);
        assert!(matches!(
            publish(&store, &job, second.clone()).await,
            JobPublishOutcome::Published { .. }
        ));
        let table = current(&store, file.id).await.expect("table");
        assert_eq!(table.title, second.0, "{backend}");
        assert!(
            table.scenes[0].curves.is_none(),
            "{backend}: identity serves no curves"
        );
        let old = by_digest(&store, file.id, &first.0.curve_digest)
            .await
            .expect("the superseded table stays addressable");
        assert_eq!(old.title, first.0, "{backend}");
        assert_eq!(old.scenes.len(), 3, "{backend}");
        let superseded = old.superseded_at_ms.expect("superseded");
        assert!(superseded >= published_at, "{backend}");
        assert_eq!(
            by_digest(&store, file.id, &second.0.curve_digest)
                .await
                .map(|t| t.title),
            Some(second.0.clone()),
            "{backend}: the current table by its digest"
        );
        assert!(by_digest(&store, file.id, &"f".repeat(64)).await.is_none());
        // Retention: kept until seven days after supersession, then dropped.
        store
            .prune_dv_refit(superseded + 7 * DAY_MS - 1)
            .await
            .expect("prune");
        assert!(
            by_digest(&store, file.id, &first.0.curve_digest)
                .await
                .is_some(),
            "{backend}: still inside the window"
        );
        store
            .prune_dv_refit(superseded + 7 * DAY_MS)
            .await
            .expect("prune");
        assert!(
            by_digest(&store, file.id, &first.0.curve_digest)
                .await
                .is_none(),
            "{backend}: dropped after seven days"
        );
        assert_eq!(
            current(&store, file.id).await.map(|t| t.title),
            Some(second.0),
            "{backend}: the current table never ages out"
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
        assert!(current(&store, file.id).await.is_none(), "{backend}");
    })
    .await;
}

/// Decision G: a newer title-scan generation that selects the same stream
/// of the same source rebinds the table instead of decoding again.
#[tokio::test]
async fn a_newer_generation_rebinds_the_stored_table() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-rebind", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        let mut rows = table_rows(&file, &found, &helper(), 2, RefitVerdict::Fitted, 0);
        // The table was read under an older generation.
        rows.0.probe_revision = found.probe_revision;
        assert!(matches!(
            publish(&store, &job, rows.clone()).await,
            JobPublishOutcome::Published { .. }
        ));
        let older = found.probe_revision + 5;
        store
            .rebind_dv_refit(RebindRefit {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                video_index: 0,
                helper_identity: helper(),
                fit_version: FIT_VERSION.into(),
                identity: identity(file.id, &helper()),
                probe_revision: older,
            })
            .await
            .expect("rebind");
        let table = current(&store, file.id).await.expect("table");
        assert_eq!(table.title.probe_revision, older, "{backend}");
        let found = demand(&store, file.id, &helper())
            .await
            .expect("its generation differs from the current one");
        assert_eq!(found.stored_video_index, Some(0), "{backend}");
        store
            .rebind_dv_refit(RebindRefit {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                video_index: 0,
                helper_identity: helper(),
                fit_version: FIT_VERSION.into(),
                identity: identity(file.id, &helper()),
                probe_revision: found.probe_revision,
            })
            .await
            .expect("rebind");
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: rebound, no work"
        );
        // A rebind for another stream moves nothing.
        store
            .rebind_dv_refit(RebindRefit {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                video_index: 1,
                helper_identity: helper(),
                fit_version: FIT_VERSION.into(),
                identity: identity(file.id, &helper()),
                probe_revision: older,
            })
            .await
            .expect("rebind");
        assert_eq!(
            current(&store, file.id)
                .await
                .expect("table")
                .title
                .probe_revision,
            found.probe_revision,
            "{backend}"
        );
    })
    .await;
}

/// Decision E: a cancel fences the attempt and stays terminal for the
/// identity; an administrator's retry lifts it.
#[tokio::test]
async fn a_cancelled_identity_stays_terminal_until_retried() {
    for_each_backend(|store, backend| async move {
        // Both titles first: a cancelling attempt holds the node's title lane.
        let queued_file = title(&store, "refit-cancel-queued", true).await;
        // A running attempt cancelled from the jobs page publishes nothing.
        let file = title(&store, "refit-cancel-run", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        store
            .cancel_job(CancelJob {
                job_id: job.id.clone(),
                now_ms: now(),
            })
            .await
            .expect("cancel");
        assert!(
            matches!(
                publish(
                    &store,
                    &job,
                    table_rows(&file, &found, &helper(), 1, RefitVerdict::Fitted, 0)
                )
                .await,
                JobPublishOutcome::LostOwnership
            ),
            "{backend}"
        );
        assert!(current(&store, file.id).await.is_none(), "{backend}");
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: a cancelling attempt holds the title"
        );
        // A queued identity cancelled outright stays terminal.
        let file = queued_file;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let key = identity(file.id, &helper());
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload: payload(&found, &helper()),
                dedupe_key: key.clone(),
                priority: 0,
                not_before_ms: now(),
                now_ms: now(),
                request: request("dv-refit", key.trim_start_matches("dv-refit:").into()),
            })
            .await
            .expect("enqueue")
        else {
            panic!("accepted");
        };
        assert_eq!(
            demand(&store, file.id, &helper())
                .await
                .and_then(|d| d.queued)
                .map(|q| q.identity),
            Some(key.clone()),
            "{backend}: the queued identity is offered to resume"
        );
        store
            .cancel_job(CancelJob {
                job_id: job_id.clone(),
                now_ms: now(),
            })
            .await
            .expect("cancel");
        assert_eq!(
            store
                .background_job(&job_id)
                .await
                .expect("job")
                .expect("job")
                .state,
            JobState::Cancelled
        );
        assert!(
            demand(&store, file.id, &helper()).await.is_none(),
            "{backend}: a cancel is terminal for its identity"
        );
        // Another helper is another identity.
        assert!(
            demand(&store, file.id, &"e".repeat(64)).await.is_some(),
            "{backend}"
        );
        let retried = store
            .retry_background_job(&job_id, &uuid::Uuid::new_v4().to_string(), 1, now())
            .await
            .expect("retry")
            .expect("retried");
        assert!(
            matches!(retried, EnqueueOutcome::Accepted { .. }),
            "{backend}: {retried:?}"
        );
        let found = demand(&store, file.id, &helper())
            .await
            .expect("a retry lifts the outcome");
        assert_eq!(found.queued.map(|q| q.identity), Some(key), "{backend}");
    })
    .await;
}

/// A token whose lease has run out publishes nothing, on either backend.
#[tokio::test]
async fn an_expired_lease_publishes_nothing() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "refit-lease", true).await;
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        let expiry = job.token.as_ref().expect("token").lease_expires_ms;
        let rows = table_rows(&file, &found, &helper(), 1, RefitVerdict::Fitted, 0);
        assert!(
            matches!(
                publish_at(&store, &job, rows.clone(), expiry).await,
                JobPublishOutcome::LostOwnership
            ),
            "{backend}: at expiry"
        );
        assert!(current(&store, file.id).await.is_none(), "{backend}");
        assert!(
            matches!(
                publish_at(&store, &job, rows, expiry - 1).await,
                JobPublishOutcome::Published { .. }
            ),
            "{backend}: inside the lease"
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
        let refused = table_rows(&file, &found, &helper(), 0, RefitVerdict::Refused, 0);
        assert!(matches!(
            publish(&store, &job, refused.clone()).await,
            JobPublishOutcome::Published { .. }
        ));
        let table = current(&store, file.id).await.expect("table");
        assert_eq!(
            (table.title, table.scenes.len()),
            (refused.0, 0),
            "{backend}"
        );
        let progress = store.dv_refit_progress().await.expect("progress");
        assert!(progress.refused >= 1, "{backend}: {progress:?}");
        // An invalid publication is refused before it reaches the store.
        let job_rows = table_rows(&file, &found, &helper(), 2, RefitVerdict::Fitted, 0);
        let mut bad = job_rows.clone();
        bad.1[0].identity_error = f64::NAN;
        assert!(store
            .publish_dv_refit_job(PublishRefitJob {
                token: job.token.clone().expect("token"),
                title: bad.0,
                scenes: bad.1,
                now_ms: now(),
            })
            .await
            .is_err());
    })
    .await;
}

/// Run the reopened title job of `file` at its current generation and settle
/// it at `now_ms` with M7 `outcome`, as the title worker does with a stored
/// aggregate.
async fn resettle_title(
    store: &Arc<dyn Store>,
    file: &MediaFile,
    now_ms: i64,
    outcome: Option<M7TitleOutcome>,
) {
    let scanner = "a".repeat(64);
    let demand = store
        .dv_title_demands(&scanner, 0, now_ms)
        .await
        .expect("title demand")
        .into_iter()
        .find(|d| d.file_id == file.id)
        .expect("reopened title demand");
    let stats = DvTitleStats::from_probe_json(&demand.probe_json).expect("stored aggregate");
    let job_id = queued_attempt(store, file, now_ms).await;
    let job = claim(store, &job_id, JobKind::DvTitleScan).await;
    let published = store
        .publish_dv_title_job(PublishTitleJob {
            token: job.token.expect("token"),
            output: TitleOutput {
                file_id: file.id,
                probe_revision: demand.probe_revision,
                source_size: file.size,
                source_mtime: file.mtime,
                scanner_digest: scanner,
                stats,
            },
            m7: outcome,
            now_ms,
        })
        .await
        .expect("title facts");
    assert!(matches!(published, JobPublishOutcome::Published { .. }));
}

/// The title job a reopen queued for `file`, as the title worker finds it.
async fn queued_attempt(store: &Arc<dyn Store>, file: &MediaFile, now_ms: i64) -> String {
    let demand = store
        .dv_title_demands(&"a".repeat(64), 0, now_ms)
        .await
        .expect("title demand")
        .into_iter()
        .find(|d| d.file_id == file.id)
        .expect("reopened title demand");
    let id = demand.active_job_id.expect("a queued attempt");
    let job = store.background_job(&id).await.expect("job").expect("job");
    assert_eq!(job.state, JobState::Queued);
    id
}

async fn title_settled(store: &Arc<dyn Store>, file: i64) -> bool {
    store
        .dv_title_demands(&"a".repeat(64), 0, now() + 60_000)
        .await
        .expect("title demand")
        .into_iter()
        .all(|d| d.file_id != file)
}

/// Two M7 identities: a build/tools pair and its successor.
fn m7(id: char) -> String {
    id.to_string().repeat(64)
}

async fn sweep(store: &Arc<dyn Store>, identity: &str) -> Vec<i64> {
    store
        .reopen_stale_dv_m7_titles(identity, FIT_VERSION, 16, now())
        .await
        .expect("reopen")
}

fn outcome(identity: &str, kind: M7OutcomeKind, gate: Option<&str>) -> M7TitleOutcome {
    M7TitleOutcome {
        identity: identity.into(),
        fit_version: FIT_VERSION.into(),
        observed_fit_table: None,
        outcome: kind,
        gate: gate.map(str::to_owned),
    }
}

async fn record(store: &Arc<dyn Store>, file: i64) -> Option<(String, String, M7OutcomeKind)> {
    store
        .dv_m7_title_outcome(file)
        .await
        .expect("M7 outcome")
        .map(|row| (row.identity, row.fit_table, row.outcome))
}

async fn cancel_queued(store: &Arc<dyn Store>, file: &MediaFile, now_ms: i64) {
    let job_id = queued_attempt(store, file, now_ms).await;
    store
        .cancel_job(CancelJob { job_id, now_ms })
        .await
        .expect("cancel")
        .expect("cancelled");
}

/// Publish a fitted F+ table for `file` under `helper`; returns its digest.
async fn land_fit(store: &Arc<dyn Store>, file: &MediaFile, helper: &str, shift: i64) -> String {
    let found = demand(store, file.id, helper).await.expect("F+ demand");
    let job = attempt(store, &found, helper).await;
    let fitted = table_rows(file, &found, helper, 3, RefitVerdict::Fitted, shift);
    let digest = fitted.0.curve_digest.clone();
    assert!(matches!(
        publish(store, &job, fitted).await,
        JobPublishOutcome::Published { .. }
    ));
    digest
}

#[tokio::test]
async fn an_unrecorded_settled_title_reopens_once_per_m7_identity() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "m7-reopen", true).await;
        let (a, b) = (m7('a'), m7('b'));
        assert!(title_settled(&store, file.id).await, "{backend}: settled");
        assert_eq!(record(&store, file.id).await, None, "{backend}: no record");
        for (identity, fit, limit) in [
            (a.as_str(), FIT_VERSION, 0),
            (a.as_str(), "not a fit version", 16),
            ("not-an-identity", FIT_VERSION, 16),
        ] {
            assert!(store
                .reopen_stale_dv_m7_titles(identity, fit, limit, now())
                .await
                .is_err());
        }
        // No record: the title reopens at its own generation, with its stored
        // aggregate, and the reopen is marked as this identity's.
        let (first, second) = tokio::join!(sweep(&store, &a), sweep(&store, &a));
        assert_eq!(
            first.into_iter().chain(second).collect::<Vec<_>>(),
            vec![file.id],
            "{backend}: concurrent sweeps admit one job"
        );
        let reopened = store
            .dv_title_demands(&"a".repeat(64), 0, now())
            .await
            .expect("title demand")
            .into_iter()
            .find(|d| d.file_id == file.id)
            .expect("reopened");
        assert!(DvTitleStats::from_probe_json(&reopened.probe_json).is_some());
        assert_eq!(
            record(&store, file.id).await,
            Some((a.clone(), String::new(), M7OutcomeKind::Pending)),
            "{backend}: loop-breaker"
        );
        assert!(
            sweep(&store, &a).await.is_empty(),
            "{backend}: already open"
        );
        // A cancelled reopen settles the title; the loop-breaker keeps the
        // same identity from reopening it again, but not the next identity.
        cancel_queued(&store, &file, now()).await;
        assert!(title_settled(&store, file.id).await, "{backend}: cancelled");
        assert!(
            sweep(&store, &a).await.is_empty(),
            "{backend}: once per identity"
        );
        assert_eq!(
            sweep(&store, &b).await,
            vec![file.id],
            "{backend}: new identity"
        );
        // A job that settles without recording (M7 skipped) leaves the mark.
        resettle_title(&store, &file, now(), None).await;
        assert!(
            title_settled(&store, file.id).await,
            "{backend}: settled again"
        );
        assert_eq!(
            record(&store, file.id).await,
            Some((b.clone(), String::new(), M7OutcomeKind::Pending))
        );
        assert!(
            sweep(&store, &b).await.is_empty(),
            "{backend}: loop-breaker holds"
        );
    })
    .await;
}

#[tokio::test]
async fn an_unsupported_title_reopens_when_its_identity_or_fitted_table_changes() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "m7-unsupported", true).await;
        let (a, b) = (m7('a'), m7('b'));
        assert_eq!(sweep(&store, &a).await, vec![file.id], "{backend}");
        resettle_title(
            &store,
            &file,
            now(),
            Some(outcome(
                &a,
                M7OutcomeKind::Unsupported,
                Some("no fitted scenes"),
            )),
        )
        .await;
        assert_eq!(
            record(&store, file.id).await,
            Some((a.clone(), String::new(), M7OutcomeKind::Unsupported)),
            "{backend}: recorded with the publication"
        );
        assert!(sweep(&store, &a).await.is_empty(), "{backend}: unchanged");
        // A table that fits nothing is not a fitted table.
        let found = demand(&store, file.id, &helper()).await.expect("demand");
        let job = attempt(&store, &found, &helper()).await;
        let identity_table = table_rows(&file, &found, &helper(), 2, RefitVerdict::Identity, 0);
        assert!(matches!(
            publish(&store, &job, identity_table).await,
            JobPublishOutcome::Published { .. }
        ));
        assert!(
            sweep(&store, &a).await.is_empty(),
            "{backend}: identity table"
        );
        // A later helper's fitted table reopens the title once.
        let fitted = land_fit(&store, &file, &"e".repeat(64), 1).await;
        assert_eq!(sweep(&store, &a).await, vec![file.id], "{backend}: new fit");
        assert_eq!(
            record(&store, file.id).await,
            Some((a.clone(), fitted.clone(), M7OutcomeKind::Pending))
        );
        resettle_title(
            &store,
            &file,
            now(),
            Some(outcome(&a, M7OutcomeKind::Unsupported, Some("gate"))),
        )
        .await;
        assert_eq!(
            record(&store, file.id).await,
            Some((a.clone(), fitted, M7OutcomeKind::Unsupported)),
            "{backend}: the publication names the table it saw"
        );
        assert!(
            sweep(&store, &a).await.is_empty(),
            "{backend}: both unchanged"
        );
        // A new M7 build or tool set reopens it again.
        assert_eq!(
            sweep(&store, &b).await,
            vec![file.id],
            "{backend}: identity"
        );
    })
    .await;
}

#[tokio::test]
async fn results_and_terminal_outcomes_are_never_swept_until_an_administrator_clears_them() {
    for_each_backend(|store, backend| async move {
        let (a, b) = (m7('a'), m7('b'));
        let mut files = Vec::new();
        for (name, kind, gate) in [
            ("m7-result", M7OutcomeKind::Result, None),
            ("m7-not-applicable", M7OutcomeKind::NotApplicable, None),
            (
                "m7-inconclusive",
                M7OutcomeKind::Inconclusive,
                Some("rate out of band"),
            ),
        ] {
            let file = title(&store, name, true).await;
            assert!(
                sweep(&store, &a).await.contains(&file.id),
                "{backend}: {name}"
            );
            resettle_title(&store, &file, now(), Some(outcome(&a, kind, gate))).await;
            assert_eq!(
                record(&store, file.id).await.map(|r| r.2),
                Some(kind),
                "{backend}: {name}"
            );
            files.push(file);
        }
        assert!(
            sweep(&store, &b).await.is_empty(),
            "{backend}: new identity"
        );
        for file in &files {
            land_fit(&store, file, &helper(), 0).await;
        }
        assert!(sweep(&store, &b).await.is_empty(), "{backend}: new fit");
        // The administrator's re-evaluation forgets the record, so the next
        // sweep reopens the title under the running identity.
        let inconclusive = &files[2];
        assert!(store.clear_dv_m7_title_outcomes(0).await.is_err());
        assert_eq!(
            store
                .clear_dv_m7_title_outcomes(inconclusive.item_id)
                .await
                .expect("clear"),
            1,
            "{backend}"
        );
        assert_eq!(record(&store, inconclusive.id).await, None);
        assert_eq!(
            store
                .clear_dv_m7_title_outcomes(inconclusive.item_id)
                .await
                .expect("clear again"),
            0,
            "{backend}: nothing left"
        );
        assert_eq!(sweep(&store, &b).await, vec![inconclusive.id], "{backend}");
    })
    .await;
}

#[tokio::test]
async fn an_m7_record_is_published_with_its_title_and_dies_with_its_generation() {
    for_each_backend(|store, backend| async move {
        let file = title(&store, "m7-generation", true).await;
        let fit = land_fit(&store, &file, &helper(), 0).await;
        let a = m7('a');
        assert_eq!(sweep(&store, &a).await, vec![file.id], "{backend}");
        // A publication that lost its claim stores neither the aggregate nor
        // the outcome.
        let scanner = "a".repeat(64);
        let found = store
            .dv_title_demands(&scanner, 0, now())
            .await
            .expect("title demand")
            .into_iter()
            .find(|d| d.file_id == file.id)
            .expect("reopened");
        let job_id = queued_attempt(&store, &file, now()).await;
        let job = claim(&store, &job_id, JobKind::DvTitleScan).await;
        let mut token = job.token.clone().expect("token");
        token.claim_id = uuid::Uuid::new_v4().to_string();
        let stats = DvTitleStats::from_probe_json(&found.probe_json).expect("aggregate");
        let output = TitleOutput {
            file_id: file.id,
            probe_revision: found.probe_revision,
            source_size: file.size,
            source_mtime: file.mtime,
            scanner_digest: scanner.clone(),
            stats,
        };
        let mut result = outcome(&a, M7OutcomeKind::Result, None);
        result.observed_fit_table = Some(fit.clone());
        let fenced = store
            .publish_dv_title_job(PublishTitleJob {
                token,
                output: output.clone(),
                m7: Some(result.clone()),
                now_ms: now(),
            })
            .await
            .expect("fenced publication");
        assert!(!matches!(fenced, JobPublishOutcome::Published { .. }));
        assert_eq!(
            record(&store, file.id).await.map(|r| r.2),
            Some(M7OutcomeKind::Pending),
            "{backend}: a fenced publication records nothing"
        );
        // An invalid outcome refuses the whole publication.
        assert!(store
            .publish_dv_title_job(PublishTitleJob {
                token: job.token.clone().expect("token"),
                output: output.clone(),
                m7: Some(outcome(&a, M7OutcomeKind::Unsupported, None)),
                now_ms: now(),
            })
            .await
            .is_err());
        let mut stale = outcome(&a, M7OutcomeKind::Unsupported, Some("no fitted scenes"));
        stale.observed_fit_table = Some(String::new());
        let changed_fit = store
            .publish_dv_title_job(PublishTitleJob {
                token: job.token.clone().expect("token"),
                output: output.clone(),
                m7: Some(stale),
                now_ms: now(),
            })
            .await
            .expect("fit changed verdict");
        assert!(matches!(changed_fit, JobPublishOutcome::ArtifactConflict));
        assert_eq!(
            record(&store, file.id).await.map(|r| r.2),
            Some(M7OutcomeKind::Pending)
        );
        let request = PublishTitleJob {
            token: job.token.clone().expect("token"),
            output,
            m7: Some(result),
            now_ms: now(),
        };
        let published = store
            .publish_dv_title_job(request.clone())
            .await
            .expect("publication");
        assert!(matches!(published, JobPublishOutcome::Published { .. }));
        let repeat = store
            .publish_dv_title_job(request)
            .await
            .expect("repeat publication");
        assert!(matches!(repeat, JobPublishOutcome::AlreadyPublished { .. }));
        let settled = store
            .background_job(&job_id)
            .await
            .expect("job")
            .expect("job");
        assert_eq!(settled.state, JobState::Succeeded, "{backend}");
        assert_eq!(
            record(&store, file.id).await,
            Some((a.clone(), fit, M7OutcomeKind::Result)),
            "{backend}: recorded with the aggregate"
        );
        // A reprobe replaces the generation; its record goes with it, so the
        // new generation is a candidate once it settles.
        let current = store.get_file(file.id).await.expect("file").expect("file");
        store
            .upsert_file(
                current.item_id,
                current.path.to_str().expect("path"),
                current.size,
                current.mtime,
                &probe(7, r#"{"streams":[{"index":0,"codec_type":"video"}]}"#),
            )
            .await
            .expect("reprobe");
        assert_eq!(record(&store, file.id).await, None, "{backend}: reprobe");
        // A source change does the same.
        let again = title(&store, "m7-generation-source", true).await;
        assert_eq!(sweep(&store, &a).await, vec![again.id], "{backend}");
        assert!(record(&store, again.id).await.is_some());
        store
            .upsert_file(
                again.item_id,
                again.path.to_str().expect("path"),
                again.size + 1,
                again.mtime,
                &probe(7, r#"{"streams":[{"index":0,"codec_type":"video"}]}"#),
            )
            .await
            .expect("source change");
        assert_eq!(
            record(&store, again.id).await,
            None,
            "{backend}: source change"
        );
    })
    .await;
}

#[tokio::test]
async fn legacy_m7_rate_stop_can_archive_and_replace_the_fenced_source_ledger_once() {
    use plurx_core::{
        store::dv_m7_attempts::{
            M7CandidateAnchor, M7CandidateAuthority, M7CandidateIntent, M7CandidatePlan,
            M7CandidateReservation, M7CandidateScope,
        },
        transcode::dv_m7_receipt::M7SourceProbe,
    };
    for_each_backend(|store, backend| async move {
        let file = title(&store, "m7-rate-supersession", true).await;
        let fit = land_fit(&store, &file, &helper(), 0).await;
        let mut table = [0; 32];
        hex::decode_to_slice(&fit, &mut table).expect("fit digest");
        let original = m7('a');
        let successor = m7('b');
        let scanner = "a".repeat(64);
        let source = [31; 32];
        let legacy_plan = M7CandidatePlan {
            protocol_sha256: [32; 32],
            recipe_sha256: [33; 32],
            maximum_attempts: 4,
        };
        let new_plan = M7CandidatePlan {
            protocol_sha256: [34; 32],
            recipe_sha256: [35; 32],
            ..legacy_plan
        };
        let attempt = M7CandidateIntent {
            source_sha256: source,
            protocol_sha256: legacy_plan.protocol_sha256,
            recipe_sha256: legacy_plan.recipe_sha256,
            binding_sha256: [36; 32],
            slot: 0,
            target_kbps: 85_068,
            maximum_attempts: 4,
        };
        let authority = |demand: &TitleDemand, token: JobToken, at| M7CandidateAuthority {
            token,
            source_probe: M7SourceProbe::from_probe_json(&demand.probe_json).expect("probe"),
            scope: M7CandidateScope {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                probe_revision: demand.probe_revision,
                scanner_digest: scanner.clone(),
                source_sha256: source,
            },
            now_ms: at,
        };

        assert_eq!(sweep(&store, &original).await, vec![file.id], "{backend}");
        let old_demand = store
            .dv_title_demands(&scanner, 0, now())
            .await
            .expect("title demands")
            .into_iter()
            .find(|demand| demand.file_id == file.id)
            .expect("title");
        let job = queued_attempt(&store, &file, now()).await;
        let token = claim(&store, &job, JobKind::DvTitleScan)
            .await
            .token
            .expect("title claim");
        let owner = authority(&old_demand, token.clone(), now());
        assert_eq!(
            store
                .anchor_dv_m7_candidate(&owner, legacy_plan)
                .await
                .expect("old anchor"),
            M7CandidateAnchor::Ready,
            "{backend}"
        );
        assert_eq!(
            store
                .reserve_dv_m7_candidate_slot(&owner, attempt.clone())
                .await
                .expect("old slot"),
            M7CandidateReservation::Reserved,
            "{backend}"
        );
        let old_gate = "NP1 did not land a rate-only candidate: OutsideTargetBounds(77504)";
        let published = store
            .publish_dv_title_job(PublishTitleJob {
                token,
                output: TitleOutput {
                    file_id: file.id,
                    probe_revision: old_demand.probe_revision,
                    source_size: file.size,
                    source_mtime: file.mtime,
                    scanner_digest: scanner.clone(),
                    stats: DvTitleStats::from_probe_json(&old_demand.probe_json)
                        .expect("title stats"),
                },
                m7: Some(outcome(
                    &original,
                    M7OutcomeKind::Inconclusive,
                    Some(old_gate),
                )),
                now_ms: now(),
            })
            .await
            .expect("legacy stop");
        assert!(matches!(published, JobPublishOutcome::Published { .. }));
        assert_eq!(
            store
                .clear_dv_m7_title_outcomes(file.item_id)
                .await
                .expect("reevaluation"),
            0,
            "{backend}: the original rate stop must remain available to archive"
        );
        assert!(sweep(&store, &original).await.is_empty());
        assert_eq!(sweep(&store, &successor).await, vec![file.id], "{backend}");
        assert!(sweep(&store, &successor).await.is_empty());
        assert_eq!(
            store
                .clear_dv_m7_title_outcomes(file.item_id)
                .await
                .expect("reevaluation"),
            0,
            "{backend}: an open successor needs its pending outcome"
        );

        let renewed_demand = store
            .dv_title_demands(&scanner, 0, now())
            .await
            .expect("title demands")
            .into_iter()
            .find(|demand| demand.file_id == file.id)
            .expect("title");
        assert_eq!(renewed_demand.probe_revision, old_demand.probe_revision);
        let job = queued_attempt(&store, &file, now()).await;
        let token = claim(&store, &job, JobKind::DvTitleScan)
            .await
            .token
            .expect("renewed claim");
        let current = authority(&renewed_demand, token, now());
        let mut legacy = store
            .legacy_m7_rate_stop(&current, table)
            .await
            .expect("lookup")
            .expect("legacy stop");
        assert_eq!(legacy.gate, old_gate);
        assert_eq!(legacy.refused_target(), Some(77_504));
        assert_eq!(legacy.ledger.slots, vec![attempt.clone()]);
        assert_eq!(
            store
                .supersede_m7_rate_stop(&owner, &legacy, table, new_plan)
                .await
                .expect("fence"),
            M7CandidateAnchor::Fenced,
            "{backend}"
        );
        legacy.ledger.slots[0].target_kbps += 1;
        assert_eq!(
            store
                .supersede_m7_rate_stop(&current, &legacy, table, new_plan)
                .await
                .expect("CAS"),
            M7CandidateAnchor::DifferentProtocol,
            "{backend}"
        );
        legacy.ledger.slots[0] = attempt.clone();
        assert_eq!(
            store
                .supersede_m7_rate_stop(&current, &legacy, table, new_plan)
                .await
                .expect("supersede"),
            M7CandidateAnchor::Ready,
            "{backend}"
        );
        let archived = store
            .legacy_m7_rate_stop(&current, table)
            .await
            .expect("archived lookup")
            .expect("archive");
        assert_eq!(archived.gate, old_gate);
        assert_eq!(archived.ledger.slots, vec![attempt.clone()]);
        let current_ledger = store
            .dv_m7_candidate_slots(&current)
            .await
            .expect("live lookup")
            .expect("new ledger");
        assert_eq!(current_ledger.protocol_sha256, new_plan.protocol_sha256);
        assert!(current_ledger.slots.is_empty());
        assert_eq!(
            store
                .reserve_dv_m7_candidate_slot(&current, attempt)
                .await
                .expect("old slot fence"),
            M7CandidateReservation::Conflict,
            "{backend}"
        );
        assert_eq!(
            store
                .supersede_m7_rate_stop(&current, &legacy, table, new_plan)
                .await
                .expect("retry"),
            M7CandidateAnchor::Ready,
            "{backend}"
        );
        let third = M7CandidatePlan {
            protocol_sha256: [37; 32],
            ..new_plan
        };
        assert_eq!(
            store
                .supersede_m7_rate_stop(&current, &archived, table, third)
                .await
                .expect("once"),
            M7CandidateAnchor::DifferentProtocol,
            "{backend}"
        );
    })
    .await;
}
