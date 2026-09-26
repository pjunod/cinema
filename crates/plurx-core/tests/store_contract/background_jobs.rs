use super::{for_each_backend, seed_file};
use plurx_core::domain::PretranscodeRequirements;
use plurx_core::store::background_jobs::*;

#[tokio::test]
async fn background_jobs_transcode_publication_is_atomic_idempotent_and_source_fenced() {
    for_each_backend(|store, backend| async move {
        for scenario in ["published", "source_replaced", "cancelled"] {
            let (_, file_id) = seed_file(&store, &format!("background-{scenario}")).await;
            let file = store.get_file(file_id).await.expect("file").expect("file");
            let job_id = uuid::Uuid::new_v4().to_string();
            let recipe = match scenario {
                "published" => "a".repeat(64),
                "source_replaced" => "b".repeat(64),
                _ => "c".repeat(64),
            };
            let accepted = store
                .enqueue_job(EnqueueJob {
                    id: job_id.clone(),
                    payload: JobPayload::TranscodePrepare {
                        file_id,
                        source_generation: format!("source:{file_id}"),
                        source_size: file.size,
                        source_mtime: file.mtime,
                        recipe_key: recipe.clone(),
                        target_height: 720,
                        policy_generation: "policy:1".to_owned(),
                        requirements: PretranscodeRequirements {
                            version: 1,
                            decoder: "h264".to_owned(),
                            acceptable_encoder_families: vec!["software".to_owned()],
                            output_contract: "hls-v1".to_owned(),
                            tone_map: false,
                            output_grade: "sdr".to_owned(),
                            scratch_bytes: 1_024,
                        },
                        reason: "recent".to_owned(),
                    },
                    dedupe_key: format!("transcode:{recipe}"),
                    priority: 1,
                    not_before_ms: 1_000,
                    now_ms: 1_000,
                    request: JobRequest {
                        scope: "internal:pretranscode".to_owned(),
                        request_id: job_id.clone(),
                        request_digest: recipe.clone(),
                        consumer_kind: "pretranscode".to_owned(),
                        consumer_ref: job_id.clone(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: enqueue: {error}"));
            assert!(matches!(accepted, EnqueueOutcome::Accepted { .. }));
            let claim = store
                .claim_job(ClaimJob {
                    job_id: job_id.clone(),
                    expected_revision: 0,
                    node_id: "node-a".to_owned(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::TranscodePrepare,
                    payload_version: 1,
                    now_ms: 1_000,
                    dispatched_at_ms: 1_000,
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim: {error}"));
            let ClaimOutcome::Claimed { job } = claim else {
                panic!("{backend}: claimed")
            };
            let publication = PublishTranscodeJob {
                token: job.token.expect("token"),
                output: TranscodeJobOutput {
                    recipe_hash: recipe,
                    recipe_version: 1,
                    relative_dir: scenario.to_owned(),
                    bytes: 100,
                    expected_previous_bytes: None,
                    manifest_digest: "d".repeat(64),
                },
                now_ms: 2_000,
            };
            if scenario == "source_replaced" {
                store
                    .upsert_file(
                        file.item_id,
                        file.path.to_str().expect("path"),
                        file.size + 1,
                        file.mtime + 1,
                        &Default::default(),
                    )
                    .await
                    .expect("rescan");
            } else if scenario == "cancelled" {
                store
                    .cancel_job(CancelJob {
                        job_id,
                        now_ms: 1_500,
                    })
                    .await
                    .expect("cancel");
            }
            let result = store
                .publish_transcode_job(publication.clone())
                .await
                .unwrap_or_else(|error| panic!("{backend}: publish: {error}"));
            match scenario {
                "published" => {
                    assert!(
                        matches!(result, JobPublishOutcome::Published { .. }),
                        "{backend}: {result:?}"
                    );
                    let repeated = store
                        .publish_transcode_job(publication)
                        .await
                        .expect("reconcile");
                    assert!(matches!(
                        repeated,
                        JobPublishOutcome::AlreadyPublished { .. }
                    ));
                }
                "source_replaced" => assert!(matches!(result, JobPublishOutcome::LostOwnership)),
                _ => assert!(matches!(result, JobPublishOutcome::LostOwnership)),
            }
            assert_eq!(
                store.cache_bytes("node-a").await.expect("cache bytes"),
                100,
                "{backend}: only the first successful publication may create a cache location"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn background_jobs_one_fragment_build_keeps_remote_delivery_durable() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "durable-fragment-delivery").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let source_sha256 = "a".repeat(64);
        let pipeline_digest = "b".repeat(64);
        let cache_key = plurx_core::store::cluster_fragment_index_key(
            file_id,
            file.size,
            file.mtime,
            &source_sha256,
            &pipeline_digest,
        )
        .expect("key");
        let id = uuid::Uuid::new_v4().to_string();
        let payload = JobPayload::FragmentIndexBuild {
            file_id,
            source_generation: cache_key.clone(),
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: source_sha256.clone(),
            cache_key: cache_key.clone(),
            pipeline_digest: pipeline_digest.clone(),
        };
        for (index, target) in ["node-a", "node-b", "node-b"].into_iter().enumerate() {
            let outcome = store
                .enqueue_job(EnqueueJob {
                    id: if index == 0 {
                        id.clone()
                    } else {
                        uuid::Uuid::new_v4().to_string()
                    },
                    payload: payload.clone(),
                    dedupe_key: format!("fragment:{cache_key}"),
                    priority: 2,
                    not_before_ms: 1_000,
                    now_ms: 1_000,
                    request: JobRequest {
                        scope: "user:1".into(),
                        request_id: format!("fragment-{index}"),
                        request_digest: "c".repeat(64),
                        consumer_kind: "analysis".into(),
                        consumer_ref: index.to_string(),
                        target_node_id: Some(target.into()),
                        deadline_ms: None,
                        retain_identity: true,
                    },
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: enqueue: {error}"));
            assert!(matches!(outcome, EnqueueOutcome::Accepted { job_id, .. } if job_id == id));
        }
        store
            .cancel_waiter(CancelWaiter {
                scope: "user:1".into(),
                request_id: "fragment-2".into(),
                now_ms: 1_001,
            })
            .await
            .expect("cancel one interest");
        let revision = store
            .background_job(&id)
            .await
            .expect("read")
            .expect("job")
            .revision;
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: revision,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms: 1_002,
                dispatched_at_ms: 1_002,
            })
            .await
            .expect("build claim")
        else {
            panic!("{backend}: build not claimed")
        };
        let artifact = plurx_core::store::ClusterFragmentIndexArtifact {
            cache_key: cache_key.clone(),
            file_id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256,
            pipeline_sha256: pipeline_digest,
            blob_sha256: "d".repeat(64),
            bytes: 100,
            built_by_node_id: "node-a".into(),
            built_at_ms: 1_003,
        };
        let publication = PublishFragmentJob {
            token: job.token.expect("token"),
            artifact: artifact.clone(),
            now_ms: 1_003,
        };
        assert!(matches!(
            store
                .publish_fragment_job(publication.clone())
                .await
                .expect("build publication"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store
                .publish_fragment_job(publication)
                .await
                .expect("lost acknowledgement replay"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        let waiters = store
            .job_waiters(WaiterQuery {
                job_id: id.clone(),
                after: None,
                limit: 100,
            })
            .await
            .expect("receipts")
            .waiters;
        assert_eq!(
            waiters
                .iter()
                .map(|waiter| waiter.state.as_str())
                .collect::<Vec<_>>(),
            ["succeeded", "awaiting_hydration", "cancelled"],
            "{backend}"
        );
        // A scheduler restart needs no in-memory callback to recover this intent.
        let intents = store.delivery_intents(1_004).await.expect("durable outbox");
        assert_eq!(intents.len(), 1, "{backend}");
        assert_eq!(intents[0].target_node_id, "node-b");
        let EnqueueOutcome::Accepted {
            job_id: hydration_id,
            ..
        } = store
            .enqueue_delivery(intents[0].clone(), 1_004)
            .await
            .expect("schedule delivery")
        else {
            panic!("{backend}: hydration not admitted")
        };
        assert!(store
            .delivery_intents(1_005)
            .await
            .expect("scheduled outbox")
            .is_empty());
        assert!(matches!(
            store
                .enqueue_delivery(intents[0].clone(), 1_005)
                .await
                .expect("retry scheduler acknowledgement"),
            EnqueueOutcome::Existing { .. }
        ));
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: hydration_id.clone(),
                expected_revision: 0,
                node_id: "node-b".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::ArtifactHydrate,
                payload_version: 1,
                now_ms: 1_005,
                dispatched_at_ms: 1_005,
            })
            .await
            .expect("delivery claim")
        else {
            panic!("{backend}: delivery not claimed")
        };
        assert!(matches!(
            store
                .publish_fragment_job(PublishFragmentJob {
                    token: job.token.expect("token"),
                    artifact: artifact.clone(),
                    now_ms: 1_006
                })
                .await
                .expect("delivery publication"),
            JobPublishOutcome::Published { .. }
        ));
        let waiters = store
            .job_waiters(WaiterQuery {
                job_id: id,
                after: None,
                limit: 100,
            })
            .await
            .expect("delivered receipts")
            .waiters;
        assert_eq!(
            waiters
                .iter()
                .map(|waiter| waiter.state.as_str())
                .collect::<Vec<_>>(),
            ["succeeded", "succeeded", "cancelled"],
            "{backend}"
        );
        assert_eq!(
            store
                .cluster_fragment_index_artifact(&cache_key)
                .await
                .expect("artifact")
                .expect("artifact"),
            artifact,
            "hydration preserves the original builder: {backend}"
        );
        // Losing a repaired copy in a later rate window must not be hidden
        // behind the previous repair's seven-day receipt.
        let mut prior_repair = None;
        for now_ms in [2_000, 3_602_000] {
            store
                .forget_cluster_fragment_index_location(&cache_key, "node-a")
                .await
                .expect("lose local holder");
            let replacement = plurx_core::store::NewClusterFragmentIndexJob {
                cache_key: cache_key.clone(),
                file_id,
                source_size: file.size,
                source_mtime: file.mtime,
                source_sha256: artifact.source_sha256.clone(),
                pipeline_sha256: artifact.pipeline_sha256.clone(),
                priority: "normal".into(),
                trigger: "background".into(),
                target_node_id: "node-a".into(),
                not_before_ms: now_ms,
                created_at_ms: now_ms,
            };
            let input = EnqueueFragmentJob {
                job: replacement.clone(),
                analysis_request: None,
                repair: true,
                now_ms,
            };
            let EnqueueOutcome::Accepted { job_id, .. } = store
                .enqueue_fragment_job(input.clone())
                .await
                .expect("repair admission")
            else {
                panic!("{backend}: a new repair window was suppressed");
            };
            assert_ne!(prior_repair.as_ref(), Some(&job_id));
            assert!(matches!(
                store
                    .enqueue_fragment_job(input)
                    .await
                    .expect("same window replay"),
                EnqueueOutcome::Existing { .. }
            ));
            let ClaimOutcome::Claimed { job } = store
                .claim_job(ClaimJob {
                    job_id: job_id.clone(),
                    expected_revision: 0,
                    node_id: "node-a".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::FragmentIndexBuild,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                })
                .await
                .expect("repair claim")
            else {
                panic!("{backend}: repair was not claimable");
            };
            assert!(matches!(
                store
                    .publish_fragment_job(PublishFragmentJob {
                        token: job.token.expect("repair token"),
                        artifact: artifact.clone(),
                        now_ms: now_ms + 1,
                    })
                    .await
                    .expect("repair publication"),
                JobPublishOutcome::Published { .. }
            ));
            assert!(!store
                .requeue_cluster_fragment_index(&replacement)
                .await
                .expect("completed receipt is not pending work"));
            prior_repair = Some(job_id);
        }
    })
    .await;
}

#[tokio::test]
async fn background_jobs_fragment_targets_share_claims_and_keep_domain_history() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "durable-fragment-domain").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let source = "a".repeat(64);
        let pipeline = "b".repeat(64);
        let key = plurx_core::store::cluster_fragment_index_key(
            file_id, file.size, file.mtime, &source, &pipeline,
        )
        .expect("key");
        let mut id = String::new();
        for target in ["node-a", "node-b"] {
            let outcome = store
                .enqueue_fragment_job(EnqueueFragmentJob {
                    job: plurx_core::store::NewClusterFragmentIndexJob {
                        cache_key: key.clone(),
                        file_id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                        source_sha256: source.clone(),
                        pipeline_sha256: pipeline.clone(),
                        priority: "normal".into(),
                        trigger: "background".into(),
                        target_node_id: target.into(),
                        not_before_ms: 1_000,
                        created_at_ms: 1_000,
                    },
                    analysis_request: None,
                    repair: false,
                    now_ms: 1_000,
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: admit target: {error}"));
            let EnqueueOutcome::Accepted { job_id, .. } = outcome else {
                panic!("{backend}: not accepted: {outcome:?}")
            };
            if id.is_empty() {
                id = job_id;
            } else {
                assert_eq!(id, job_id);
            }
        }
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "node-c".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::FragmentIndexBuild,
                payload_version: 1,
                now_ms: 1_001,
                dispatched_at_ms: 1_001,
            })
            .await
            .expect("claim")
        else {
            panic!("{backend}: not claimed")
        };
        for target in ["node-a", "node-b"] {
            let domain = store
                .cluster_fragment_index_job(&key, target)
                .await
                .expect("domain row")
                .expect("domain row");
            assert_eq!(domain.owner_node_id, "node-c", "{backend}");
            assert_eq!(domain.state, "running");
            assert_eq!(domain.attempts, 1);
        }
        store
            .fail_fragment_job(FragmentJobFailure {
                token: job.token.expect("token"),
                code: plurx_core::content_analysis::IndexFailureCode::IndexBudgetExceeded,
                transient_allowlisted: false,
                diagnostic: Default::default(),
                now_ms: 1_002,
            })
            .await
            .expect("typed failure");
        for target in ["node-a", "node-b"] {
            let domain = store
                .cluster_fragment_index_job(&key, target)
                .await
                .expect("domain row")
                .expect("domain row");
            assert_eq!(domain.state, "queued");
            assert_eq!(domain.attempts, 1);
            assert_eq!(domain.attempt_errors, "index_budget_exceeded");
            assert!(domain.index_retry_deadline_ms > 1_002);
        }
    })
    .await;
}
