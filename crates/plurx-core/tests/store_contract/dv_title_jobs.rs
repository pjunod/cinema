//! The same source/attempt-fenced title lifecycle on every store backend.
#[path = "dv_title_probe_provenance.rs"]
mod probe_provenance;
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
        .dv_title_demands(scanner, 0, 1_000)
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
                .dv_title_demands(&scanner, 0, 1_000)
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
                .dv_title_demands(&scanner, 0, 1_000)
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
            .dv_title_demands(&scanner, 0, 1_000)
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
        .dv_title_demands(&scanner, 0, 1_000)
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
        .dv_title_demands(&scanner, 0, 1_000)
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
                    .probe_can_preempt_title(
                        id,
                        &title.id,
                        if index == 2 {
                            "other-node"
                        } else {
                            "title-node"
                        },
                        1_001,
                    )
                    .await
                    .expect("priority observation"),
                index == 0,
                "{backend}: candidate {index}"
            );
        }
        // MediaProbe is unassigned execution: a waiter's target is its
        // consumer, not a job executor constraint. The title's actual owner
        // may still execute that same readable source for a remote consumer.
        assert!(store
            .probe_can_preempt_title(&ids[2], &title.id, "title-node", 1_001)
            .await
            .expect("remote consumer does not restrict unassigned execution"));
        // The title owns the single background-reader slot. Another ordinary
        // probe cannot manufacture a concurrent blocker by claiming that slot.
        let ordinary_claim = store
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
            .expect("ordinary background reader claim");
        assert!(
            matches!(ordinary_claim, ClaimOutcome::Contended),
            "{backend}: title already owns the background-reader slot: {ordinary_claim:?}"
        );
        // Attest actual live fragment demand through the production viewer
        // bridge. It may use the reserved second reader slot alongside title
        // work, and must conservatively prevent preemption for another probe.
        let source = "c".repeat(64);
        let key = plurx_core::store::cluster_fragment_index_key(
            file.id, file.size, file.mtime, &source, &scanner,
        )
        .expect("live fragment key");
        let outcome = store
            .enqueue_fragment_job(
                plurx_core::store::background_jobs_fragment_admission::EnqueueFragmentJob {
                    job: plurx_core::store::NewClusterFragmentIndexJob {
                        cache_key: key.clone(),
                        file_id: file.id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                        source_sha256: source,
                        pipeline_sha256: scanner,
                        priority: "foreground".into(),
                        trigger: "foreground".into(),
                        target_node_id: "title-node".into(),
                        not_before_ms: 1_002,
                        created_at_ms: 1_002,
                    },
                    analysis_request: None,
                    repair: false,
                    now_ms: 1_002,
                },
            )
            .await
            .expect("enqueue live fragment reader");
        let EnqueueOutcome::Accepted { job_id, .. } = outcome else {
            panic!("{backend}: live fragment admission: {outcome:?}");
        };
        let viewer = store
            .create_user("title-priority-viewer", "hash", false)
            .await
            .expect("live viewer");
        assert!(store
            .join_artifact_viewer(plurx_core::store::ArtifactViewerInterest {
                cache_key: key,
                file_id: file.id,
                target_node_id: "title-node".into(),
                principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                    user_id: viewer.id,
                },
                playback_id: "title-priority-playback".into(),
                now_ms: 1_002,
            })
            .await
            .expect("attest live fragment demand"));
        let candidate = store
            .background_job(&job_id)
            .await
            .expect("live fragment job")
            .expect("live fragment job");
        let outcome = store
            .claim_job(ClaimJob {
                job_id,
                expected_revision: candidate.revision,
                node_id: "title-node".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms: 1_002,
                dispatched_at_ms: 1_002,
            })
            .await
            .expect("live resource reader claim");
        let ClaimOutcome::Claimed { job: blocker } = outcome else {
            panic!("{backend}: live blocker claim: {outcome:?}");
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

#[tokio::test]
async fn a_fel_verdict_lives_with_the_title_facts_and_requalification_reruns_the_title() {
    use plurx_core::transcode::dv_fel_qualification::{self as q, FelVerdict};
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let (_, id) = seed_file(&store, "title-fel-verdict").await;
        let file = store.get_file(id).await.expect("file").expect("file");
        let mut fel = probe();
        fel.dolby_vision.el_present = Some(true);
        store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("path"),
                file.size,
                file.mtime,
                &fel,
            )
            .await
            .expect("FEL probe");
        let file = store.get_file(id).await.expect("file").expect("file");
        let (scanned, job) = active(&store, file.id, &scanner, 1_000).await;
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&scanned, &job, &scanner, 1_001))
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        let identity = q::identity(
            &scanner,
            plurx_core::transcode::RECONSTRUCTED_P81_RECIPE,
            &q::encoder_identity("ffmpeg", "Mesa (radeonsi)", "/dev/dri/renderD128"),
        );
        let verdict = FelVerdict {
            schema: q::SCHEMA,
            identity: identity.clone(),
            source_size: file.size,
            source_mtime: file.mtime,
            admitted: false,
            served_mean: 2.9,
            dropped_mean: 1.79,
            served_p99: 7.78,
            dropped_p99: 9.6,
            p99_tolerance: 0.0,
            measured_at_ms: 2_000,
            windows: Vec::new(),
        };
        let value = serde_json::to_value(&verdict).expect("verdict");
        // Another source generation never receives this verdict.
        assert!(
            !store
                .put_dv_fel_verdict(file.id, file.size + 1, file.mtime, &value)
                .await
                .expect("stale"),
            "{backend}"
        );
        assert!(
            store
                .put_dv_fel_verdict(file.id, file.size, file.mtime, &value)
                .await
                .expect("store"),
            "{backend}"
        );
        let stored = store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe");
        // Beside the title aggregate, which it leaves intact; read back as
        // this source's current verdict for this renderer and recipe.
        let document: serde_json::Value = serde_json::from_str(&stored).expect("JSON");
        assert_eq!(
            document[PROBE_FIELD],
            serde_json::to_value(stats()).expect("stats")
        );
        assert_eq!(
            q::verdict_state(Some(&stored), file.size, file.mtime, &identity),
            q::VerdictState::Current(verdict.clone()),
            "{backend}"
        );
        assert_eq!(
            store
                .dv_fel_verdict_counts(&identity)
                .await
                .expect("counts"),
            (1, 0)
        );
        assert_eq!(
            store
                .dv_fel_verdict_counts(&"b".repeat(64))
                .await
                .expect("counts"),
            (0, 0)
        );
        // A verdict measured under this identity is current; under any other
        // it is what a renderer change re-qualifies.
        assert!(
            store
                .dv_fel_stale_titles(&identity, 16)
                .await
                .expect("current")
                .is_empty(),
            "{backend}"
        );
        assert_eq!(
            store
                .dv_fel_stale_titles(&"b".repeat(64), 16)
                .await
                .expect("stale"),
            vec![file.id],
            "{backend}"
        );
        assert!(store.dv_fel_stale_titles(&identity, 0).await.is_err());
        // The settled title has no demand until a play asks for it again.
        assert!(store
            .dv_title_demands(&scanner, 0, 1_000)
            .await
            .expect("settled")
            .is_empty());
        assert!(
            store
                .request_dv_fel_requalification(file.id, &q::requalify_binding(&scanner, &identity))
                .await
                .expect("requalify"),
            "{backend}"
        );
        let rerun = demand(&store, file.id, &scanner).await;
        assert!(rerun.probe_revision > scanned.probe_revision, "{backend}");
        // While a title job for it is queued or running, a request is a
        // no-op: rotating the demand would cancel that attempt.
        let (attempt, attempt_job) = active(&store, file.id, &scanner, 3_000).await;
        assert!(
            !store
                .request_dv_fel_requalification(file.id, &q::requalify_binding(&scanner, &identity))
                .await
                .expect("requalify during a scan"),
            "{backend}"
        );
        assert_eq!(
            store
                .background_job(&attempt_job.id)
                .await
                .expect("job")
                .expect("job")
                .state,
            JobState::Running,
            "{backend}: the in-flight scan keeps running"
        );
        assert_eq!(
            demand(&store, file.id, &scanner).await.probe_revision,
            attempt.probe_revision
        );
        // A verdict is never written for a title without an enhancement layer.
        let plain = seed(&store, "title-fel-verdict-mel").await;
        assert!(
            !store
                .put_dv_fel_verdict(plain.id, plain.size, plain.mtime, &value)
                .await
                .expect("not FEL"),
            "{backend}"
        );
        assert!(store
            .put_dv_fel_verdict(file.id, file.size, file.mtime, &serde_json::json!([]))
            .await
            .is_err());
    })
    .await;
}

fn analysis(file: &MediaFile, scanner: &str) -> AnalysisTitleOutput {
    AnalysisTitleOutput {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        scanner_digest: scanner.into(),
        stats: stats(),
    }
}

async fn stored_aggregate(store: &Arc<dyn Store>, file: i64) -> Option<DvTitleStats> {
    let document = store
        .get_file_probe_json(file)
        .await
        .expect("probe")
        .expect("probe");
    DvTitleStats::from_probe_json(&document)
}

/// The content-analysis pass's aggregate lands on the title's current
/// demand only: bound to the pass's scanner like a job's, refused for a
/// source the catalogue no longer describes, dropped with its generation when
/// another scanner binds, and left open for the job to settle without a read.
#[tokio::test]
async fn analysis_title_aggregate_is_fenced_to_source_scanner_and_generation() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "title-analysis").await;
        let before = demand(&store, file.id, &scanner).await;

        let mut stale = analysis(&file, &scanner);
        stale.source_size += 1;
        assert_eq!(
            store
                .publish_dv_title_from_analysis(stale)
                .await
                .expect("stale"),
            AnalysisTitleOutcome::NoDemand,
            "{backend}"
        );
        assert_eq!(stored_aggregate(&store, file.id).await, None, "{backend}");

        // An unbound demand is bound to the pass's scanner, then receives it.
        assert_eq!(
            store
                .publish_dv_title_from_analysis(analysis(&file, &scanner))
                .await
                .expect("publish"),
            AnalysisTitleOutcome::Published,
            "{backend}"
        );
        assert_eq!(
            stored_aggregate(&store, file.id).await,
            Some(stats()),
            "{backend}"
        );
        // Still open, same generation: the title worker's own bind changes
        // nothing, which is what lets its job reuse the aggregate.
        let open = demand(&store, file.id, &scanner).await;
        assert_eq!(open.probe_revision, before.probe_revision, "{backend}");
        assert_eq!(
            DvTitleStats::from_probe_json(&open.probe_json),
            Some(stats()),
            "{backend}"
        );
        assert_eq!(
            store
                .bind_dv_title_demand(open.probe_revision, &scanner)
                .await
                .expect("rebind"),
            Some(open.probe_revision),
            "{backend}"
        );
        assert_eq!(
            stored_aggregate(&store, file.id).await,
            Some(stats()),
            "{backend}"
        );

        // Another scanner rotates the generation away with its aggregate.
        let other = "b".repeat(64);
        let rotated = store
            .bind_dv_title_demand(open.probe_revision, &other)
            .await
            .expect("rotate")
            .expect("rotated");
        assert_ne!(rotated, open.probe_revision, "{backend}");
        assert_eq!(stored_aggregate(&store, file.id).await, None, "{backend}");

        // That scanner's queued job keeps its generation: the pass does not
        // rotate it away, and the job publishes its own answer.
        let other_demand = demand(&store, file.id, &other).await;
        assert!(matches!(
            enqueue(&store, &other_demand, &other, 4_000).await,
            EnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(
            store
                .publish_dv_title_from_analysis(analysis(&file, &scanner))
                .await
                .expect("busy"),
            AnalysisTitleOutcome::Busy,
            "{backend}"
        );
        assert_eq!(
            demand(&store, file.id, &scanner).await.probe_revision,
            rotated,
            "{backend}: not rotated"
        );

        // A job's completed answer for this scanner is not replaced.
        let (job_demand, job) = active(&store, file.id, &scanner, 5_000).await;
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&job_demand, &job, &scanner, 5_001))
                .await
                .expect("job publication"),
            JobPublishOutcome::Published { .. }
        ));
        assert_eq!(
            store
                .publish_dv_title_from_analysis(analysis(&file, &scanner))
                .await
                .expect("settled"),
            AnalysisTitleOutcome::Current,
            "{backend}"
        );
        assert!(store
            .publish_dv_title_from_analysis(AnalysisTitleOutput {
                scanner_digest: "not a digest".into(),
                ..analysis(&file, &scanner)
            })
            .await
            .is_err());
    })
    .await;
}

#[tokio::test]
async fn fel_recipe_variants_preserve_peers_and_late_requests_reopen_the_title_outbox() {
    use plurx_core::transcode::{
        dv_fel_qualification as q, EnhancementRateEnvelope, EnhancementRateLimits,
        EnhancementRateProvenance,
    };
    for_each_backend(|store, backend| async move {
        eprintln!("exact FEL variant backend entered: {backend}");
        let scanner = "a".repeat(64);
        let (_, id) = seed_file(&store, "fel-exact-variants").await;
        let file = store.get_file(id).await.expect("file").expect("file");
        let mut p = probe();
        p.dolby_vision.el_present = Some(true);
        store
            .upsert_file(
                file.item_id,
                file.path.to_str().expect("path"),
                file.size,
                file.mtime,
                &p,
            )
            .await
            .expect("FEL probe");
        let file = store.get_file(id).await.expect("file").expect("file");
        let envelope = |peak| {
            EnhancementRateEnvelope::resolve(
                90_000,
                EnhancementRateProvenance::ContainerAverageVideoUpperBound,
                3840,
                2160,
                (24, 1),
                true,
                EnhancementRateLimits {
                    video_peak_bps: Some(peak),
                    ..Default::default()
                },
            )
            .expect("checked envelope")
        };
        let first =
            q::FelRecipeRequest::new("b".repeat(64), file.size, file.mtime, envelope(135_000_000), q::FelRecipeOutput::new(0,(3840,2160),(24,1)));
        let second =
            q::FelRecipeRequest::new("b".repeat(64), file.size, file.mtime, envelope(130_000_000), q::FelRecipeOutput::new(0,(3840,2160),(24,1)));
        assert_ne!(
            first.identity, second.identity,
            "same target with a different peak is different evidence"
        );
        assert!(
            store
                .request_dv_fel_variant(id, &first)
                .await
                .expect("first request"),
            "{backend}"
        );
        let (scanned, mut job) = active(&store, id, &scanner, 1000).await;
        // Arrives after the worker's snapshot/claim and must survive completion.
        assert!(store
            .request_dv_fel_variant(id, &second)
            .await
            .expect("late request"));
        assert!(store.dv_title_demands(&scanner, 0, 1_000).await.expect("running demand").iter().all(|d|d.file_id!=id));
        // Qualification Busy/preempted does not complete its request. The same
        // durable title job must be discoverable and reclaimable after Yield,
        // including the recipe which arrived after its original snapshot.
        for (now,reason) in [(1001,"fel_busy"),(1004,"fel_preempted")] {
            assert!(store.settle_job(SettleJob {
                token:job.token.clone().expect("owner"), now_ms:now,
                settlement:JobSettlement::Yield { error_code:Some(reason.into()),
                    not_before_ms:now+1,checkpoint:Some(serde_json::json!({"fel_title_aggregate":publication(&scanned,&job,&scanner,now).output})) },
            }).await.expect("yield pending measurement"));
            let resumed=demand(&store,id,&scanner).await;
            assert_eq!(resumed.queued_job_id.as_deref(),Some(job.id.as_str()));
            assert_eq!(q::pending_recipes(&resumed.probe_json,file.size,file.mtime).len(),2);
            job=claim(&store,resumed.queued_job_id.as_deref().expect("queued resume"),now+2).await;
            assert!(job.checkpoint.is_some(),"resume retains scanned title aggregate");
        }
        let receipt = |request: q::FelRecipeRequest, at| q::FelRecipeVerdict {
            verdict: q::FelVerdict::unmeasurable(
                request.identity.clone(),
                request.source_size,
                request.source_mtime,
                at,
            ),
            request,
        };
        assert!(store
            .put_dv_fel_verdict(
                id,
                file.size,
                file.mtime,
                &serde_json::to_value(receipt(first.clone(), 1001)).expect("receipt")
            )
            .await
            .expect("publish first"));
        let document = store
            .get_file_probe_json(id)
            .await
            .expect("probe")
            .expect("probe");
        assert_eq!(
            q::pending_recipes(&document, file.size, file.mtime),
            vec![second.clone()]
        );
        assert!(matches!(
            q::verdict_state(Some(&document), file.size, file.mtime, &first.identity),
            q::VerdictState::Current(_)
        ));
        assert!(store.attach_dv_title_viewer(TitleViewerRequest {
            file_id:id,viewer:"late-exact-viewer".into(),request:second.clone(),scanner_digest:scanner.clone(),
            deadline_ms:60_000,now_ms:1007,
        }).await.expect("late exact viewer"));
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&scanned, &job, &scanner, 1008))
                .await
                .expect("complete"),
            JobPublishOutcome::Published { .. }
        ));
        let waiters=store.job_waiters(WaiterQuery {job_id:job.id.clone(),after:None,limit:20}).await.expect("late viewer waits");
        assert!(waiters.waiters.iter().any(|w|w.consumer_ref==second.identity && w.state=="pending"),
            "a full title aggregate cannot claim exact quality measurement complete");
        assert!(store
            .dv_title_demands(&scanner, 0, 1_000)
            .await
            .expect("pending wakes after completion")
            .iter()
            .any(|d| d.file_id == id));
        assert!(store
            .put_dv_fel_verdict(
                id,
                file.size,
                file.mtime,
                &serde_json::to_value(receipt(second.clone(), 1003)).expect("second receipt")
            )
            .await
            .expect("publish second"));
        let waiters=store.job_waiters(WaiterQuery {job_id:job.id.clone(),after:None,limit:20}).await.expect("exact verdict completion");
        assert!(waiters.waiters.iter().any(|w|w.consumer_ref==second.identity && w.state=="succeeded"));
        let document = store
            .get_file_probe_json(id)
            .await
            .expect("probe")
            .expect("probe");
        assert!(q::pending_recipes(&document, file.size, file.mtime).is_empty());
        for request in [&first, &second] {
            assert!(matches!(
                q::verdict_state(Some(&document), file.size, file.mtime, &request.identity),
                q::VerdictState::Current(_)
            ));
        }
        assert_eq!(
            store.dv_fel_verdict_counts(&"b".repeat(64)).await.expect("variant readiness counts"),
            (1, 0), "multiple completed variants count this title only once"
        );
        assert_eq!(
            store.dv_fel_verdict_counts(&"f".repeat(64)).await.expect("other backend counts"),
            (0, 0)
        );
        assert!(store
            .put_dv_fel_verdict(
                id, file.size + 1, file.mtime,
                &serde_json::to_value(receipt(first.clone(), 1004)).expect("inconsistent receipt")
            ).await.is_err(), "mismatched API/request/verdict source facts are malformed");
        let mut stale = first.clone();
        stale.source_size += 1;
        assert!(!store
            .put_dv_fel_verdict(
                id,
                file.size + 1,
                file.mtime,
                &serde_json::to_value(receipt(stale, 1004)).expect("coherent stale source receipt")
            )
            .await
            .expect("source fence"));
        for i in 0..8 {
            let request = q::FelRecipeRequest::new(
                format!("{i:064x}"),
                file.size,
                file.mtime,
                envelope(130_000_000),
                q::FelRecipeOutput::new(0,(3840,2160),(24,1)),
            );
            assert!(store
                .request_dv_fel_variant(id, &request)
                .await
                .expect("bounded request"));
        }
        let ninth =
            q::FelRecipeRequest::new("f".repeat(64), file.size, file.mtime, envelope(130_000_000), q::FelRecipeOutput::new(0,(3840,2160),(24,1)));
        assert!(!store
            .request_dv_fel_variant(id, &ninth)
            .await
            .expect("pending bound"));
        let pending_doc = store
            .get_file_probe_json(id)
            .await
            .expect("pending")
            .expect("pending");
        for (i, request) in q::pending_recipes(&pending_doc, file.size, file.mtime)
            .into_iter()
            .enumerate()
        {
            assert!(store
                .put_dv_fel_verdict(
                    id,
                    file.size,
                    file.mtime,
                    &serde_json::to_value(receipt(request, 2000 + i as i64)).expect("receipt")
                )
                .await
                .expect("publish bounded variants"));
        }
        let final_doc = store
            .get_file_probe_json(id)
            .await
            .expect("final")
            .expect("final");
        let parsed: serde_json::Value = serde_json::from_str(&final_doc).expect("JSON");
        assert_eq!(
            parsed[q::VARIANTS_KEY].as_object().expect("variants").len(),
            8
        );
        assert!(q::pending_recipes(&final_doc, file.size, file.mtime).is_empty());
        assert!(
            matches!(
                q::verdict_state(Some(&final_doc), file.size, file.mtime, &first.identity),
                q::VerdictState::Missing
            ),
            "oldest complete variant evicts only under bounded pressure"
        );
        eprintln!("exact FEL variant backend completed: {backend}");
    })
    .await;
}

/// A viewer interest uses the ordinary enqueue authority and claim path.
async fn playback_title(
    store: &Arc<dyn Store>,
    demand: &TitleDemand,
    scanner: &str,
    deadline_ms: Option<i64>,
) -> String {
    let recipe = playback_recipe(demand);
    assert!(store
        .request_dv_fel_variant(demand.file_id, &recipe)
        .await
        .expect("pending exact recipe"));
    let id = uuid::Uuid::new_v4().to_string();
    let outcome = store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::DvTitleScan {
                file_id: demand.file_id,
                source_size: demand.source_size,
                source_mtime: demand.source_mtime,
                probe_revision: demand.probe_revision,
                scanner_digest: scanner.into(),
            },
            dedupe_key: format!("playback-title:{id}"),
            priority: 3,
            not_before_ms: 1_000,
            now_ms: 1_000,
            request: JobRequest {
                scope: "playback-title".into(),
                request_id: id.clone(),
                request_digest: "b".repeat(64),
                consumer_kind: "playback_qualification".into(),
                consumer_ref: recipe.identity.clone(),
                target_node_id: None,
                deadline_ms,
                retain_identity: false,
            },
        })
        .await
        .expect("enqueue viewer through actual source authority");
    assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
    id
}

#[tokio::test]
async fn title_live_playback_demands_precede_cursor_and_remain_visible_while_running() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let viewer = seed_playback(&store, "title-live-viewer").await;
        let requested = bound(&store, viewer.id, &scanner).await;
        let ordinary = seed(&store, "title-live-ordinary").await;
        let later = bound(&store, ordinary.id, &scanner).await;
        let job_id = playback_title(&store, &requested, &scanner, Some(5_000)).await;
        let rows = store
            .dv_title_playback_demands(0, 1_001, None)
            .await
            .expect("independent live cursor");
        assert_eq!(rows.len(), 1, "{backend}");
        assert_eq!(rows[0].file_id, viewer.id);
        assert!(rows[0].playback_requested);
        assert_eq!(
            rows[0].playback_recipes,
            vec![playback_recipe(&requested).identity]
        );
        assert_eq!(rows[0].queued_job_id.as_deref(), Some(job_id.as_str()));
        assert_eq!(rows[0].scanner_digest.as_deref(), Some(scanner.as_str()));
        assert!(store
            .dv_title_playback_demands(requested.probe_revision, 1_001, None)
            .await
            .expect("live cursor applies")
            .is_empty());
        let ordinary_rows = store
            .dv_title_demands(&scanner, requested.probe_revision, 1_001)
            .await
            .expect("maintenance cursor independent");
        assert_eq!(
            ordinary_rows.iter().map(|d| d.file_id).collect::<Vec<_>>(),
            vec![ordinary.id]
        );
        let _running = claim(&store, &job_id, 1_002).await;
        let rows = store
            .dv_title_playback_demands(later.probe_revision, 1_003, Some(viewer.id))
            .await
            .expect("direct owned live interest");
        assert_eq!(rows.len(), 1, "{backend}");
        assert!(rows[0].queued_job_id.is_none());
        assert_eq!(rows[0].active_job_id.as_deref(), Some(job_id.as_str()));
        assert!(store.dv_title_playback_demands(0, -1, None).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn title_expired_unbounded_cancelled_and_stale_playback_do_not_promote() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        for case in ["expired", "unbounded", "cancelled", "source", "scanner"] {
            let file = seed_playback(&store, &format!("title-inactive-{case}")).await;
            let requested = bound(&store, file.id, &scanner).await;
            let deadline = match case {
                "expired" => Some(1_001),
                "unbounded" => None,
                _ => Some(5_000),
            };
            let job_id = playback_title(&store, &requested, &scanner, deadline).await;
            match case {
                "cancelled" => {
                    store
                        .cancel_waiter(CancelWaiter {
                            scope: "playback-title".into(),
                            request_id: job_id,
                            now_ms: 1_001,
                        })
                        .await
                        .expect("cancel viewer");
                }
                "source" => {
                    store
                        .upsert_file(
                            file.item_id,
                            file.path.to_str().expect("path"),
                            file.size + 1,
                            file.mtime + 1,
                            &probe(),
                        )
                        .await
                        .expect("replace source");
                }
                "scanner" => {
                    store
                        .bind_dv_title_demand(requested.probe_revision, &"c".repeat(64))
                        .await
                        .expect("replace scanner generation");
                }
                _ => {}
            }
            let rows = store
                .dv_title_demands(&scanner, 0, 1_001)
                .await
                .expect("inactive viewer");
            assert!(
                rows.iter().all(|d| !d.playback_requested),
                "{backend}: {case}"
            );
            let cursor = rows
                .iter()
                .map(|d| d.probe_revision)
                .max()
                .unwrap_or(requested.probe_revision);
            assert!(
                store
                    .dv_title_demands(&scanner, cursor, 1_001)
                    .await
                    .expect("past cursor")
                    .iter()
                    .all(|d| d.file_id != file.id),
                "{backend}: {case}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn title_ordinary_demands_keep_bounded_revision_paging() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let mut ids = Vec::new();
        for n in 0..130 {
            ids.push(seed(&store, &format!("title-page-{n}")).await.id);
        }
        let first = store
            .dv_title_demands(&scanner, 0, 1_000)
            .await
            .expect("first page");
        assert_eq!(first.len(), 128, "{backend}");
        assert!(first.iter().all(|d| !d.playback_requested));
        assert!(first
            .windows(2)
            .all(|d| d[0].probe_revision < d[1].probe_revision));
        let second = store
            .dv_title_demands(&scanner, first.last().expect("page").probe_revision, 1_000)
            .await
            .expect("remaining page");
        assert_eq!(
            second.iter().map(|d| d.file_id).collect::<Vec<_>>(),
            ids[128..],
            "{backend}"
        );
    })
    .await;
}

async fn seed_playback(store: &Arc<dyn Store>, name: &str) -> MediaFile {
    let file = seed(store, name).await;
    let mut fel_probe = probe();
    fel_probe.dolby_vision.el_present = Some(true);
    store
        .upsert_file(
            file.item_id,
            file.path.to_str().expect("path"),
            file.size,
            file.mtime,
            &fel_probe,
        )
        .await
        .expect("FEL source facts");
    store.get_file(file.id).await.expect("file").expect("file")
}

fn playback_recipe(
    demand: &TitleDemand,
) -> plurx_core::transcode::dv_fel_qualification::FelRecipeRequest {
    use plurx_core::transcode::{
        dv_fel_qualification as q, EnhancementRateEnvelope, EnhancementRateLimits,
        EnhancementRateProvenance,
    };
    let envelope = EnhancementRateEnvelope::resolve(
        90_000,
        EnhancementRateProvenance::ContainerAverageVideoUpperBound,
        3840,
        2160,
        (24, 1),
        true,
        EnhancementRateLimits::default(),
    )
    .expect("recipe envelope");
    q::FelRecipeRequest::new(
        "b".repeat(64),
        demand.source_size,
        demand.source_mtime,
        envelope,
        q::FelRecipeOutput::new(0, (3840, 2160), (24, 1)),
    )
}

#[tokio::test]
async fn title_consumed_playback_recipe_returns_to_ordinary_paging() {
    use plurx_core::transcode::dv_fel_qualification as q;
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed_playback(&store, "title-consumed-playback").await;
        let demand = bound(&store, file.id, &scanner).await;
        let _job = playback_title(&store, &demand, &scanner, Some(5_000)).await;
        assert!(
            store
                .dv_title_playback_demands(0, 1_001, None)
                .await
                .expect("live")[0]
                .playback_requested
        );
        let request = playback_recipe(&demand);
        let verdict =
            q::FelVerdict::unmeasurable(request.identity.clone(), file.size, file.mtime, 1_002);
        assert!(store
            .put_dv_fel_verdict(
                file.id,
                file.size,
                file.mtime,
                &serde_json::to_value(q::FelRecipeVerdict { request, verdict }).expect("receipt")
            )
            .await
            .expect("independent quality receipt"));
        assert!(
            store
                .dv_title_demands(&scanner, demand.probe_revision, 1_003)
                .await
                .expect("past cursor")
                .is_empty(),
            "{backend}"
        );
        let maintenance = store
            .dv_title_demands(&scanner, 0, 1_003)
            .await
            .expect("maintenance");
        assert_eq!(maintenance.len(), 1, "{backend}");
        assert!(!maintenance[0].playback_requested);
    })
    .await;
}

#[tokio::test]
async fn title_viewer_bootstraps_current_work_and_keeps_recipe_interest_exact() {
    use plurx_core::transcode::dv_fel_qualification as q;
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed_playback(&store, "title-bootstrap-viewer").await;
        let demand = demand(&store, file.id, &scanner).await;
        let first = playback_recipe(&demand);
        assert!(store
            .request_dv_fel_variant(file.id, &first)
            .await
            .expect("register first"));
        let viewer = |request: q::FelRecipeRequest, now| TitleViewerRequest {
            file_id: file.id,
            viewer: "viewer".into(),
            request,
            scanner_digest: scanner.clone(),
            deadline_ms: now + 60_000,
            now_ms: now,
        };
        assert!(
            store
                .attach_dv_title_viewer(viewer(first.clone(), 1_000))
                .await
                .expect("bootstrap without physical admission"),
            "{backend}"
        );
        let live = store
            .dv_title_playback_demands(0, 1_001, None)
            .await
            .expect("fresh queued viewer");
        assert_eq!(live.len(), 1);
        let job = live[0].queued_job_id.clone().expect("bootstrapped job");
        assert!(store
            .attach_dv_title_viewer(viewer(first.clone(), 1_002))
            .await
            .expect("idempotent renewal"));
        assert_eq!(
            store
                .dv_title_playback_demands(0, 1_003, None)
                .await
                .expect("renewed")[0]
                .queued_job_id
                .as_deref(),
            Some(job.as_str())
        );
        let envelope = plurx_core::transcode::EnhancementRateEnvelope::resolve(
            90_000,
            plurx_core::transcode::EnhancementRateProvenance::ContainerAverageVideoUpperBound,
            3840,
            2160,
            (24, 1),
            true,
            plurx_core::transcode::EnhancementRateLimits {
                video_peak_bps: Some(130_000_000),
                ..Default::default()
            },
        )
        .expect("second exact envelope");
        let second = q::FelRecipeRequest::new(
            first.backend_identity.clone(),
            file.size,
            file.mtime,
            envelope,
            first.output,
        );
        assert!(second.valid());
        assert!(store
            .request_dv_fel_variant(file.id, &second)
            .await
            .expect("peer request"));
        assert_eq!(
            store
                .dv_title_playback_demands(0, 1_004, None)
                .await
                .expect("only exact viewer recipe")[0]
                .playback_recipes,
            vec![first.identity.clone()],
            "an unrelated pending backend is not live work"
        );
        assert!(store
            .attach_dv_title_viewer(TitleViewerRequest {
                viewer: "peer".into(),
                ..viewer(second.clone(), 1_004)
            })
            .await
            .expect("peer exact waiter"));
        let receipt = q::FelRecipeVerdict {
            request: first.clone(),
            verdict: q::FelVerdict::unmeasurable(
                first.identity.clone(),
                file.size,
                file.mtime,
                1_005,
            ),
        };
        assert!(store
            .put_dv_fel_verdict(
                file.id,
                file.size,
                file.mtime,
                &serde_json::to_value(receipt).expect("receipt")
            )
            .await
            .expect("finish exact measurement"));
        let waiters = store
            .job_waiters(WaiterQuery {
                job_id: job.clone(),
                after: None,
                limit: 20,
            })
            .await
            .expect("waiters");
        assert!(waiters
            .waiters
            .iter()
            .any(|w| w.consumer_ref == first.identity && w.state == "succeeded"));
        assert!(waiters
            .waiters
            .iter()
            .any(|w| w.consumer_ref == second.identity && w.state == "pending"));
        assert!(!store
            .attach_dv_title_viewer(viewer(first, 1_006))
            .await
            .expect("completed recipe cannot revive"));
        assert!(store
            .attach_dv_title_viewer(viewer(second.clone(), 1_007))
            .await
            .expect("same viewer advances exact recipe"));
        let live = store
            .dv_title_playback_demands(0, 1_008, None)
            .await
            .expect("remaining recipe");
        assert_eq!(live[0].playback_recipes, vec![second.identity]);
    })
    .await;
}

#[tokio::test]
async fn title_live_paging_reaches_after_full_page_and_direct_own_interest() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let initial = seed_playback(&store, "title-many-live").await;
        let mut last = initial.id;
        for n in 0..129 {
            let id = if n == 0 {
                initial.id
            } else {
                let mut p = probe();
                p.dolby_vision.el_present = Some(true);
                store
                    .upsert_file(
                        initial.item_id,
                        &format!("/title-many-live/{n}.mkv"),
                        initial.size,
                        initial.mtime,
                        &p,
                    )
                    .await
                    .expect("distinct source")
            };
            let demand = bound(&store, id, &scanner).await;
            playback_title(&store, &demand, &scanner, Some(60_000)).await;
            last = id;
        }
        let first = store
            .dv_title_playback_demands(0, 1_001, None)
            .await
            .expect("first live page");
        assert_eq!(first.len(), 128, "{backend}");
        let second = store
            .dv_title_playback_demands(first.last().expect("cursor").probe_revision, 1_001, None)
            .await
            .expect("second live page");
        assert_eq!(
            second.iter().map(|d| d.file_id).collect::<Vec<_>>(),
            vec![last],
            "{backend}"
        );
        let own = store
            .dv_title_playback_demands(0, 1_001, Some(last))
            .await
            .expect("direct own interest");
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].file_id, last);
    })
    .await;
}

#[tokio::test]
async fn title_live_preemption_rejects_running_and_backoff_candidates() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let maintenance = seed(&store, "title-preempt-maintenance").await;
        let (_, current) = active(&store, maintenance.id, &scanner, 1_000).await;
        let viewer = seed_playback(&store, "title-preempt-viewer").await;
        let demand = bound(&store, viewer.id, &scanner).await;
        let candidate = playback_title(&store, &demand, &scanner, Some(60_000)).await;
        assert!(
            store
                .dv_title_can_preempt(&candidate, &current.id, "title-node", 1_001)
                .await
                .expect("queued eligible"),
            "{backend}"
        );
        let running = claim(&store, &candidate, 1_002).await;
        assert!(!store
            .dv_title_can_preempt(&candidate, &current.id, "title-node", 1_003)
            .await
            .expect("healthy running"));
        assert!(store
            .settle_job(SettleJob {
                token: running.token.expect("owner"),
                now_ms: 1_004,
                settlement: JobSettlement::Yield {
                    error_code: Some("busy".into()),
                    not_before_ms: 10_000,
                    checkpoint: None
                }
            })
            .await
            .expect("backoff"));
        assert!(!store
            .dv_title_can_preempt(&candidate, &current.id, "title-node", 1_005)
            .await
            .expect("future retry"));
    })
    .await;
}

#[tokio::test]
async fn title_live_handoff_accounts_for_current_reader_release() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let maintenance = seed(&store, "title-handoff-current").await;
        let (_, current) = active(&store, maintenance.id, &scanner, 1_000).await;
        let other = seed_playback(&store, "title-handoff-other").await;
        let other_demand = bound(&store, other.id, &scanner).await;
        let other_id = playback_title(&store, &other_demand, &scanner, Some(60_000)).await;
        let _other = claim(&store, &other_id, 1_001).await;
        let viewer = seed_playback(&store, "title-handoff-viewer").await;
        let demand = bound(&store, viewer.id, &scanner).await;
        let candidate = playback_title(&store, &demand, &scanner, Some(60_000)).await;
        assert!(
            store
                .dv_title_can_preempt(&candidate, &current.id, "title-node", 1_002)
                .await
                .expect("current release makes one slot"),
            "{backend}"
        );
        assert!(store
            .settle_job(SettleJob {
                token: current.token.expect("owner"),
                now_ms: 1_003,
                settlement: JobSettlement::Yield {
                    error_code: Some("viewer".into()),
                    not_before_ms: 10_000,
                    checkpoint: None
                }
            })
            .await
            .expect("current owner yields"));
        let _claimed = claim(&store, &candidate, 1_004).await;
    })
    .await;
}

#[tokio::test]
async fn title_terminal_episode_new_recipe_bootstraps_a_new_job() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed_playback(&store, "title-new-episode").await;
        let demand = bound(&store, file.id, &scanner).await;
        let first = playback_recipe(&demand);
        assert!(store
            .request_dv_fel_variant(file.id, &first)
            .await
            .expect("pending"));
        let attach = |request, now| TitleViewerRequest {
            file_id: file.id,
            viewer: "viewer".into(),
            request,
            scanner_digest: scanner.clone(),
            deadline_ms: now + 60_000,
            now_ms: now,
        };
        assert!(store
            .attach_dv_title_viewer(attach(first.clone(), 1_000))
            .await
            .expect("first episode"));
        let id = store
            .dv_title_playback_demands(0, 1_001, None)
            .await
            .expect("live")[0]
            .queued_job_id
            .clone()
            .expect("job");
        let job = claim(&store, &id, 1_002).await;
        assert!(store
            .settle_job(SettleJob {
                token: job.token.expect("owner"),
                now_ms: 1_003,
                settlement: JobSettlement::Stop {
                    error_code: "unmeasurable".into()
                }
            })
            .await
            .expect("terminal episode"));
        let second = plurx_core::transcode::dv_fel_qualification::FelRecipeRequest::new(
            "c".repeat(64),
            file.size,
            file.mtime,
            first.envelope,
            first.output,
        );
        assert!(store
            .request_dv_fel_variant(file.id, &second)
            .await
            .expect("new pending"));
        assert!(store
            .attach_dv_title_viewer(attach(second.clone(), 1_004))
            .await
            .expect("new episode"));
        let next = store
            .dv_title_playback_demands(0, 1_005, None)
            .await
            .expect("new live");
        assert_ne!(
            next[0].queued_job_id.as_deref(),
            Some(id.as_str()),
            "{backend}"
        );
        assert_eq!(next[0].playback_recipes, vec![second.identity]);
    })
    .await;
}

#[tokio::test]
async fn title_manual_cancellation_suppresses_same_viewer_renewal_until_new_intent() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed_playback(&store, "title-cancelled-viewer").await;
        let demand = demand(&store, file.id, &scanner).await;
        let recipe = playback_recipe(&demand);
        assert!(store
            .request_dv_fel_variant(file.id, &recipe)
            .await
            .expect("pending"));
        let viewer = |name: &str, now| TitleViewerRequest {
            file_id: file.id,
            viewer: name.into(),
            request: recipe.clone(),
            scanner_digest: scanner.clone(),
            deadline_ms: now + 60_000,
            now_ms: now,
        };
        assert!(store
            .attach_dv_title_viewer(viewer("original", 1_000))
            .await
            .expect("first intent"));
        let id = store
            .dv_title_playback_demands(0, 1_001, None)
            .await
            .expect("live")[0]
            .active_job_id
            .clone()
            .expect("job");
        store
            .cancel_job(CancelJob {
                job_id: id.clone(),
                now_ms: 1_002,
            })
            .await
            .expect("manual cancel");
        assert!(store
            .request_dv_fel_variant(file.id, &recipe)
            .await
            .expect("retained request"));
        assert!(
            !store
                .attach_dv_title_viewer(viewer("original", 1_003))
                .await
                .expect("renewal respects manual cancellation"),
            "{backend}"
        );
        assert!(store
            .dv_title_demands(&scanner, 0, 1_004)
            .await
            .expect("cancelled outbox stays terminal")
            .is_empty());
        assert!(store
            .dv_title_playback_demands(0, 1_004, None)
            .await
            .expect("no revived viewer")
            .is_empty());
        assert!(store
            .attach_dv_title_viewer(viewer("new-viewer", 1_005))
            .await
            .expect("new independent intent"));
        assert_ne!(
            store
                .dv_title_playback_demands(0, 1_006, None)
                .await
                .expect("new viewer")[0]
                .active_job_id
                .as_deref(),
            Some(id.as_str())
        );
    })
    .await;
}
