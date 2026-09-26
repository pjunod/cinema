use super::{for_each_backend, seed_file};
#[cfg(feature = "hiqlite-contract-tests")]
use super::{
    open_contract_hiqlite_store, populated_current_import_fixture, ContractCluster, HIQLITE_CASE,
};
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
            if scenario == "published" {
                let metrics = store
                    .prometheus_store_snapshot("node-a", 2)
                    .await
                    .expect("metrics");
                let slot = JOB_METRIC_STATES
                    .iter()
                    .position(|state| *state == "running")
                    .expect("state");
                assert_eq!(metrics.background_jobs.counts[slot], 1, "{backend}");
                assert_eq!(
                    metrics.background_jobs.oldest_age_seconds[slot], 1,
                    "{backend}"
                );
                assert_eq!(
                    metrics.background_jobs.source_io_reservations, 1,
                    "{backend}"
                );
                assert_eq!(metrics.background_jobs.legacy_pending, 0, "{backend}");
            }
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

#[tokio::test]
async fn background_retry_is_new_audited_interest_idempotent_and_source_fenced() {
    for_each_backend(|store, backend| async move {
        let (user_id, file_id) = seed_file(&store, "background-retry").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let original_id = uuid::Uuid::new_v4().to_string();
        let input = preparation_request(&file, &original_id, 1000);
        store.enqueue_job(input.clone()).await.expect("enqueue");
        let request_id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(store.retry_background_job(&original_id, &request_id, user_id, 1001).await.expect("active conflict"), Some(EnqueueOutcome::Conflict)), "{backend}");
        store.cancel_job(CancelJob { job_id: original_id.clone(), now_ms: 1002 }).await.expect("cancel");
        let result = store.retry_background_job(&original_id, &request_id, user_id, 1003).await.expect("retry");
        let Some(EnqueueOutcome::Accepted { job_id, .. }) = result else { panic!("{backend}: retry: {result:?}"); };
        assert_ne!(job_id, original_id);
        let retried = store.background_job(&job_id).await.expect("read").expect("new job");
        assert_eq!(retried.priority, 2);
        assert_eq!(retried.failed_attempts, 0);
        assert_eq!(store.background_job(&original_id).await.expect("original").expect("original").state, JobState::Cancelled);
        assert!(matches!(store.retry_background_job(&original_id, &request_id, user_id, 1004).await.expect("replay"), Some(EnqueueOutcome::Existing { job_id: replay, .. }) if replay == job_id));
        let waiters = store.job_waiters(WaiterQuery { job_id: job_id.clone(), after: None, limit: 100 }).await.expect("audit");
        assert_eq!(waiters.waiters.len(), 1);
        assert_eq!(waiters.waiters[0].consumer_ref, original_id);
        assert_eq!(waiters.waiters[0].scope, format!("user:{user_id}"));
        store.upsert_file(file.item_id, file.path.to_str().expect("path"), file.size + 1, file.mtime + 1, &Default::default()).await.expect("replace source");
        assert!(matches!(store.retry_background_job(&original_id, &uuid::Uuid::new_v4().to_string(), user_id, 1005).await.expect("source refusal"), Some(EnqueueOutcome::SourceChanged)), "{backend}");
        // The original retry receipt is still stable after replacement.
        assert!(matches!(store.retry_background_job(&original_id, &request_id, user_id, 1006).await.expect("replay after replacement"), Some(EnqueueOutcome::Existing { job_id: replay, .. }) if replay == job_id));
    }).await;
}

#[tokio::test]
async fn background_candidates_rotate_scopes_and_offer_lower_work_after_eight_completions() {
    for_each_backend(|store, backend| async move {
        let request = |index: usize, priority: u8, scope: &str| EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::LibraryScan {
                library_id: 1,
                generation: format!("generation:{index}"),
            },
            dedupe_key: format!("fairness:{index}"),
            priority,
            not_before_ms: 1000,
            now_ms: 1000,
            request: JobRequest {
                scope: scope.into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: "a".repeat(64),
                consumer_kind: "scan".into(),
                consumer_ref: "library:1".into(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        };
        let high_old_scope = request(0, 2, "user:1");
        let high_new_scope = request(1, 2, "user:2");
        let low = request(2, 0, "user:3");
        for item in [&high_old_scope, &high_new_scope, &low] {
            store.enqueue_job(item.clone()).await.expect("candidate");
        }
        for index in 0..8 {
            let completed = request(index + 3, 2, "user:1");
            store
                .enqueue_job(completed.clone())
                .await
                .expect("history enqueue");
            let now = 2000 + index as i64 * 10;
            let ClaimOutcome::Claimed { job } = store
                .claim_job(ClaimJob {
                    job_id: completed.id,
                    expected_revision: 0,
                    node_id: "node-a".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::LibraryScan,
                    payload_version: 1,
                    now_ms: now,
                    dispatched_at_ms: now,
                })
                .await
                .expect("history claim")
            else {
                panic!("{backend}: history claim");
            };
            store
                .settle_job(SettleJob {
                    token: job.token.expect("token"),
                    settlement: JobSettlement::Fail {
                        error_code: "unsupported".into(),
                    },
                    now_ms: now + 1,
                })
                .await
                .expect("terminal completion");
            if index == 6 {
                let candidates = store
                    .job_candidates(CandidateQuery {
                        node_id: "node-a".into(),
                        kinds: vec![JobKind::LibraryScan],
                        after: None,
                        now_ms: 2070,
                        limit: 100,
                    })
                    .await
                    .expect("seven completions");
                assert_eq!(
                    candidates.jobs[0].id, high_new_scope.id,
                    "{backend}: rotate the previously unserved user first within priority"
                );
                assert_eq!(candidates.jobs[1].id, high_old_scope.id);
                assert_eq!(candidates.jobs[2].id, low.id);
            }
        }
        let viewer = request(20, 3, "user:4");
        store.enqueue_job(viewer.clone()).await.expect("viewer");
        let mut cursor = None;
        let mut order = Vec::new();
        loop {
            let page = store
                .job_candidates(CandidateQuery {
                    node_id: "node-a".into(),
                    kinds: vec![JobKind::LibraryScan],
                    after: cursor,
                    now_ms: 3000,
                    limit: 1,
                })
                .await
                .expect("paged fair candidates");
            order.extend(page.jobs.into_iter().map(|job| job.id));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            order,
            vec![viewer.id, low.id, high_new_scope.id, high_old_scope.id],
            "{backend}: priority 3 stays first; stable cursor preserves the lower-class turn"
        );
    })
    .await;
}

fn preparation_request(file: &plurx_core::domain::MediaFile, id: &str, now_ms: i64) -> EnqueueJob {
    plurx_core::store::background_jobs_pretranscode::enqueue_request(
        &plurx_core::domain::NewPretranscodeJob {
            id: id.into(),
            dedupe_key: "a".repeat(64),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            target_height: 720,
            policy_generation: "policy:1".into(),
            requirements_json: serde_json::to_string(&PretranscodeRequirements {
                version: 1,
                decoder: "h264".into(),
                acceptable_encoder_families: vec!["software".into()],
                output_contract: "hls-v1".into(),
                tone_map: false,
                output_grade: "sdr".into(),
                scratch_bytes: 1024,
            })
            .expect("requirements"),
            reason: "recent".into(),
            priority: 0,
            not_before_ms: now_ms,
            created_at_ms: now_ms,
        },
    )
    .expect("request")
}

#[tokio::test]
async fn background_evicted_transcode_gets_one_new_interest_without_resetting_failures() {
    for_each_backend(|store, backend| async move {
        use plurx_core::cluster::coordination::LeaseClaim;
        let unix_ms = || -> Result<i64, std::time::SystemTimeError> {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as i64)
        };
        let (_, file_id) = seed_file(&store, "background-eviction").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let now = unix_ms().expect("clock");
        let LeaseClaim::Acquired(mut lease) = store
            .acquire_lease("candidate:eviction", "node-a", now, now + 90_000)
            .await
            .expect("producer lease")
        else {
            panic!("{backend}: lease");
        };
        let input = preparation_request(&file, &uuid::Uuid::new_v4().to_string(), now);
        let successor = lease.publication_successor().expect("successor");
        assert!(matches!(
            store
                .enqueue_job_fenced(input.clone(), lease, successor.clone())
                .await
                .expect("admission"),
            EnqueueOutcome::Accepted { .. }
        ));
        lease = successor;
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: input.id.clone(),
                expected_revision: 0,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::TranscodePrepare,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("claim")
        else {
            panic!("{backend}: claim");
        };
        let published = store
            .publish_transcode_job(PublishTranscodeJob {
                token: job.token.expect("token"),
                output: TranscodeJobOutput {
                    recipe_hash: "b".repeat(64),
                    recipe_version: 1,
                    relative_dir: "first".into(),
                    bytes: 100,
                    expected_previous_bytes: None,
                    manifest_digest: "d".repeat(64),
                },
                now_ms: now + 1,
            })
            .await
            .expect("publish");
        assert!(matches!(published, JobPublishOutcome::Published { .. }));
        let mut next = input.clone();
        next.id = uuid::Uuid::new_v4().to_string();
        let successor = lease.publication_successor().expect("successor");
        assert!(matches!(
            store
                .enqueue_job_fenced(next.clone(), lease, successor.clone())
                .await
                .expect("cached"),
            EnqueueOutcome::Existing { .. }
        ));
        lease = successor;
        store
            .forget_cache_entry(&"b".repeat(64), "node-a", "local")
            .await
            .expect("evict");
        let successor = lease.publication_successor().expect("successor");
        let outcome = store
            .enqueue_job_fenced(next, lease, successor.clone())
            .await
            .expect("repair demand");
        let EnqueueOutcome::Accepted { job_id, .. } = outcome else {
            panic!("{backend}: expected repair {outcome:?}");
        };
        lease = successor;
        assert_ne!(job_id, input.id);
        let interests = store
            .job_waiters(WaiterQuery {
                job_id: job_id.clone(),
                after: None,
                limit: 100,
            })
            .await
            .expect("repair interest");
        assert_eq!(interests.waiters.len(), 1);
        assert_eq!(interests.waiters[0].consumer_kind, "transcode_cache_repair");
        assert_eq!(interests.waiters[0].consumer_ref, input.id);
        // Failed or cancelled repair remains terminal across ordinary ticks.
        store
            .cancel_job(CancelJob {
                job_id: job_id.clone(),
                now_ms: unix_ms().expect("clock"),
            })
            .await
            .expect("cancel repair");
        for _ in 0..2 {
            let mut repeated = input.clone();
            repeated.id = uuid::Uuid::new_v4().to_string();
            let successor = lease.publication_successor().expect("successor");
            assert!(matches!(
                store
                    .enqueue_job_fenced(repeated, lease, successor.clone())
                    .await
                    .expect("repeat tick"),
                EnqueueOutcome::Existing { .. }
            ));
            lease = successor;
        }
        assert_eq!(
            store
                .list_jobs(JobQuery {
                    state: None,
                    kind: Some(JobKind::TranscodePrepare),
                    after_id: None,
                    limit: 100
                })
                .await
                .expect("jobs")
                .jobs
                .len(),
            2
        );
        assert_eq!(
            store
                .background_job(&input.id)
                .await
                .expect("original")
                .expect("original")
                .state,
            JobState::Succeeded
        );
    })
    .await;
}

#[tokio::test]
async fn background_offline_join_preserves_recipe_authority_and_independent_interests() {
    for_each_backend(|store, backend| async move {
        let (user_id, file_id) = seed_file(&store, "background-offline-join").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let input = preparation_request(&file, &uuid::Uuid::new_v4().to_string(), now);
        store.enqueue_job(input.clone()).await.expect("speculation");
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: input.id.clone(),
                expected_revision: 0,
                node_id: "offline-node".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::TranscodePrepare,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("claim")
        else {
            panic!("{backend}: claim");
        };
        let token = job.token.expect("token");
        let recipe = "b".repeat(64);
        assert!(store
            .bind_transcode_job_recipe(token.clone(), &recipe, now + 1)
            .await
            .expect("bind"));
        let mut stale = token.clone();
        stale.revision += 1;
        assert!(!store
            .bind_transcode_job_recipe(stale, &recipe, now + 1)
            .await
            .expect("stale binding"));
        let mut packages = Vec::new();
        for ordinal in 0..2 {
            let mut request = super::offline_request(
                &format!("joined-{ordinal}"),
                &format!("joined-request-{ordinal}"),
                user_id,
                file_id,
            );
            request.expires_at = now / 1000 + 3600;
            store
                .create_offline_package(&request, 10, 100_000, 100_000)
                .await
                .expect("quota admission");
            let package = store
                .claim_next_offline_package("offline-node")
                .await
                .expect("claim package")
                .expect("package");
            assert!(store
                .set_offline_package_recipe(
                    &package.id,
                    &package.node_id,
                    package.claim_generation,
                    &recipe
                )
                .await
                .expect("package recipe"));
            let join = JoinOfflineJob {
                package_id: package.id.clone(),
                node_id: package.node_id.clone(),
                claim_generation: package.claim_generation,
                recipe_hash: recipe.clone(),
                now_ms: now + 2,
            };
            for mismatch in 0..3 {
                let mut changed = join.clone();
                match mismatch {
                    0 => changed.recipe_hash = "c".repeat(64),
                    1 => changed.node_id = "other-node".into(),
                    _ => changed.claim_generation += 1,
                }
                assert!(
                    store
                        .join_offline_job(changed)
                        .await
                        .expect("mismatch")
                        .is_none(),
                    "{backend}"
                );
            }
            for _ in 0..2 {
                let outcome = store
                    .join_offline_job(join.clone())
                    .await
                    .expect("join")
                    .expect("match");
                match outcome {
                    EnqueueOutcome::Accepted { job_id, .. }
                    | EnqueueOutcome::Existing {
                        job_id,
                        cancelled: false,
                        ..
                    } => assert_eq!(job_id, input.id),
                    other => panic!("{backend}: {other:?}"),
                }
            }
            packages.push(package);
        }
        let promoted = store
            .background_job(&input.id)
            .await
            .expect("job")
            .expect("job");
        assert_eq!(promoted.priority, 2);
        assert_eq!(promoted.token.expect("unchanged authority"), token);
        assert!(!store
            .bind_transcode_job_recipe(token.clone(), &"c".repeat(64), now + 3)
            .await
            .expect("cannot replace joined recipe"));
        assert!(matches!(
            store
                .publish_transcode_job(PublishTranscodeJob {
                    token: token.clone(),
                    output: TranscodeJobOutput {
                        recipe_hash: "c".repeat(64),
                        recipe_version: 1,
                        relative_dir: "wrong-recipe".into(),
                        bytes: 100,
                        expected_previous_bytes: None,
                        manifest_digest: "d".repeat(64)
                    },
                    now_ms: now + 3,
                })
                .await
                .expect("wrong output"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(store
            .settle_job(SettleJob {
                token,
                settlement: JobSettlement::Yield {
                    checkpoint: Some(
                        serde_json::json!({"effective_recipe_hash": "c".repeat(64), "part": 2})
                    ),
                    not_before_ms: now + 4
                },
                now_ms: now + 4
            })
            .await
            .expect("yield"));
        let yielded = store
            .background_job(&input.id)
            .await
            .expect("job")
            .expect("job");
        assert_eq!(
            yielded.checkpoint.as_ref().expect("binding")["effective_recipe_hash"],
            recipe
        );
        let remote = || CandidateQuery {
            node_id: "other-node".into(),
            kinds: vec![JobKind::TranscodePrepare],
            after: None,
            now_ms: now + 5,
            limit: 10,
        };
        assert!(store
            .job_candidates(remote())
            .await
            .expect("affinity")
            .jobs
            .is_empty());
        for (ordinal, package) in packages.iter().enumerate() {
            assert!(store
                .delete_offline_package(&package.id, user_id)
                .await
                .expect("delete own package"));
            let job = store
                .background_job(&input.id)
                .await
                .expect("job")
                .expect("job");
            assert_eq!(
                job.state,
                JobState::Queued,
                "{backend}: automatic interest survives"
            );
            assert_eq!(job.priority, if ordinal == 0 { 2 } else { 0 });
            assert_eq!(
                store
                    .job_candidates(remote())
                    .await
                    .expect("remaining affinity")
                    .jobs
                    .len(),
                ordinal
            );
        }
        let waiters = store
            .job_waiters(WaiterQuery {
                job_id: input.id,
                after: None,
                limit: 10,
            })
            .await
            .expect("interests");
        assert_eq!(
            waiters
                .waiters
                .iter()
                .filter(|waiter| waiter.state == "pending")
                .count(),
            1
        );
        assert_eq!(
            waiters
                .waiters
                .iter()
                .filter(|waiter| waiter.consumer_kind == "offline_preparation"
                    && waiter.state == "cancelled")
                .count(),
            2
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_jobs_v70_backup_import_seals_legacy_work() {
    use sha2::{Digest, Sha256};
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("fresh target");
    let source = tempfile::tempdir().expect("source");
    let path = populated_current_import_fixture(source.path());
    {
        let connection = rusqlite::Connection::open(&path).expect("fixture");
        let objects = connection.prepare("SELECT type, name FROM sqlite_master WHERE name LIKE 'background_%' AND type IN ('trigger','table') ORDER BY CASE type WHEN 'trigger' THEN 0 ELSE 1 END")
            .expect("queue objects").query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .expect("objects").collect::<Result<Vec<_>, _>>().expect("names");
        for (kind, name) in objects {
            connection
                .execute_batch(&format!("DROP {} \"{}\"", kind, name.replace('"', "\"\"")))
                .expect("remove v71 shape");
        }
        connection.execute("INSERT INTO pretranscode_jobs
            (id, dedupe_key, file_id, source_size, source_mtime, target_height, policy_generation,
             requirements_json, reason, priority, state, not_before_ms, created_at_ms, updated_at_ms)
            SELECT ?1, 'v70-backup', id, size, mtime, 720, 'policy:1', ?2, 'recent', 0, 'queued', 0, 1, 1
            FROM files ORDER BY id LIMIT 1",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), serde_json::json!({"version":1,"decoder":"h264",
                "acceptable_encoder_families":["software"],"output_contract":"hls-v1","tone_map":false,
                "output_grade":"sdr","scratch_bytes":1024}).to_string()]).expect("accepted legacy work");
        connection
            .pragma_update(None, "user_version", 70)
            .expect("v70 marker");
    }
    // Read the old binary's prepared snapshot directly: opening through the
    // current SQLite store would migrate it and conceal the import boundary.
    let digest = hex::encode(Sha256::digest(
        std::fs::read(&path).expect("snapshot bytes"),
    ));
    let report = store
        .import_sqlite_backup(&path, &digest, 70)
        .await
        .expect("v70 import");
    assert_eq!(report.source_schema_version, 70);
    assert!(report
        .tables
        .iter()
        .filter(|table| table.table.starts_with("background_"))
        .all(|table| table.row_count == 0));
    let migration = store
        .job_migration_status()
        .await
        .expect("finite legacy seal");
    assert!(migration.accepted > 0);
    assert_eq!(migration.accepted, migration.awaiting_import);
    assert!(store
        .import_legacy_jobs(1000)
        .await
        .expect("materialize imported work"));
    assert!(
        store
            .job_migration_status()
            .await
            .expect("progress")
            .materialized
            > 0
    );
}

#[tokio::test]
async fn background_library_publication_requires_both_live_owners() {
    use super::{acquired, publication_successor};
    for_each_backend(|store, backend| async move {
        for scenario in ["live", "cancelled", "expired", "taken_over"] {
            let (_, file_id) = seed_file(&store, &format!("dual-owner-{scenario}")).await;
            let file = store
                .get_file(file_id)
                .await
                .expect("file read")
                .expect("file");
            let library_id = store
                .get_item(file.item_id)
                .await
                .expect("item read")
                .expect("item")
                .library_id;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis() as i64;
            let claim_time = if scenario == "expired" {
                now - 31_000
            } else {
                now
            };
            let id = uuid::Uuid::new_v4().to_string();
            let admitted = store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::LibraryScan {
                        library_id,
                        generation: "full-v1".into(),
                    },
                    dedupe_key: format!("scan:{library_id}"),
                    priority: 2,
                    not_before_ms: claim_time,
                    now_ms: claim_time,
                    request: JobRequest {
                        scope: "binding-fixture".into(),
                        request_id: id.clone(),
                        request_digest: "a".repeat(64),
                        consumer_kind: "library_scan".into(),
                        consumer_ref: library_id.to_string(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("admission");
            assert!(
                matches!(admitted, EnqueueOutcome::Accepted { .. }),
                "{backend}"
            );
            let ClaimOutcome::Claimed { job } = store
                .claim_job(ClaimJob {
                    job_id: id.clone(),
                    expected_revision: 0,
                    node_id: "node-a".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::LibraryScan,
                    payload_version: 1,
                    now_ms: claim_time,
                    dispatched_at_ms: claim_time,
                })
                .await
                .expect("claim")
            else {
                panic!("{backend}: claim refused");
            };
            let token = job.token.expect("token");
            let lease = acquired(
                store
                    .acquire_lease(
                        &format!("scan:library:{library_id}"),
                        "node-a",
                        now,
                        now + 90_000,
                    )
                    .await
                    .expect("domain lease"),
                backend,
            );
            let binding = BindLibraryJob {
                token: token.clone(),
                lease: lease.clone(),
                now_ms: claim_time,
            };
            assert!(
                store.bind_library_job(binding.clone()).await.expect("bind"),
                "{backend}"
            );
            assert!(
                store
                    .bind_library_job(binding)
                    .await
                    .expect("binding replay"),
                "{backend}"
            );
            if scenario == "cancelled" {
                store
                    .cancel_job(CancelJob {
                        job_id: id.clone(),
                        now_ms: now + 1,
                    })
                    .await
                    .expect("cancel");
            } else if scenario == "taken_over" {
                let takeover = store
                    .claim_job(ClaimJob {
                        job_id: id.clone(),
                        expected_revision: token.revision,
                        node_id: "node-b".into(),
                        boot_id: uuid::Uuid::new_v4().to_string(),
                        claim_id: uuid::Uuid::new_v4().to_string(),
                        kind: JobKind::LibraryScan,
                        payload_version: 1,
                        now_ms: token.lease_expires_ms + 1,
                        dispatched_at_ms: token.lease_expires_ms + 1,
                    })
                    .await
                    .expect("takeover");
                assert!(
                    matches!(takeover, ClaimOutcome::Claimed { .. }),
                    "{backend}"
                );
            }
            let replacement = publication_successor(&lease);
            let result = store
                .put_setting_fenced(
                    &format!("dual-owner:{scenario}"),
                    "published",
                    &lease,
                    &replacement,
                )
                .await;
            if scenario == "live" {
                result.expect("both owners live");
                // A domain publication advances its revision independently of
                // queue renewal. The stable binding must still authorize it.
                store
                    .put_setting_fenced(
                        &format!("dual-owner:{scenario}"),
                        "second",
                        &replacement,
                        &publication_successor(&replacement),
                    )
                    .await
                    .expect("domain revision advanced");
                store
                    .cancel_job(CancelJob {
                        job_id: id,
                        now_ms: now + 1,
                    })
                    .await
                    .expect("cleanup");
            } else {
                assert!(
                    matches!(
                        result,
                        Err(plurx_core::error::StoreError::FenceRejected { .. })
                    ),
                    "{backend}: {scenario}: {result:?}"
                );
                assert!(
                    store
                        .get_setting(&format!("dual-owner:{scenario}"))
                        .await
                        .expect("read")
                        .is_none(),
                    "{backend}"
                );
            }
            // Release expired reservations between scenarios without rewriting
            // or removing the binding: retirement must not restore authority.
            store.maintain_jobs(now + 120_000).await.expect("upkeep");
        }
    })
    .await;
}

#[tokio::test]
async fn background_library_intents_preserve_hints_and_do_not_settle_late_arrivals() {
    use super::acquired;
    use plurx_core::store::background_jobs_library::{
        LibraryIdHints, LibraryWorkInput, LibraryWorkResult,
    };
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "durable-library-intents").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let library_id = store.get_item(file.item_id).await.expect("item").expect("item").library_id;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_millis() as i64;
        let input = NewLibraryWork {
            request_id: "scan-request-one".into(), library_id, now_ms: now,
            input: LibraryWorkInput::Targeted {
                path: file.path.clone(), ids: Some(LibraryIdHints { tmdb: Some(123), ..Default::default() }),
                book: None, correlation_id: Some("caller-one".into()), source: Some("integration".into()),
            },
        };
        let EnqueueOutcome::Accepted { job_id, .. } = store.enqueue_library_work(input.clone()).await.expect("enqueue")
            else { panic!("{backend}: first admission refused"); };
        assert!(matches!(store.enqueue_library_work(input.clone()).await.expect("replay"), EnqueueOutcome::Existing { .. }), "{backend}");
        let mut different = input.clone();
        different.input = LibraryWorkInput::Targeted { path: file.path.clone(),
            ids: Some(LibraryIdHints { tmdb: Some(456), ..Default::default() }), book: None,
            correlation_id: Some("caller-two".into()), source: Some("integration".into()) };
        assert!(matches!(store.enqueue_library_work(different.clone()).await.expect("identity conflict"), EnqueueOutcome::Conflict), "{backend}");
        different.request_id = "scan-request-two".into();
        let ClaimOutcome::Claimed { job } = store.claim_job(ClaimJob {
            job_id: job_id.clone(), expected_revision: 0, node_id: "node-a".into(),
            boot_id: uuid::Uuid::new_v4().to_string(), claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::LibraryScan, payload_version: 1, now_ms: now, dispatched_at_ms: now,
        }).await.expect("claim") else { panic!("{backend}: claim refused"); };
        let token = job.token.expect("token");
        let lease = acquired(store.acquire_lease(&format!("scan:library:{library_id}"), "node-a", now, now + 90_000)
            .await.expect("domain lease"), backend);
        assert!(store.bind_library_job(BindLibraryJob { token: token.clone(), lease: lease.clone(), now_ms: now })
            .await.expect("bind"), "{backend}");
        // Arrives after the worker took its first snapshot, with a distinct
        // hint for the same path. It joins the computation but owes its own work.
        assert!(matches!(store.enqueue_library_work(different).await.expect("late join"),
            EnqueueOutcome::Accepted { job_id: joined, .. } if joined == job_id), "{backend}");
        let completion = CompleteLibraryWork { token: token.clone(), lease: lease.clone(),
            request_id: input.request_id.clone(), now_ms: now + 1,
            result: LibraryWorkResult::Completed { scan: plurx_core::scan::TargetedScan {
                report: plurx_core::scan::ScanReport::default(), items: vec![] } },
        };
        assert!(store.complete_library_work(completion.clone()).await.expect("first completion"), "{backend}");
        assert!(!store.complete_library_work(completion.clone()).await.expect("completion replay"), "{backend}");
        assert_eq!(store.background_job(&job_id).await.expect("job").expect("job").state, JobState::Running, "{backend}");
        let pending = store.library_work_requests(LibraryWorkQuery {
            job_id: Some(job_id.clone()), pending_only: true, limit: 256, ..Default::default()
        }).await.expect("pending");
        assert_eq!(pending.len(), 1, "{backend}");
        assert_eq!(pending[0].request_id, "scan-request-two", "{backend}");
        assert!(matches!(&pending[0].input, LibraryWorkInput::Targeted { ids: Some(ids), .. } if ids.tmdb == Some(456)), "{backend}");
        let second = CompleteLibraryWork { request_id: "scan-request-two".into(), ..completion };
        assert!(store.complete_library_work(second).await.expect("second completion"), "{backend}");
        assert_eq!(store.background_job(&job_id).await.expect("job").expect("job").state, JobState::Succeeded, "{backend}");
        let results = store.library_work_requests(LibraryWorkQuery { job_id: Some(job_id), limit: 256, ..Default::default() })
            .await.expect("durable results");
        assert_eq!(results.len(), 2, "{backend}");
        assert!(results.iter().all(|row| row.state == "completed" && row.result.is_some()), "{backend}");
    }).await;
}

#[tokio::test]
async fn background_library_accepted_intent_survives_database_reopen() {
    use plurx_core::store::background_jobs_library::{LibraryTrigger, LibraryWorkInput};
    use plurx_core::store::{SqliteStore, Store};
    use std::sync::Arc;
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("durable-library.sqlite");
    let original: Arc<dyn Store> = Arc::new(SqliteStore::open(&path).expect("open"));
    let (_, file_id) = seed_file(&original, "library-restart").await;
    let file = original
        .get_file(file_id)
        .await
        .expect("file")
        .expect("file");
    let library_id = original
        .get_item(file.item_id)
        .await
        .expect("item")
        .expect("item")
        .library_id;
    let input = NewLibraryWork {
        request_id: "restart-request".into(),
        library_id,
        input: LibraryWorkInput::Full {
            refresh: true,
            trigger: LibraryTrigger::Manual,
        },
        now_ms: 1_000,
    };
    let EnqueueOutcome::Accepted { job_id, .. } = original
        .enqueue_library_work(input.clone())
        .await
        .expect("admission")
    else {
        panic!("admission refused");
    };
    drop(original);
    let reopened = SqliteStore::open(&path).expect("reopen");
    let rows = reopened
        .library_work_requests(LibraryWorkQuery {
            request_id: Some(input.request_id.clone()),
            limit: 1,
            ..Default::default()
        })
        .await
        .expect("persisted request");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job_id, job_id);
    assert_eq!(rows[0].state, "pending");
    assert!(matches!(
        rows[0].input,
        LibraryWorkInput::Full { refresh: true, .. }
    ));
    assert!(matches!(
        reopened
            .enqueue_library_work(input)
            .await
            .expect("replay after restart"),
        EnqueueOutcome::Existing { .. }
    ));
}

#[tokio::test]
async fn background_storage_aliases_share_capacity_and_contention_is_atomic() {
    use plurx_core::store::background_jobs_resources::StorageDomainMapping;
    for_each_backend(|store, backend| async move {
        let mut files = Vec::new();
        let mut mappings = Vec::new();
        for (index, domain) in ["nas", "nas", "nas", "other"].into_iter().enumerate() {
            let prefix = format!("storage-domain-{index}");
            let (_, file_id) = seed_file(&store, &prefix).await;
            let file = store.get_file(file_id).await.expect("file").expect("file");
            let item = store
                .get_item(file.item_id)
                .await
                .expect("item")
                .expect("item");
            mappings.push(StorageDomainMapping {
                library_id: item.library_id,
                root_path: format!("/{prefix}"),
                domain_id: domain.into(),
            });
            files.push(file_id);
        }
        assert!(
            store
                .replace_storage_domains(mappings.clone(), 1_000)
                .await
                .expect("map"),
            "{backend}"
        );
        assert_eq!(
            store.storage_domains().await.expect("mappings"),
            mappings,
            "{backend}"
        );
        let mut claims = Vec::new();
        for (index, file_id) in files.into_iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::MediaProbe {
                        file_id,
                        source_generation: "source:1".into(),
                        probe_digest: "a".repeat(64),
                    },
                    dedupe_key: format!("storage-probe:{index}"),
                    priority: 1,
                    not_before_ms: 1_000,
                    now_ms: 1_000,
                    request: JobRequest {
                        scope: "storage-test".into(),
                        request_id: id.clone(),
                        request_digest: "b".repeat(64),
                        consumer_kind: "probe".into(),
                        consumer_ref: id.clone(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("enqueue");
            let claim = ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: format!("node-{index}"),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MediaProbe,
                payload_version: 1,
                now_ms: 1_000,
                dispatched_at_ms: 1_000,
            };
            let outcome = store.claim_job(claim.clone()).await.expect("claim");
            assert_eq!(
                matches!(outcome, ClaimOutcome::Claimed { .. }),
                index != 2,
                "{backend}: alias {index}"
            );
            if index == 2 {
                assert!(
                    store.job_attempts(&id).await.expect("attempts").is_empty(),
                    "{backend}: contention must not create an attempt"
                );
            }
            claims.push(claim);
        }
        assert!(
            !store
                .replace_storage_domains(vec![], 1_001)
                .await
                .expect("busy remap"),
            "{backend}"
        );
        assert_eq!(
            store.storage_domains().await.expect("unchanged"),
            mappings,
            "{backend}"
        );
        let job = store
            .background_job(&claims[0].job_id)
            .await
            .expect("job")
            .expect("job");
        store
            .settle_job(SettleJob {
                token: job.token.expect("token"),
                now_ms: 1_002,
                settlement: JobSettlement::Yield {
                    checkpoint: None,
                    not_before_ms: 5_000,
                },
            })
            .await
            .expect("yield");
        assert!(
            matches!(
                store
                    .claim_job(ClaimJob {
                        now_ms: 1_003,
                        dispatched_at_ms: 1_003,
                        ..claims[2].clone()
                    })
                    .await
                    .expect("freed alias slot"),
                ClaimOutcome::Claimed { .. }
            ),
            "{backend}"
        );
        assert!(
            store
                .replace_storage_domains(vec![], 100_000)
                .await
                .expect("expired owners no longer block remap"),
            "{backend}"
        );
        assert!(store.storage_domains().await.expect("cleared").is_empty());
    })
    .await;
}

#[tokio::test]
async fn background_provider_contention_does_not_reserve_independent_storage() {
    use plurx_core::store::background_jobs_library::{LibraryTrigger, LibraryWorkInput};
    use plurx_core::store::background_jobs_resources::StorageDomainMapping;
    for_each_backend(|store, backend| async move {
        let mut libraries = Vec::new();
        let mut files = Vec::new();
        let mut mappings = Vec::new();
        for index in 0..3 {
            let prefix = format!("provider-slot-{index}");
            let (_, file_id) = seed_file(&store, &prefix).await;
            let file = store.get_file(file_id).await.expect("file").expect("file");
            let library_id = store
                .get_item(file.item_id)
                .await
                .expect("item")
                .expect("item")
                .library_id;
            mappings.push(StorageDomainMapping {
                library_id,
                root_path: format!("/{prefix}"),
                domain_id: prefix,
            });
            libraries.push(library_id);
            files.push(file_id);
        }
        assert!(store
            .replace_storage_domains(mappings, 1_000)
            .await
            .expect("map"));
        for (index, library_id) in libraries.into_iter().enumerate() {
            let request_id = format!("provider-work-{index}");
            let EnqueueOutcome::Accepted { job_id, .. } = store
                .enqueue_library_work(NewLibraryWork {
                    request_id,
                    library_id,
                    now_ms: 1_000,
                    input: LibraryWorkInput::Full {
                        refresh: true,
                        trigger: LibraryTrigger::Manual,
                    },
                })
                .await
                .expect("enqueue")
            else {
                panic!("{backend}: refused");
            };
            let result = store
                .claim_job(ClaimJob {
                    job_id,
                    expected_revision: 0,
                    node_id: format!("node-{index}"),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::MetadataRefresh,
                    payload_version: 1,
                    now_ms: 1_000,
                    dispatched_at_ms: 1_000,
                })
                .await
                .expect("claim");
            assert_eq!(
                matches!(result, ClaimOutcome::Claimed { .. }),
                index < 2,
                "{backend}"
            );
        }
        // Both independent storage slots must remain available after the third
        // metadata claim lost provider contention in its all-or-none transaction.
        for index in 0..2 {
            let id = uuid::Uuid::new_v4().to_string();
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::MediaProbe {
                        file_id: files[2],
                        source_generation: "source:1".into(),
                        probe_digest: "c".repeat(64),
                    },
                    dedupe_key: format!("provider-probe:{index}"),
                    priority: 1,
                    not_before_ms: 1_000,
                    now_ms: 1_000,
                    request: JobRequest {
                        scope: "provider-test".into(),
                        request_id: id.clone(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "probe".into(),
                        consumer_ref: id.clone(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("probe enqueue");
            assert!(
                matches!(
                    store
                        .claim_job(ClaimJob {
                            job_id: id,
                            expected_revision: 0,
                            node_id: format!("probe-{index}"),
                            boot_id: uuid::Uuid::new_v4().to_string(),
                            claim_id: uuid::Uuid::new_v4().to_string(),
                            kind: JobKind::MediaProbe,
                            payload_version: 1,
                            now_ms: 1_000,
                            dispatched_at_ms: 1_000
                        })
                        .await
                        .expect("probe claim"),
                    ClaimOutcome::Claimed { .. }
                ),
                "{backend}"
            );
        }
    })
    .await;
}
