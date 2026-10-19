//! The same source/attempt-fenced title lifecycle on every store backend.
use super::{for_each_backend, seed_file};
use plurx_core::{
    domain::{DolbyVisionFacts, MediaFile, ProbeResult},
    scan::dv_title::{DvTitleStats, TitleConstant, PROBE_FIELD},
    store::{background_jobs::*, dv_title_jobs::*, Store},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn probe() -> ProbeResult {
    ProbeResult {
        video_codec: Some("hevc".into()),
        dolby_vision: DolbyVisionFacts {
            profile: Some(7),
            rpu_present: Some(true),
            ..Default::default()
        },
        raw_json: Some(r#"{"streams":[{"index":0,"codec_type":"video"}]}"#.into()),
        ..Default::default()
    }
}
fn stats() -> DvTitleStats {
    DvTitleStats {
        schema: 1,
        complete: true,
        decoded_frames: 2,
        valid_l1_frames: 2,
        missing_l1_frames: 0,
        invalid_l1_frames: 0,
        max_l1_pq: Some(3900),
        mastering_luminance: TitleConstant::Unknown,
        mastering_primaries: TitleConstant::Unknown,
    }
}
async fn reprobe(store: &Arc<dyn Store>, file: &MediaFile) {
    store
        .upsert_file(
            file.item_id,
            file.path.to_str().expect("path"),
            file.size,
            file.mtime,
            &probe(),
        )
        .await
        .expect("fresh ordinary probe");
}
async fn seed(store: &Arc<dyn Store>, name: &str) -> MediaFile {
    let (_, id) = seed_file(store, name).await;
    let file = store.get_file(id).await.expect("file").expect("file");
    reprobe(store, &file).await;
    store.get_file(id).await.expect("DV file").expect("DV file")
}
async fn demand(store: &Arc<dyn Store>, file: i64, scanner: &str) -> TitleDemand {
    store
        .dv_title_demands(scanner, 0)
        .await
        .expect("demand")
        .into_iter()
        .find(|d| d.file_id == file)
        .expect("current source demands a title scan")
}
async fn bound(store: &Arc<dyn Store>, file: i64, scanner: &str) -> TitleDemand {
    let current = demand(store, file, scanner).await;
    store
        .bind_dv_title_demand(current.probe_revision, scanner)
        .await
        .expect("bind installed scanner");
    demand(store, file, scanner).await
}
async fn enqueue(
    store: &Arc<dyn Store>,
    demand: &TitleDemand,
    scanner: &str,
    now: i64,
) -> EnqueueOutcome {
    let identity = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            "dv-title-v1",
            demand.file_id,
            demand.probe_revision,
            scanner,
        ))
        .expect("request identity"),
    ));
    store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::DvTitleScan {
                file_id: demand.file_id,
                source_size: demand.source_size,
                source_mtime: demand.source_mtime,
                probe_revision: demand.probe_revision,
                scanner_digest: scanner.into(),
            },
            dedupe_key: format!("dv-title:{identity}"),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "dv-title".into(),
                request_id: identity.clone(),
                request_digest: identity,
                consumer_kind: "dv_title_scan".into(),
                consumer_ref: demand.file_id.to_string(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("enqueue title")
}
async fn claim(store: &Arc<dyn Store>, id: &str, now: i64) -> BackgroundJob {
    let candidate = store.background_job(id).await.expect("job").expect("job");
    let outcome = store
        .claim_job(ClaimJob {
            job_id: id.into(),
            expected_revision: candidate.revision,
            node_id: "title-node".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::DvTitleScan,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim title");
    match outcome {
        ClaimOutcome::Claimed { job } => *job,
        other => panic!("expected title claim: {other:?}"),
    }
}
async fn active(
    store: &Arc<dyn Store>,
    file: i64,
    scanner: &str,
    now: i64,
) -> (TitleDemand, BackgroundJob) {
    let demand = bound(store, file, scanner).await;
    let EnqueueOutcome::Accepted { job_id, .. } = enqueue(store, &demand, scanner, now).await
    else {
        panic!("new title demand must enqueue");
    };
    let job = claim(store, &job_id, now).await;
    (demand, job)
}
fn publication(
    demand: &TitleDemand,
    job: &BackgroundJob,
    scanner: &str,
    now: i64,
) -> PublishTitleJob {
    PublishTitleJob {
        token: job.token.clone().expect("live attempt"),
        output: TitleOutput {
            file_id: demand.file_id,
            probe_revision: demand.probe_revision,
            source_size: demand.source_size,
            source_mtime: demand.source_mtime,
            scanner_digest: scanner.into(),
            stats: stats(),
        },
        now_ms: now,
    }
}

#[tokio::test]
async fn title_publication_is_atomic_idempotent_and_never_self_enqueues() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "title-publish").await;
        let (demand, job) = active(&store, file.id, &scanner, 1_000).await;
        let publish = publication(&demand, &job, &scanner, 1_001);
        assert!(
            matches!(
                store
                    .publish_dv_title_job(publish.clone())
                    .await
                    .expect("publish"),
                JobPublishOutcome::Published { .. }
            ),
            "{backend}"
        );
        assert!(
            matches!(
                store
                    .publish_dv_title_job(publish)
                    .await
                    .expect("idempotent replay"),
                JobPublishOutcome::AlreadyPublished { .. }
            ),
            "{backend}"
        );
        let stored = store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe");
        let document: serde_json::Value = serde_json::from_str(&stored).expect("JSON");
        assert_eq!(
            document[PROBE_FIELD],
            serde_json::to_value(stats()).expect("stats"),
            "{backend}"
        );
        assert!(
            store
                .dv_title_demands(&scanner, 0)
                .await
                .expect("settled demand")
                .is_empty(),
            "{backend}: attach must not create another generation"
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
        assert_eq!(
            store
                .prometheus_store_snapshot("title-node", 2)
                .await
                .expect("resources")
                .background_jobs
                .source_io_reservations,
            0
        );
        reprobe(&store, &file).await;
        assert!(demand.probe_revision < demand_for(&store, file.id, &scanner).await);
    })
    .await;
}
async fn demand_for(store: &Arc<dyn Store>, file: i64, scanner: &str) -> i64 {
    demand(store, file, scanner).await.probe_revision
}

#[tokio::test]
async fn title_manual_cancel_wins_publication_and_does_not_become_scheduler_yield() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "title-cancel").await;
        let (demand, job) = active(&store, file.id, &scanner, 1_000).await;
        store
            .cancel_job(CancelJob {
                job_id: job.id.clone(),
                now_ms: 1_001,
            })
            .await
            .expect("cancel by ID");
        assert!(
            matches!(
                store
                    .publish_dv_title_job(publication(&demand, &job, &scanner, 1_002))
                    .await
                    .expect("cancel race"),
                JobPublishOutcome::LostOwnership
            ),
            "{backend}"
        );
        assert!(
            !store
                .settle_job(SettleJob {
                    token: job.token.expect("old token"),
                    settlement: JobSettlement::Yield {
                        error_code: Some("title_interrupted".into()),
                        not_before_ms: 1_003,
                        checkpoint: None
                    },
                    now_ms: 1_002,
                })
                .await
                .expect("stale yield rejected"),
            "{backend}"
        );
        assert!(
            store
                .dv_title_demands(&scanner, 0)
                .await
                .expect("cancel suppression")
                .is_empty(),
            "{backend}"
        );
        assert_eq!(
            store
                .bind_dv_title_demand(demand.probe_revision, &scanner)
                .await
                .expect("idempotent scanner binding"),
            Some(demand.probe_revision),
        );
        assert!(store
            .dv_title_demands(&scanner, 0)
            .await
            .expect("cancel remains suppressed")
            .is_empty());
        let stored = store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe");
        assert!(!stored.contains(PROBE_FIELD), "{backend}");
        reprobe(&store, &file).await;
        assert!(
            demand.probe_revision < demand_for(&store, file.id, &scanner).await,
            "{backend}: explicit reprobe changes generation"
        );
    })
    .await;
}

#[tokio::test]
async fn title_scheduler_yield_releases_resources_and_retains_exact_demand() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "title-yield").await;
        let (demand, job) = active(&store, file.id, &scanner, 1_000).await;
        assert_eq!(
            store
                .prometheus_store_snapshot("title-node", 1)
                .await
                .expect("held resources")
                .background_jobs
                .source_io_reservations,
            1
        );
        assert!(
            store
                .settle_job(SettleJob {
                    token: job.token.clone().expect("token"),
                    settlement: JobSettlement::Yield {
                        error_code: Some("title_interrupted".into()),
                        not_before_ms: 2_000,
                        checkpoint: None
                    },
                    now_ms: 1_001,
                })
                .await
                .expect("yield"),
            "{backend}"
        );
        assert_eq!(
            store
                .prometheus_store_snapshot("title-node", 2)
                .await
                .expect("released resources")
                .background_jobs
                .source_io_reservations,
            0
        );
        assert_eq!(
            demand.probe_revision,
            demand_for(&store, file.id, &scanner).await,
            "{backend}"
        );
        let resumed = claim(&store, &job.id, 2_000).await;
        assert!(resumed.fence > job.fence, "{backend}");
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&demand, &job, &scanner, 2_001))
                .await
                .expect("stale preemption result"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&demand, &resumed, &scanner, 2_001))
                .await
                .expect("resumed publication"),
            JobPublishOutcome::Published { .. }
        ));
    })
    .await;
}

#[tokio::test]
async fn title_reprobe_and_scanner_aba_never_reuse_a_request_or_attach_old_results() {
    for_each_backend(|store, backend| async move {
        let scanner_a = "a".repeat(64);
        let scanner_b = "b".repeat(64);
        let file = seed(&store, "title-aba").await;
        let (first, first_job) = active(&store, file.id, &scanner_a, 1_000).await;
        let second = bound(&store, file.id, &scanner_b).await;
        let third = bound(&store, file.id, &scanner_a).await;
        assert!(
            first.probe_revision < second.probe_revision
                && second.probe_revision < third.probe_revision,
            "{backend}: scanner A->B->A needs monotonic generations"
        );
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&first, &first_job, &scanner_a, 1_001))
                .await
                .expect("scanner supersession"),
            JobPublishOutcome::LostOwnership
        ));
        let EnqueueOutcome::Accepted { job_id, .. } =
            enqueue(&store, &third, &scanner_a, 40_000).await
        else {
            panic!("{backend}: reused stale scanner-A request");
        };
        let current = claim(&store, &job_id, 40_000).await;
        reprobe(&store, &file).await;
        assert!(
            third.probe_revision < demand_for(&store, file.id, &scanner_a).await,
            "{backend}: identical facts are still a new ordinary probe"
        );
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&third, &current, &scanner_a, 40_001))
                .await
                .expect("reprobe supersession"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(!store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe")
            .contains(PROBE_FIELD));
        store.delete_files(&[file.id]).await.expect("delete source");
        let replacement = store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("path"),
                file.size,
                file.mtime,
                &probe(),
            )
            .await
            .expect("reinsert same source snapshot");
        assert!(
            third.probe_revision < demand_for(&store, replacement, &scanner_a).await,
            "{backend}: delete/reinsert must not reuse demand identity"
        );
    })
    .await;
}

#[tokio::test]
async fn title_demand_and_manual_cancel_suppression_survive_store_reopen() {
    let dir = tempfile::tempdir().expect("durable store fixture");
    let path = dir.path().join("titles.sqlite");
    let scanner = "a".repeat(64);
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open(&path).expect("open"));
    let file = seed(&store, "title-restart").await;
    let first = bound(&store, file.id, &scanner).await;
    drop(store);
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open(&path).expect("reopen pending"));
    assert_eq!(
        first.probe_revision,
        demand_for(&store, file.id, &scanner).await
    );
    let (current, job) = active(&store, file.id, &scanner, 1_000).await;
    store
        .cancel_job(CancelJob {
            job_id: job.id,
            now_ms: 1_001,
        })
        .await
        .expect("cancel");
    drop(store);
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open(&path).expect("reopen cancelled"));
    assert!(store
        .dv_title_demands(&scanner, 0)
        .await
        .expect("suppressed")
        .is_empty());
    assert_eq!(
        store
            .bind_dv_title_demand(current.probe_revision, &scanner)
            .await
            .expect("same scanner"),
        Some(current.probe_revision)
    );
    assert!(store
        .dv_title_demands(&scanner, 0)
        .await
        .expect("still suppressed")
        .is_empty());
}

#[tokio::test]
async fn title_priority_observation_requires_live_higher_priority_same_node_work() {
    use plurx_core::store::background_jobs_probe::{generation, ProbeCoordinator};
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "title-priority").await;
        let (_, title) = active(&store, file.id, &scanner, 1_000).await;
        let plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) = store
            .acquire_lease("repair:probe", "coordinator", 1_000, 60_000)
            .await
            .expect("lease")
        else {
            panic!("coordinator");
        };
        let coordinator = ProbeCoordinator::from_lease(&lease);
        let mut ids = Vec::new();
        for (priority, target) in [(1, None), (0, None), (1, Some("other-node"))] {
            let id = uuid::Uuid::new_v4().to_string();
            assert!(matches!(
                store
                    .enqueue_job(EnqueueJob {
                        id: id.clone(),
                        payload: JobPayload::MediaProbe {
                            file_id: file.id,
                            source_generation: generation(
                                file.id,
                                file.size,
                                file.mtime,
                                &coordinator,
                                &scanner
                            ),
                            source_size: file.size,
                            source_mtime: file.mtime,
                            probe_digest: scanner.clone(),
                            coordinator: coordinator.clone(),
                        },
                        dedupe_key: format!("title-priority-probe:{id}"),
                        priority,
                        not_before_ms: 1_000,
                        now_ms: 1_000,
                        request: JobRequest {
                            scope: "title-priority".into(),
                            request_id: id.clone(),
                            request_digest: "b".repeat(64),
                            consumer_kind: "probe".into(),
                            consumer_ref: id.clone(),
                            target_node_id: target.map(str::to_owned),
                            deadline_ms: None,
                            retain_identity: false,
                        },
                    })
                    .await
                    .expect("enqueue ordinary probe"),
                EnqueueOutcome::Accepted { .. }
            ));
            ids.push(id);
        }
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                store
                    .probe_can_preempt_title(id, &title.id, "title-node", 1_001)
                    .await
                    .expect("priority observation"),
                index == 0,
                "{backend}: candidate {index}"
            );
        }
        let ClaimOutcome::Claimed { job: blocker } = store
            .claim_job(ClaimJob {
                job_id: ids[1].clone(),
                expected_revision: 0,
                node_id: "title-node".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MediaProbe,
                payload_version: 1,
                now_ms: 1_002,
                dispatched_at_ms: 1_002,
            })
            .await
            .expect("other resource reader")
        else {
            panic!("{backend}: blocker claim");
        };
        assert!(
            !store
                .probe_can_preempt_title(&ids[0], &title.id, "title-node", 1_003)
                .await
                .expect("blocked probe cannot preempt"),
            "{backend}"
        );
        assert!(store
            .settle_job(SettleJob {
                token: blocker.token.expect("blocker token"),
                now_ms: 1_004,
                settlement: JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: 5_000
                },
            })
            .await
            .expect("release blocker"));
        assert!(
            store
                .probe_can_preempt_title(&ids[0], &title.id, "title-node", 1_005)
                .await
                .expect("runnable again"),
            "{backend}"
        );
        store
            .release_lease(&lease, 1_006)
            .await
            .expect("coordinator offline");
        assert!(
            !store
                .probe_can_preempt_title(&ids[0], &title.id, "title-node", 1_007)
                .await
                .expect("offline probe cannot preempt"),
            "{backend}"
        );
        assert!(
            !store
                .probe_can_preempt_title(&ids[0], &title.id, "other-node", 1_007)
                .await
                .expect("other executor"),
            "{backend}"
        );
    })
    .await;
}
