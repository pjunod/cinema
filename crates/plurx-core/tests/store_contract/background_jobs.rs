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
async fn background_transcode_copies_preserve_source_proof_and_settle_only_the_receiving_target() {
    for_each_backend(|store, backend| async move {
        use plurx_core::store::background_jobs_transcode::artifact_key;
        let (_, file_id) = seed_file(&store, "transcode-copies").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let id = uuid::Uuid::new_v4().to_string();
        let recipe = "a".repeat(64);
        let manifest = "d".repeat(64);
        let long_target = "z".repeat(256);
        for target in ["node-a", "node-b", long_target.as_str()] {
            let mut input = preparation_request(&file, &id, 1_000);
            input.id = uuid::Uuid::new_v4().to_string();
            input.request.request_id = uuid::Uuid::new_v4().to_string();
            input.request.target_node_id = Some(target.into());
            assert!(matches!(
                store.enqueue_job(input).await.expect("demand"),
                EnqueueOutcome::Accepted { .. }
            ));
        }
        let candidate = store
            .job_candidates(CandidateQuery {
                node_id: "node-a".into(),
                kinds: vec![JobKind::TranscodePrepare],
                after: None,
                now_ms: 1_001,
                limit: 8,
            })
            .await
            .expect("candidate")
            .jobs
            .remove(0);
        let parent = candidate.id.clone();
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: parent.clone(),
                expected_revision: candidate.revision,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::TranscodePrepare,
                payload_version: 1,
                now_ms: 1_001,
                dispatched_at_ms: 1_001,
            })
            .await
            .expect("producer")
        else {
            panic!("{backend}: producer not claimed")
        };
        let output = TranscodeJobOutput {
            recipe_hash: recipe.clone(),
            recipe_version: 1,
            relative_dir: "aa/original".into(),
            bytes: 100,
            expected_previous_bytes: None,
            manifest_digest: manifest.clone(),
        };
        assert!(matches!(
            store
                .publish_transcode_job(PublishTranscodeJob {
                    token: job.token.expect("token"),
                    output: output.clone(),
                    now_ms: 1_002,
                })
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        let key = artifact_key(&recipe, &manifest);
        let holders = store.transcode_copy_sources(&key).await.expect("holders");
        assert_eq!(holders.len(), 1, "{backend}");
        assert_eq!(holders[0].source_size, file.size);
        let intents = store.delivery_intents(1_003).await.expect("outbox");
        assert_eq!(intents.len(), 2, "{backend}: two receiving nodes");
        for intent in intents {
            assert_eq!(intent.artifact_key, key);
            let target = intent.target_node_id.clone();
            let (payload, digest) = plurx_core::store::background_jobs::hydration_identity(&key, &target).expect("shared identity");
            let hot_request_id = uuid::Uuid::new_v4().to_string();
            let hot = EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(), payload,
                dedupe_key: format!("hydrate:{digest}"), priority: 0,
                not_before_ms: 1_003, now_ms: 1_003,
                request: JobRequest {
                    scope: "automatic:hot-copy".into(), request_id: hot_request_id.clone(),
                    request_digest: digest, consumer_kind: "hot_copy".into(), consumer_ref: key.clone(),
                    target_node_id: Some(target.clone()), deadline_ms: Some(1_000_000), retain_identity: false,
                },
            };
            let before = if target == "node-b" { Some(store.enqueue_job(hot.clone()).await.expect("hot first")) } else { None };
            let EnqueueOutcome::Accepted { job_id, .. } = store
                .enqueue_delivery(intent, 1_003)
                .await
                .expect("admit delivery")
            else {
                panic!("{backend}: delivery not admitted")
            };
            let hot_outcome = match before {
                Some(outcome) => outcome,
                None => store.enqueue_job(hot).await.expect("hot second"),
            };
            let EnqueueOutcome::Accepted { job_id: hot_job, .. } = hot_outcome else { panic!("{backend}: hot interest refused") };
            assert_eq!(hot_job, job_id, "{backend}: both admission orders share one hydration job, including a 256-byte target");
            store.cancel_waiter(CancelWaiter {
                scope: "automatic:hot-copy".into(), request_id: hot_request_id, now_ms: 1_003,
            }).await.expect("cancel only forecast");
            let waiters = store.job_waiters(WaiterQuery { job_id: job_id.clone(), after: None, limit: 8 }).await.expect("independent interests");
            assert_eq!(waiters.waiters.len(), 2);
            assert!(waiters.waiters.iter().any(|waiter| waiter.scope.starts_with("delivery:") && waiter.state == "pending"));
            assert!(waiters.waiters.iter().any(|waiter| waiter.scope == "automatic:hot-copy" && waiter.state == "cancelled"));
            let copy = store
                .background_job(&job_id)
                .await
                .expect("copy")
                .expect("copy");
            let ClaimOutcome::Claimed { job } = store
                .claim_job(ClaimJob {
                    job_id,
                    expected_revision: copy.revision,
                    node_id: target.clone(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::ArtifactHydrate,
                    payload_version: 1,
                    now_ms: 1_004,
                    dispatched_at_ms: 1_004,
                })
                .await
                .expect("copy claim")
            else {
                panic!("{backend}: copy not claimed")
            };
            assert!(
                store
                    .background_staging_jobs(&target)
                    .await
                    .expect("copy staging inventory")
                    .contains(&job.id),
                "{backend}: hydration owns cache bytes"
            );
            let mut publication = PublishTranscodeJob {
                token: job.token.expect("copy token"),
                output: output.clone(),
                now_ms: 1_005,
            };
            publication.output.relative_dir = format!("aa/copy-{target}");
            if target == "node-b" {
                let mut stale = publication.clone();
                stale.token.fence += 1;
                assert!(matches!(
                    store.publish_transcode_job(stale).await.expect("stale"),
                    JobPublishOutcome::LostOwnership
                ));
                let mut wrong_manifest = publication.clone();
                wrong_manifest.output.manifest_digest = "e".repeat(64);
                assert!(matches!(
                    store
                        .publish_transcode_job(wrong_manifest)
                        .await
                        .expect("wrong manifest"),
                    JobPublishOutcome::LostOwnership
                ));
                assert!(matches!(
                    store
                        .publish_transcode_job(publication.clone())
                        .await
                        .expect("copy publish"),
                    JobPublishOutcome::Published { .. }
                ));
                assert!(matches!(
                    store
                        .publish_transcode_job(publication)
                        .await
                        .expect("ack replay"),
                    JobPublishOutcome::AlreadyPublished { .. }
                ));
                let holders = store
                    .transcode_copy_sources(&key)
                    .await
                    .expect("verified holders");
                assert_eq!(holders.len(), 2);
                assert!(holders.iter().all(
                    |holder| holder.built_by_node_id == "node-a" && holder.built_at_ms == 1_002
                ));
                let waiters = store
                    .job_waiters(WaiterQuery {
                        job_id: parent.clone(),
                        after: None,
                        limit: 8,
                    })
                    .await
                    .expect("waiters");
                assert_eq!(
                    waiters
                        .waiters
                        .iter()
                        .find(|waiter| waiter.target_node_id.as_deref() == Some("node-b"))
                        .expect("B")
                        .state,
                    "succeeded"
                );
                assert_eq!(
                    waiters
                        .waiters
                        .iter()
                        .find(|waiter| waiter.target_node_id.as_deref() == Some(long_target.as_str()))
                        .expect("C")
                        .state,
                    "awaiting_hydration"
                );
            } else {
                store
                    .upsert_file(
                        file.item_id,
                        file.path.to_str().expect("path"),
                        file.size + 1,
                        file.mtime + 1,
                        &Default::default(),
                    )
                    .await
                    .expect("replace source");
                assert!(matches!(
                    store
                        .publish_transcode_job(publication)
                        .await
                        .expect("stale source"),
                    JobPublishOutcome::SourceChanged
                ));
                assert!(store
                    .transcode_copy_sources(&key)
                    .await
                    .expect("current holders")
                    .is_empty());
                assert!(store
                    .cache_hit(&recipe, &target)
                    .await
                    .expect("C cache")
                    .is_none());
            }
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
    use plurx_core::store::ClusterFragmentIndexStore;
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
        connection.execute("INSERT INTO analysis_requests
            (request_id, file_id, source_size, source_mtime, component, pipeline_version, force_rebuild, target_node_id,
             state, owner_node_id, lease_expires_ms, fence, attempts, not_before_ms, created_at_ms, updated_at_ms)
            SELECT 'legacy-running-subtitle', id, size, mtime, 'subtitle_source', 'subtitle-source-v1', 0, '',
             'running', 'dead-owner', 999999, 7, 2, 0, 1, 1 FROM files ORDER BY id LIMIT 1", [])
            .expect("legacy running subtitle");
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
    let recovered = store
        .analysis_request("legacy-running-subtitle")
        .await
        .expect("cutover")
        .expect("legacy subtitle");
    assert_eq!(recovered.state, "queued");
    assert!(recovered.owner_node_id.is_empty());
    assert_eq!(recovered.fence, 8);
    assert_eq!(recovered.attempts, 2);

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
    let admitted = store
        .enqueue_subtitle_job(recovered, 1000)
        .await
        .expect("recovered admission");
    let EnqueueOutcome::Accepted { job_id, .. } = admitted else {
        panic!("legacy subtitle must be admitted: {admitted:?}");
    };
    assert_eq!(
        store
            .background_job(&job_id)
            .await
            .expect("common job")
            .expect("job")
            .failed_attempts,
        2
    );
}

#[tokio::test]
async fn background_library_publication_requires_both_live_owners() {
    use super::{acquired, publication_successor};
    for_each_backend(|store, backend| async move {
        for scenario in ["live", "cancelled", "expired", "taken_over", "roots_changed"] {
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
            if scenario == "roots_changed" {
                let library = store.get_library(library_id).await.expect("library").expect("library");
                assert!(store.replace_storage_domains(vec![plurx_core::store::background_jobs_resources::StorageDomainMapping {
                    library_id, root_path: library.paths[0].to_string_lossy().into_owned(), domain_id: "old-nas".into(),
                }], now).await.expect("map original storage"));
            }
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
            if scenario == "roots_changed" {
                let library = store.get_library(library_id).await.expect("library").expect("library");
                store.update_library(library_id, &plurx_core::domain::NewLibrary {
                    name: library.name, kind: library.kind, paths: vec![std::path::PathBuf::from("/new-unmapped-storage")], anime: library.anime,
                }).await.expect("change library root");
                let renewed = store.renew_jobs(RenewJobs { tokens: vec![token.clone()], now_ms: now }).await.expect("renewal verdict");
                assert!(matches!(renewed.as_slice(), [RenewOutcome::LostOwnership { .. }]), "{backend}: old slots cannot renew new storage work");
                assert!(matches!(store.resolve_claim(ResolveClaim { job_id: id.clone(), node_id: token.node_id.clone(),
                    boot_id: token.boot_id.clone(), claim_id: token.claim_id.clone(), fence: Some(token.fence),
                    dispatched_at_ms: now, now_ms: now }).await.expect("reconcile refusal"), ClaimResolution::LostOwnership),
                    "{backend}: acknowledgement recovery cannot restore invalid resource ownership");

            }
            let allowance = store.update_provider_budget(plurx_core::store::background_jobs_provider::ProviderBudgetRequest {
                provider: plurx_core::metadata::Provider::Tmdb,
                lease: lease.clone(), now_ms: now,
                action: plurx_core::store::background_jobs_provider::ProviderBudgetAction::Observe { cooldown_ms: 0, interval_ms: None },
            }).await.expect("joint provider authority");
            assert_eq!(allowance, if scenario == "live" {
                plurx_core::store::background_jobs_provider::ProviderBudgetOutcome::Observed
            } else {
                plurx_core::store::background_jobs_provider::ProviderBudgetOutcome::LostAuthority
            }, "{backend}: {scenario}: provider authority follows both owners");
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
                        job_id: id.clone(),
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
            store.cancel_job(CancelJob { job_id: id, now_ms: now + 120_000 }).await.expect("retire scenario owner");
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
        assert!(results.iter().all(|row| row.state == "succeeded" && row.result.is_some()), "{backend}");
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
                    payload: probe_fixture(&store, file_id).await,
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
                    payload: probe_fixture(&store, files[2]).await,
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

#[tokio::test]
async fn background_provider_budget_is_shared_charged_once_and_keeps_cooldowns() {
    use super::acquired;
    use plurx_core::metadata::Provider;
    use plurx_core::store::background_jobs_provider::{
        ProviderBudgetAction as Action, ProviderBudgetOutcome as Outcome,
        ProviderBudgetRequest as Request,
    };
    for_each_backend(|store, backend| async move {
        let artwork = acquired(
            store
                .acquire_lease("provider:artwork", "node-a", 1_000, 200_000)
                .await
                .expect("artwork lease"),
            backend,
        );
        let genres = acquired(
            store
                .acquire_lease("provider:genres", "node-b", 1_000, 200_000)
                .await
                .expect("genre lease"),
            backend,
        );
        let first = Request {
            provider: Provider::Tmdb,
            lease: artwork.clone(),
            now_ms: 1_000,
            action: Action::Charge,
        };
        assert_eq!(
            store
                .update_provider_budget(first.clone())
                .await
                .expect("dispatch"),
            Outcome::Charged,
            "{backend}"
        );
        // Replaying a lost acknowledgement never refunds the spent dispatch.
        assert_eq!(
            store
                .update_provider_budget(first)
                .await
                .expect("lost reply retry"),
            Outcome::Wait { until_ms: 1_100 },
            "{backend}"
        );
        let second = Request {
            provider: Provider::Tmdb,
            lease: genres,
            now_ms: 1_099,
            action: Action::Charge,
        };
        assert_eq!(
            store
                .update_provider_budget(second.clone())
                .await
                .expect("other node"),
            Outcome::Wait { until_ms: 1_100 },
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    now_ms: 1_100,
                    ..second.clone()
                })
                .await
                .expect("refill"),
            Outcome::Charged,
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    action: Action::Observe {
                        cooldown_ms: 60_000,
                        interval_ms: Some(4_001)
                    },
                    now_ms: 1_101,
                    ..second.clone()
                })
                .await
                .expect("429 cooldown"),
            Outcome::Observed,
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    lease: artwork.clone(),
                    now_ms: 2_000,
                    ..second.clone()
                })
                .await
                .expect("shared cooldown"),
            Outcome::Wait { until_ms: 61_101 },
            "{backend}"
        );
        // An unrelated later response cannot shorten the server's cooldown.
        store
            .update_provider_budget(Request {
                action: Action::Observe {
                    cooldown_ms: 0,
                    interval_ms: Some(100),
                },
                now_ms: 3_000,
                ..second.clone()
            })
            .await
            .expect("response");
        assert_eq!(
            store
                .update_provider_budget(Request {
                    now_ms: 61_100,
                    ..second.clone()
                })
                .await
                .expect("cooldown retained"),
            Outcome::Wait { until_ms: 61_101 },
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    now_ms: 61_101,
                    ..second.clone()
                })
                .await
                .expect("cooldown elapsed"),
            Outcome::Charged,
            "{backend}"
        );
        // AniList has a separate allowance with conservative non-burst spacing.
        assert_eq!(
            store
                .update_provider_budget(Request {
                    provider: Provider::AniList,
                    now_ms: 1_000,
                    ..second.clone()
                })
                .await
                .expect("anilist"),
            Outcome::Charged,
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    provider: Provider::AniList,
                    now_ms: 1_001,
                    ..second.clone()
                })
                .await
                .expect("anilist spacing"),
            Outcome::Wait { until_ms: 3_100 },
            "{backend}"
        );
        assert_eq!(
            store
                .update_provider_budget(Request {
                    now_ms: 200_001,
                    ..second
                })
                .await
                .expect("expired owner"),
            Outcome::LostAuthority,
            "{backend}"
        );
        assert!(
            store
                .update_provider_budget(Request {
                    provider: Provider::Tmdb,
                    lease: plurx_core::cluster::coordination::Lease {
                        resource: "catalogue:unrelated".into(),
                        ..artwork
                    },
                    now_ms: 300_000,
                    action: Action::Charge
                })
                .await
                .is_err(),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn background_artifact_claim_cannot_adopt_catalogue_or_provider_authority() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "artifact-only-claim").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let library_id = store
            .get_item(file.item_id)
            .await
            .expect("item")
            .expect("item")
            .library_id;
        for (index, payload) in [
            JobPayload::LibraryScan {
                library_id,
                generation: "library-work-v1".into(),
            },
            JobPayload::MetadataRefresh {
                library_id,
                generation: "library-work-v1".into(),
            },
            probe_fixture(&store, file_id).await,
        ]
        .into_iter()
        .enumerate()
        {
            let kind = payload.kind();
            let id = uuid::Uuid::new_v4().to_string();
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload,
                    dedupe_key: format!("artifact-scope:{index}"),
                    priority: 1,
                    not_before_ms: 1_000,
                    now_ms: 1_000,
                    request: JobRequest {
                        scope: "artifact-scope".into(),
                        request_id: id.clone(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "test".into(),
                        consumer_ref: id.clone(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("enqueue");
            let request = ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "artifact-worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind,
                payload_version: 1,
                now_ms: 1_000,
                dispatched_at_ms: 1_000,
            };
            if kind.permits_artifact_execution() {
                assert!(
                    matches!(
                        store
                            .claim_artifact_job(request)
                            .await
                            .expect("artifact scope"),
                        ClaimOutcome::Claimed { .. }
                    ),
                    "{backend}"
                );
            } else {
                assert!(
                    store.claim_artifact_job(request.clone()).await.is_err(),
                    "{backend}: {kind:?}"
                );
                assert!(
                    store
                        .job_attempts(&id)
                        .await
                        .expect("no attempt")
                        .is_empty(),
                    "{backend}"
                );
                assert!(matches!(
                    store
                        .claim_job(request.clone())
                        .await
                        .expect("voter coordinator"),
                    ClaimOutcome::Claimed { .. }
                ));
                // Even knowing a live coordinator's exact claim identity cannot
                // smuggle it through artifact acknowledgement replay.
                assert!(
                    matches!(
                        store
                            .claim_artifact_job(ClaimJob {
                                kind: JobKind::MediaProbe,
                                ..request
                            })
                            .await
                            .expect("wrong-kind replay"),
                        ClaimOutcome::Unsupported
                    ),
                    "{backend}"
                );
                let token = store
                    .background_job(&id)
                    .await
                    .expect("job")
                    .expect("job")
                    .token
                    .expect("token");
                store
                    .settle_job(SettleJob {
                        token,
                        settlement: JobSettlement::Stop {
                            error_code: "scope_fixture_complete".into(),
                        },
                        now_ms: 1_001,
                    })
                    .await
                    .expect("cleanup");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn background_subtitles_share_one_owner_and_publish_with_exact_current_authority() {
    use super::subtitle_jobs_fixture::SubtitleFixture;
    use plurx_core::store::background_jobs_subtitle::{SubtitleJobWrite, WriteSubtitleJob};
    for_each_backend(|store, backend| async move {
        for (index, scenario) in ["complete", "cancel", "takeover", "forged_source"]
            .iter()
            .enumerate()
        {
            let now = 1_000 + index as i64 * 100_000;
            let (_, file_id) = seed_file(&store, &format!("subtitle-common-{scenario}")).await;
            let stamp = super::subtitle_source_stamp(file_id);
            let queued = store
                .enqueue_or_promote_subtitle_source(&stamp, "foreground", now)
                .await
                .expect("subtitle ownership contract")
                .expect("subtitle ownership contract");
            assert!(
                store
                    .claim_analysis_request("node-a", now, now + 1_000)
                    .await
                    .expect("subtitle ownership contract")
                    .is_none(),
                "{backend}: old scheduler cannot claim subtitles"
            );
            assert!(store
                .subtitle_job_intents(128)
                .await
                .expect("subtitle ownership contract")
                .contains(&queued.request_id));
            let request = store
                .claim_subtitle_fixture(&queued.request_id, "node-a", now + 1, now + 1_000)
                .await
                .expect("subtitle ownership contract")
                .expect("subtitle ownership contract");
            assert!(!store
                .subtitle_job_intents(128)
                .await
                .expect("subtitle ownership contract")
                .contains(&queued.request_id));
            assert!(
                store
                    .claim_subtitle_fixture(&queued.request_id, "node-b", now + 2, now + 1_000)
                    .await
                    .expect("subtitle ownership contract")
                    .is_none(),
                "{backend}: joined demand cannot create another owner"
            );
            let token = store
                .subtitle_fixture_token(&request)
                .await
                .expect("subtitle ownership contract");
            assert!(!store
                .renew_analysis_request(
                    &request.request_id,
                    "node-a",
                    request.fence,
                    now + 2,
                    now + 60_000
                )
                .await
                .expect("subtitle ownership contract"));
            assert!(!store
                .complete_analysis_request(&request, "old-path", now + 2)
                .await
                .expect("subtitle ownership contract"));
            let mut write = WriteSubtitleJob {
                token: token.clone(),
                request: request.clone(),
                now_ms: now + 3,
                output: SubtitleJobWrite::Representation {
                    publication: plurx_core::store::SubtitleSourcePublication {
                        file_id,
                        source_size: stamp.source_size,
                        source_mtime: stamp.source_mtime,
                        source_attestation: "a".repeat(64),
                        node_id: "node-a".into(),
                        ordinal: 0,
                        kind: "text".into(),
                        format: "webvtt".into(),
                        verdict: "kept".into(),
                        attempts: 1,
                        origin: "extracted".into(),
                        sha256: "b".repeat(64),
                        bytes: 20,
                        published_at_ms: now + 3,
                    },
                },
            };
            match *scenario {
                "cancel" => {
                    store
                        .cancel_analysis_request_admin(&request.request_id, now + 3)
                        .await
                        .expect("subtitle ownership contract");
                    assert_eq!(
                        store
                            .background_job(&token.job_id)
                            .await
                            .expect("subtitle ownership contract")
                            .expect("subtitle ownership contract")
                            .state,
                        JobState::Cancelling
                    );
                }
                "takeover" => {
                    let later = store
                        .claim_subtitle_fixture(
                            &queued.request_id,
                            "node-b",
                            now + 40_000,
                            now + 41_000,
                        )
                        .await
                        .expect("subtitle ownership contract")
                        .expect("subtitle ownership contract");
                    assert!(later.fence > request.fence);
                    write.now_ms = now + 40_001;
                    assert!(
                        !store
                            .write_subtitle_job(write.clone())
                            .await
                            .expect("subtitle ownership contract"),
                        "{backend}: old owner cannot publish after takeover"
                    );
                    store
                        .fail_subtitle_fixture(&later, "fixture_done", now + 40_002)
                        .await
                        .expect("subtitle ownership contract");
                }
                "forged_source" => {
                    // Matching an existing request ID/fence is insufficient:
                    // the complete source tuple must come from that record.
                    write.request.source_size += 1;
                    if let SubtitleJobWrite::Representation { publication } = &mut write.output {
                        publication.source_size += 1;
                    }
                }
                _ => {}
            }
            assert_eq!(
                store
                    .write_subtitle_job(write)
                    .await
                    .expect("subtitle ownership contract"),
                *scenario == "complete",
                "{backend}/{scenario}"
            );
            if *scenario == "complete" {
                let completed = WriteSubtitleJob {
                    token: token.clone(),
                    request: request.clone(),
                    now_ms: now + 4,
                    output: SubtitleJobWrite::Complete {
                        result_key: "subtitle-test-result".into(),
                    },
                };
                assert!(store
                    .write_subtitle_job(completed.clone())
                    .await
                    .expect("subtitle ownership contract"));
                assert!(
                    store
                        .write_subtitle_job(completed)
                        .await
                        .expect("subtitle ownership contract"),
                    "{backend}: lost completion acknowledgement is idempotent"
                );
                let waiters = store
                    .job_waiters(WaiterQuery {
                        job_id: token.job_id,
                        after: None,
                        limit: 10,
                    })
                    .await
                    .expect("retired subtitle receipts");
                assert_eq!(waiters.waiters.len(), 1, "{backend}");
                assert_eq!(waiters.waiters[0].state, "succeeded", "{backend}");
                assert_eq!(
                    waiters.waiters[0].result_ref.as_deref(),
                    Some("subtitle-test-result"),
                    "{backend}"
                );
                assert_eq!(
                    store
                        .analysis_request(&request.request_id)
                        .await
                        .expect("subtitle ownership contract")
                        .expect("subtitle ownership contract")
                        .state,
                    "ready"
                );
            } else {
                assert!(store
                    .list_subtitle_source_publications(
                        file_id,
                        stamp.source_size,
                        stamp.source_mtime
                    )
                    .await
                    .expect("subtitle ownership contract")
                    .is_empty());
            }
        }
    })
    .await;
}

#[tokio::test]
async fn background_subtitle_admin_retry_and_later_intents_remain_admissible() {
    use super::subtitle_jobs_fixture::SubtitleFixture;
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "subtitle-retry-outbox").await;
        let original = store
            .enqueue_or_promote_subtitle_source(
                &super::subtitle_source_stamp(file_id),
                "normal",
                1000,
            )
            .await
            .expect("enqueue")
            .expect("request");
        store
            .cancel_analysis_request_admin(&original.request_id, 1001)
            .await
            .expect("cancel");
        let retry = store
            .retry_analysis_request_admin(&original.request_id, "subtitle-admin-successor", 1002)
            .await
            .expect("retry")
            .expect("successor");
        assert!(retry.force_rebuild, "{backend}: exercise real Retry input");
        let (_, later_file) = seed_file(&store, "subtitle-later-outbox").await;
        let later = store
            .enqueue_or_promote_subtitle_source(
                &super::subtitle_source_stamp(later_file),
                "normal",
                1003,
            )
            .await
            .expect("later")
            .expect("request");
        for request in [retry, later] {
            let claimed = store
                .claim_subtitle_fixture(&request.request_id, "node-a", 1004, 2000)
                .await
                .expect("admit retry and later demand")
                .expect("common owner");
            assert!(
                store
                    .complete_subtitle_fixture(&claimed, "retry-result", 1005)
                    .await
                    .expect("complete"),
                "{backend}"
            );
        }
        assert!(
            store
                .subtitle_job_intents(128)
                .await
                .expect("outbox")
                .is_empty(),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn background_artwork_publication_fences_builds_and_completes_verified_delivery() {
    use plurx_core::store::background_jobs_artwork::{
        ArtworkLocation, ArtworkVariantSpec, PublishArtworkJob, ARTWORK_PIPELINE,
    };
    for_each_backend(|store, backend| async move {
        let spec = ArtworkVariantSpec {
            source_name: "poster.png".into(),
            source_sha256: "a".repeat(64),
            width: 300,
            format: "png".into(),
            pipeline: ARTWORK_PIPELINE.into(),
        };
        let key = spec.artifact_key();
        let id = uuid::Uuid::new_v4().to_string();
        for (index, node) in ["node-a", "node-b"].iter().enumerate() {
            let mut alias = spec.clone();
            if index == 1 {
                alias.source_name = "alias.png".into();
            }
            assert_eq!(alias.artifact_key(), key);
            let admitted = store
                .enqueue_job(EnqueueJob {
                    id: if index == 0 {
                        id.clone()
                    } else {
                        uuid::Uuid::new_v4().to_string()
                    },
                    payload: JobPayload::ArtworkDerivative {
                        artifact_key: key.clone(),
                        spec: alias,
                    },
                    dedupe_key: format!("artwork:{key}"),
                    priority: 1,
                    not_before_ms: 1000,
                    now_ms: 1000,
                    request: JobRequest {
                        scope: "artwork".into(),
                        request_id: node.to_string(),
                        request_digest: key.clone(),
                        consumer_kind: "artwork".into(),
                        consumer_ref: key.clone(),
                        target_node_id: Some(node.to_string()),
                        deadline_ms: Some(100_000),
                        retain_identity: false,
                    },
                })
                .await
                .expect("enqueue");
            assert!(
                matches!(admitted, EnqueueOutcome::Accepted { job_id, .. } if job_id == id),
                "{backend}"
            );
        }
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::ArtworkDerivative,
                payload_version: 1,
                now_ms: 1000,
                dispatched_at_ms: 1000,
            })
            .await
            .expect("claim")
        else {
            panic!("{backend}: claim")
        };
        let publication = PublishArtworkJob {
            token: job.token.expect("token"),
            now_ms: 2000,
            location: ArtworkLocation {
                artifact_key: key.clone(),
                node_id: "node-a".into(),
                spec: spec.clone(),
                blob_sha256: "b".repeat(64),
                bytes: 123,
                built_by_node_id: "node-a".into(),
                built_at_ms: 2000,
                verified_at_ms: 2000,
            },
        };
        let mut forged = publication.clone();
        forged.token.fence += 1;
        assert!(matches!(
            store.publish_artwork_job(forged).await.expect("fenced"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(store
            .artwork_locations(&key, 2000)
            .await
            .expect("empty")
            .is_empty());
        assert!(matches!(
            store
                .publish_artwork_job(publication.clone())
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        let mut replay = publication.clone();
        replay.now_ms += 1;
        replay.location.verified_at_ms += 1;
        assert!(matches!(
            store.publish_artwork_job(replay).await.expect("lost reply"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        let waiters = store
            .job_waiters(WaiterQuery {
                job_id: id.clone(),
                after: None,
                limit: 10,
            })
            .await
            .expect("waiters")
            .waiters;
        assert_eq!(
            waiters
                .iter()
                .find(|w| w.request_id == "node-a")
                .expect("local")
                .state,
            "succeeded"
        );
        assert_eq!(
            waiters
                .iter()
                .find(|w| w.request_id == "node-b")
                .expect("remote")
                .state,
            "awaiting_hydration"
        );
        let intents = store.delivery_intents(3000).await.expect("outbox");
        let delivery = intents
            .into_iter()
            .find(|i| i.artifact_key == format!("artwork:{key}"))
            .expect("artwork delivery");
        let EnqueueOutcome::Accepted {
            job_id: hydration, ..
        } = store
            .enqueue_delivery(delivery, 3000)
            .await
            .expect("admit delivery")
        else {
            panic!("delivery")
        };
        let claim = ClaimJob {
            job_id: hydration.clone(),
            expected_revision: 0,
            node_id: "node-a".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::ArtifactHydrate,
            payload_version: 1,
            now_ms: 3000,
            dispatched_at_ms: 3000,
        };
        assert!(!matches!(
            store.claim_job(claim.clone()).await.expect("wrong target"),
            ClaimOutcome::Claimed { .. }
        ));
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                node_id: "node-b".into(),
                ..claim
            })
            .await
            .expect("hydrate")
        else {
            panic!("hydrate claim")
        };
        let mut received = publication.clone();
        received.token = job.token.expect("token");
        received.now_ms = 4000;
        received.location.node_id = "node-b".into();
        received.location.verified_at_ms = 4000;
        let mut corrupt = received.clone();
        corrupt.location.blob_sha256 = "c".repeat(64);
        assert!(!matches!(
            store
                .publish_artwork_job(corrupt)
                .await
                .expect("corrupt transfer"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store
                .publish_artwork_job(received)
                .await
                .expect("verified copy"),
            JobPublishOutcome::Published { .. }
        ));
        assert_eq!(
            store
                .artwork_locations(&key, 4000)
                .await
                .expect("two holders")
                .len(),
            2
        );
        assert!(store
            .job_waiters(WaiterQuery {
                job_id: id,
                after: None,
                limit: 10
            })
            .await
            .expect("receipts")
            .waiters
            .iter()
            .all(|w| w.state == "succeeded"));
        assert!(store
            .delivery_intents(4000)
            .await
            .expect("drained")
            .is_empty());
        let mut future = spec;
        future.pipeline = "ffmpeg-lanczos-fit-v2".into();
        assert_ne!(future.artifact_key(), key);
        store.maintain_jobs(700_000_000).await.expect("retention");
        assert!(store
            .artwork_locations(&key, 700_000_000)
            .await
            .expect("expired")
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn background_artwork_grid_demand_coalesces_and_does_not_erase_failure_budget() {
    use plurx_core::store::background_jobs_artwork::{ArtworkVariantSpec, ARTWORK_PIPELINE};
    for_each_backend(|store, backend| async move {
        let spec = ArtworkVariantSpec { source_name: "grid.png".into(), source_sha256: "c".repeat(64), width: 500, format: "png".into(), pipeline: ARTWORK_PIPELINE.into() };
        let EnqueueOutcome::Accepted { job_id, .. } = store.enqueue_artwork_demand(spec.clone(), "node-a", 1000).await.expect("demand") else { panic!("{backend}: demand") };
        for _ in 0..20 {
            assert!(matches!(store.enqueue_artwork_demand(spec.clone(), "node-a", 1001).await.expect("repeat"), EnqueueOutcome::Existing { job_id: existing, .. } if existing == job_id));
        }
        assert_eq!(store.job_waiters(WaiterQuery { job_id: job_id.clone(), after: None, limit: 128 }).await.expect("receipts").waiters.len(), 1);
        store.cancel_job(CancelJob { job_id: job_id.clone(), now_ms: 2000 }).await.expect("cancel");
        assert!(matches!(store.enqueue_artwork_demand(spec.clone(), "node-a", 3000).await.expect("cooldown"), EnqueueOutcome::Existing { cancelled: true, .. }));
        assert!(matches!(store.enqueue_artwork_demand(spec, "node-a", 3_603_000).await.expect("new later demand"), EnqueueOutcome::Accepted { job_id: next, .. } if next != job_id));
    }).await;
}

#[tokio::test]
async fn background_prediction_cap_preserves_foreground_admission_and_releases_cancelled_slots() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "prediction-cap").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let mut prototype = preparation_request(&file, &uuid::Uuid::new_v4().to_string(), 1000);
        let mut first = None;
        for index in 0..64 {
            let mut request = prototype.clone();
            request.id = uuid::Uuid::new_v4().to_string();
            request.request.request_id = format!("prediction-{index}");
            if index == 0 {
                first = Some(request.request.clone());
            }
            assert!(
                matches!(
                    store.enqueue_job(request).await.expect("prediction"),
                    EnqueueOutcome::Accepted { .. }
                ),
                "{backend}: slot {index}"
            );
        }
        prototype.id = uuid::Uuid::new_v4().to_string();
        prototype.request.request_id = "overflow".into();
        assert!(
            matches!(
                store.enqueue_job(prototype.clone()).await.expect("bounded"),
                EnqueueOutcome::QueueFull
            ),
            "{backend}"
        );
        let mut foreground = prototype.clone();
        foreground.id = uuid::Uuid::new_v4().to_string();
        foreground.priority = 3;
        foreground.request.scope = "user:foreground".into();
        assert!(
            matches!(
                store.enqueue_job(foreground).await.expect("foreground"),
                EnqueueOutcome::Accepted { .. }
            ),
            "{backend}: speculation cannot fill foreground capacity"
        );
        let first = first.expect("first interest");
        store
            .cancel_waiter(CancelWaiter {
                scope: first.scope,
                request_id: first.request_id,
                now_ms: 1001,
            })
            .await
            .expect("cancel one interest");
        assert!(
            matches!(
                store.enqueue_job(prototype).await.expect("released slot"),
                EnqueueOutcome::Accepted { .. }
            ),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn background_preparation_demand_expires_after_one_day_and_dormant_users_are_not_viewers() {
    use plurx_core::store::background_jobs_preparation::PreparationDemand;
    for_each_backend(|store, backend| async move {
        let (user, file_id) = seed_file(&store, "preparation-demand").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let watch = store.put_progress(user, file.item_id, 1000, Some(100_000)).await.expect("demand");
        let now = watch.updated_at * 1000;
        let demand = store.preparation_demands(now).await.expect("demand snapshot");
        assert!(demand.iter().any(|row| matches!(row, PreparationDemand::Item { item_id, .. } if *item_id == file.item_id)), "{backend}");
        assert!(!demand.iter().any(|row| matches!(row, PreparationDemand::Viewer { .. })), "{backend}: watch history is not an active lease");
        assert!(store.preparation_demands(now + 86_400_000).await.expect("expired snapshot").is_empty(), "{backend}");
        assert!(store.hot_artifacts(now).await.expect("empty artifact snapshot").is_empty(), "{backend}: demand alone is not proof of stored bytes");
    }).await;
}

#[tokio::test]
async fn background_prediction_lifecycle_cancels_only_owned_work_and_playback_adopts_subtitles() {
    use plurx_core::cluster::coordination::LeaseClaim;
    use plurx_core::store::background_jobs_predictions::SyncPredictions;
    use plurx_core::store::{NewAnalysisRequest, SubtitleSourceStamp};
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "prediction-lifecycle").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let LeaseClaim::Acquired(lease) = store
            .acquire_lease("candidate:prediction", "node-a", now, now + 90_000)
            .await
            .expect("lease")
        else {
            panic!("{backend}: lease")
        };
        let replacement = lease.publication_successor().expect("replacement");
        let request = |id: char, component: &str| NewAnalysisRequest {
            request_id: id.to_string().repeat(64),
            file_id,
            source_size: file.size,
            source_mtime: file.mtime,
            component: component.into(),
            pipeline_version: "prediction-pipeline".into(),
            video_identity: String::new(),
            requested_generation: format!("predict:{}", id.to_string().repeat(64)),
            priority: "normal".into(),
            trigger: "background".into(),
            force_rebuild: false,
            target_node_id: if component == "fragment_index" {
                "node-a".into()
            } else {
                String::new()
            },
            not_before_ms: now,
            created_at_ms: now,
        };
        let index = request('a', "fragment_index");
        let subtitle = request('b', "subtitle_source");
        store
            .sync_predictions(SyncPredictions {
                requests: vec![index.clone(), subtitle.clone()],
                desired_files: vec![file_id],
                lease: lease.clone(),
                replacement: replacement.clone(),
                now_ms: now,
            })
            .await
            .expect("persist predictions");
        assert_eq!(
            store.pending_predictions(now).await.expect("outbox").len(),
            2,
            "{backend}"
        );
        store
            .enqueue_analysis_request(&index)
            .await
            .expect("index intent");
        let analysis = store
            .enqueue_analysis_request(&subtitle)
            .await
            .expect("subtitle intent");
        let admitted = store
            .enqueue_subtitle_job(analysis, now)
            .await
            .expect("subtitle job");
        let EnqueueOutcome::Accepted { job_id, .. } = admitted else {
            panic!("{backend}: subtitle job")
        };
        let waiters = || WaiterQuery {
            job_id: job_id.clone(),
            after: None,
            limit: 128,
        };
        assert_eq!(
            store.job_waiters(waiters()).await.expect("waiters").waiters[0].deadline_ms,
            Some(now + 86_400_000)
        );
        let adopted = store
            .enqueue_or_promote_subtitle_source(
                &SubtitleSourceStamp {
                    file_id,
                    source_size: file.size,
                    source_mtime: file.mtime,
                    pipeline_version: subtitle.pipeline_version.clone(),
                },
                "foreground",
                now + 1,
            )
            .await
            .expect("playback adopts")
            .expect("analysis");
        assert_eq!(adopted.request_id, subtitle.request_id, "{backend}");
        assert_eq!(
            store.job_waiters(waiters()).await.expect("waiters").waiters[0].deadline_ms,
            None,
            "{backend}: actual playback outlives prediction"
        );
        let third = replacement.publication_successor().expect("successor");
        store
            .sync_predictions(SyncPredictions {
                requests: vec![],
                desired_files: vec![],
                lease: replacement.clone(),
                replacement: third.clone(),
                now_ms: now + 2,
            })
            .await
            .expect("demand disappeared");
        assert_eq!(
            store
                .analysis_request(&index.request_id)
                .await
                .expect("index")
                .expect("index")
                .state,
            "cancelled",
            "{backend}"
        );
        assert_eq!(
            store
                .analysis_request(&subtitle.request_id)
                .await
                .expect("subtitle")
                .expect("subtitle")
                .state,
            "queued",
            "{backend}: adopted request remains ordinary work"
        );
        assert_eq!(
            store
                .background_job(&job_id)
                .await
                .expect("job")
                .expect("job")
                .state,
            JobState::Queued,
            "{backend}"
        );
        assert!(
            store
                .sync_predictions(SyncPredictions {
                    requests: vec![],
                    desired_files: vec![],
                    lease,
                    replacement,
                    now_ms: now + 3
                })
                .await
                .is_err(),
            "{backend}: stale discovery cannot mutate predictions"
        );

        // A crash between intent persistence and domain admission is safe in
        // both directions: another pass can drain it, and a retired intent
        // cannot be materialized by the old producer's delayed request.
        let mut delayed = request('c', "fragment_index");
        delayed.pipeline_version = "delayed-pipeline".into();
        let fourth = third.publication_successor().expect("successor");
        store
            .sync_predictions(SyncPredictions {
                requests: vec![delayed.clone()],
                desired_files: vec![file_id],
                lease: third,
                replacement: fourth.clone(),
                now_ms: now + 4,
            })
            .await
            .expect("new outbox intent");
        assert_eq!(
            store
                .pending_predictions(now + 4)
                .await
                .expect("outbox")
                .len(),
            1
        );
        let fifth = fourth.publication_successor().expect("successor");
        store
            .sync_predictions(SyncPredictions {
                requests: vec![],
                desired_files: vec![],
                lease: fourth,
                replacement: fifth,
                now_ms: now + 5,
            })
            .await
            .expect("retire before dispatch");
        delayed.created_at_ms = now + 6;
        delayed.not_before_ms = now + 6;
        assert!(
            store.enqueue_analysis_request(&delayed).await.is_err(),
            "{backend}: no orphan speculative request after retire"
        );
        assert!(
            store
                .analysis_request(&delayed.request_id)
                .await
                .expect("delayed request")
                .is_none(),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn background_next_episode_uses_the_active_session_before_watch_completion() {
    use plurx_core::domain::{MediaSessionActivation, MEDIA_SESSION_PUBLICATION_BLOCKED};
    use plurx_core::store::background_jobs_preparation::PreparationDemand;
    for_each_backend(|store, backend| async move {
        let fixture = super::seed_watch_fence_fixture(&store, "prediction-next").await;
        let mut files = Vec::new();
        for (number, episode) in fixture.episodes.iter().enumerate() {
            files.push(
                store
                    .upsert_file(
                        *episode,
                        &format!("/prediction-next/{number}.mkv"),
                        100,
                        1,
                        &Default::default(),
                    )
                    .await
                    .expect("episode file"),
            );
        }
        assert!(
            store
                .next_up(fixture.user, 1)
                .await
                .expect("normal rail")
                .is_empty(),
            "{backend}: no completed episodes yet"
        );
        let activation = MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: fixture.user,
            playback_id: "prediction-next-playback".into(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: "node-a".into(),
            recipe_json: serde_json::json!({"request":{"file_id":files[0]}}).to_string(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: 1000,
            lease_expires_at_ms: 91_000,
        };
        store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("route");
        super::confirm_media_activation(store.as_ref(), &activation, 0, backend).await;
        let viewers = store
            .preparation_demands(1001)
            .await
            .expect("snapshot")
            .into_iter()
            .filter_map(|demand| match demand {
                PreparationDemand::Viewer {
                    user_id,
                    next_item_id,
                    ..
                } => Some((user_id, next_item_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            viewers,
            vec![(fixture.user, Some(fixture.episodes[1]))],
            "{backend}: exactly one following episode"
        );
        assert!(
            !store
                .preparation_demands(91_001)
                .await
                .expect("expired viewer")
                .iter()
                .any(|demand| matches!(demand, PreparationDemand::Viewer { .. })),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn background_embeddings_share_verified_vectors_and_fence_source_changes() {
    use plurx_core::store::background_jobs_embeddings::{
        entry_digest, EmbeddingModel, PublishEmbeddingJob, SharedEmbedding,
    };
    for_each_backend(|store, backend| async move {
        for scenario in ["published", "source_changed", "cancelled", "expired"] {
            let (_, file_id) = seed_file(&store, &format!("embedding-{scenario}")).await;
            let item_id = store
                .get_file(file_id)
                .await
                .expect("file")
                .expect("file")
                .item_id;
            let entry = store
                .classification_page(item_id - 1, 1)
                .await
                .expect("entry")
                .remove(0);
            let model = EmbeddingModel {
                weights_sha256: "a".repeat(64),
                config_sha256: "b".repeat(64),
                tokenizer_sha256: "c".repeat(64),
                tokenizer_version: "v1".into(),
                dimensions: 3,
                normalization_version: "l2-v1".into(),
            };
            let content = entry_digest(&entry);
            let id = uuid::Uuid::new_v4().to_string();
            let request = EnqueueJob {
                id: id.clone(),
                payload: JobPayload::SemanticEmbedding {
                    item_id,
                    content_digest: content.clone(),
                    model_digest: model.digest(),
                },
                dedupe_key: format!("embed:{item_id}:{}", model.digest()),
                priority: 3,
                not_before_ms: 1000,
                now_ms: 1000,
                request: JobRequest {
                    scope: "semantic".into(),
                    request_id: format!("{item_id}:a"),
                    request_digest: content.clone(),
                    consumer_kind: "semantic".into(),
                    consumer_ref: item_id.to_string(),
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            };
            assert!(
                matches!(
                    store.enqueue_job(request.clone()).await.expect("enqueue"),
                    EnqueueOutcome::Accepted { .. }
                ),
                "{backend}"
            );
            let mut other = request.clone();
            other.id = uuid::Uuid::new_v4().to_string();
            other.request.request_id = format!("{item_id}:b");
            assert!(matches!(
                store.enqueue_job(other.clone()).await.expect("join"),
                EnqueueOutcome::Existing { .. } | EnqueueOutcome::Accepted { .. }
            ));
            store
                .cancel_waiter(CancelWaiter {
                    scope: "semantic".into(),
                    request_id: request.request.request_id.clone(),
                    now_ms: 1001,
                })
                .await
                .expect("independent cancellation");
            let candidate = store.background_job(&id).await.expect("read").expect("job");
            let ClaimOutcome::Claimed { job } = store
                .claim_artifact_job(ClaimJob {
                    job_id: id.clone(),
                    expected_revision: candidate.revision,
                    node_id: "node-a".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::SemanticEmbedding,
                    payload_version: 1,
                    now_ms: 1010,
                    dispatched_at_ms: 1010,
                })
                .await
                .expect("claim")
            else {
                panic!("{backend}: claim");
            };
            let vector = vec![1., 0., 0.];
            let mut publication = PublishEmbeddingJob {
                token: job.token.expect("token"),
                artifact: SharedEmbedding {
                    item_id,
                    content_digest: content.clone(),
                    model: model.clone(),
                    vector_sha256: SharedEmbedding::vector_digest(&vector),
                    vector,
                },
                source_json: entry.source_json.clone(),
                classification_revision: 0,
                now_ms: 1100,
            };
            for invalid in ["model", "dimension", "hash", "norm"] {
                let mut bad = publication.clone();
                match invalid {
                    "model" => bad.artifact.model.tokenizer_version = "v2".into(),
                    "dimension" => bad.artifact.vector.push(0.),
                    "hash" => bad.artifact.vector_sha256 = "0".repeat(64),
                    _ => {
                        bad.artifact.vector = vec![2., 0., 0.];
                        bad.artifact.vector_sha256 =
                            SharedEmbedding::vector_digest(&bad.artifact.vector);
                    }
                }
                assert!(
                    !matches!(
                        store.publish_embedding_job(bad).await,
                        Ok(JobPublishOutcome::Published { .. })
                    ),
                    "{backend}: {invalid}"
                );
            }
            if scenario == "source_changed" {
                let record = plurx_core::store::classification::Record {
                    source_json: entry.source_json.clone(),
                    revision: 0,
                    overrides: Default::default(),
                    classification: plurx_core::metadata::classification::classify(
                        &entry.input().expect("input").metadata(),
                        vec!["new classification".into()],
                    ),
                };
                assert!(store
                    .write_classification(item_id, &record)
                    .await
                    .expect("change"));
            } else if scenario == "cancelled" {
                store
                    .cancel_job(CancelJob {
                        job_id: id.clone(),
                        now_ms: 1050,
                    })
                    .await
                    .expect("cancel");
            } else if scenario == "expired" {
                publication.now_ms = publication.token.lease_expires_ms;
            }
            let verdict = store
                .publish_embedding_job(publication.clone())
                .await
                .expect("publish");
            if scenario == "published" {
                assert!(
                    matches!(verdict, JobPublishOutcome::Published { .. }),
                    "{backend}"
                );
                publication.now_ms += 1;
                assert!(matches!(
                    store
                        .publish_embedding_job(publication)
                        .await
                        .expect("lost reply"),
                    JobPublishOutcome::AlreadyPublished { .. }
                ));
                let reused = store
                    .embedding_for(item_id, &content, &model.digest())
                    .await
                    .expect("new node lookup");
                assert_eq!(
                    reused.artifact.expect("portable vector").vector,
                    vec![1., 0., 0.]
                );
                assert_eq!(reused.last_completed_job.as_deref(), Some(id.as_str()));
                let mut late = other;
                late.id = uuid::Uuid::new_v4().to_string();
                late.request.request_id = format!("{item_id}:late-node");
                assert!(matches!(
                    store.enqueue_job(late).await.expect("late discovery"),
                    EnqueueOutcome::NoDemand
                ));
                let waiters = store
                    .job_waiters(WaiterQuery {
                        job_id: id,
                        after: None,
                        limit: 10,
                    })
                    .await
                    .expect("waiters")
                    .waiters;
                assert_eq!(
                    waiters
                        .iter()
                        .filter(|waiter| waiter.state == "succeeded")
                        .count(),
                    1
                );
                assert_eq!(
                    waiters
                        .iter()
                        .filter(|waiter| waiter.state == "cancelled")
                        .count(),
                    1
                );
                let mut wrong_model = model;
                wrong_model.dimensions = 4;
                assert!(store
                    .embedding_for(item_id, &content, &wrong_model.digest())
                    .await
                    .expect("different model")
                    .artifact
                    .is_none());
            } else {
                assert!(
                    matches!(verdict, JobPublishOutcome::LostOwnership),
                    "{backend}: {scenario}"
                );
                assert!(store
                    .embedding_for(item_id, &content, &model.digest())
                    .await
                    .expect("absent")
                    .artifact
                    .is_none());
            }
        }
    })
    .await;
}

async fn probe_fixture(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    file_id: i64,
) -> JobPayload {
    use plurx_core::store::background_jobs_probe::{generation, ProbeCoordinator};
    let coordinator = match store
        .acquire_lease("repair:probe", "probe-coordinator", 1000, 200_000)
        .await
        .expect("coordinator")
    {
        plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) => {
            ProbeCoordinator::from_lease(&lease)
        }
        plurx_core::cluster::coordination::LeaseClaim::Held {
            owner_node_id,
            fence,
            ..
        } => ProbeCoordinator {
            resource: "repair:probe".into(),
            node_id: owner_node_id,
            fence,
        },
    };
    let file = store.get_file(file_id).await.expect("file").expect("file");
    let probe_digest = "a".repeat(64);
    JobPayload::MediaProbe {
        file_id,
        source_generation: generation(file_id, file.size, file.mtime, &coordinator, &probe_digest),
        probe_digest,
        source_size: file.size,
        source_mtime: file.mtime,
        coordinator,
    }
}

#[tokio::test]
async fn background_probe_facts_require_current_source_and_coordinator_to_apply() {
    use plurx_core::store::background_jobs_probe::{ApplyProbeJob, ProbeOutput, PublishProbeJob};
    for_each_backend(|store, backend| async move {
        let coordinator = super::acquired(
            store
                .acquire_lease("repair:probe", "probe-coordinator", 1000, 200_000)
                .await
                .expect("coordinator"),
            backend,
        );
        for scenario in [
            "published",
            "source_changed",
            "cancelled",
            "wrong_coordinator",
        ] {
            let (_, file_id) = seed_file(&store, &format!("leaf-{scenario}")).await;
            let file = store.get_file(file_id).await.expect("file").expect("file");
            let payload = probe_fixture(&store, file_id).await;
            let id = uuid::Uuid::new_v4().to_string();
            let accepted = store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: payload.clone(),
                    dedupe_key: format!("leaf:{file_id}"),
                    priority: 1,
                    not_before_ms: 1000,
                    now_ms: 1000,
                    request: JobRequest {
                        scope: "probe-contract".into(),
                        request_id: id.clone(),
                        request_digest: "b".repeat(64),
                        consumer_kind: "probe".into(),
                        consumer_ref: file_id.to_string(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("admit");
            assert!(
                matches!(accepted, EnqueueOutcome::Accepted { .. }),
                "{backend}"
            );
            let ClaimOutcome::Claimed { job } = store
                .claim_artifact_job(ClaimJob {
                    job_id: id.clone(),
                    expected_revision: 0,
                    node_id: "leaf-worker".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::MediaProbe,
                    payload_version: 1,
                    now_ms: 1000,
                    dispatched_at_ms: 1000,
                })
                .await
                .expect("claim")
            else {
                panic!("{backend}: claim");
            };
            let publication = PublishProbeJob {
                token: job.token.expect("token"),
                output: ProbeOutput {
                    source: payload,
                    probe: plurx_core::domain::ProbeResult {
                        duration_ms: Some(123456),
                        video_codec: Some("hevc".into()),
                        raw_json: Some("{}".into()),
                        ..Default::default()
                    },
                },
                now_ms: 1100,
            };
            if scenario == "source_changed" {
                store
                    .upsert_file(
                        file.item_id,
                        file.path.to_str().expect("path"),
                        file.size + 1,
                        file.mtime + 1,
                        &Default::default(),
                    )
                    .await
                    .expect("replace");
            } else if scenario == "cancelled" {
                store
                    .cancel_job(CancelJob {
                        job_id: id.clone(),
                        now_ms: 1050,
                    })
                    .await
                    .expect("cancel");
            }
            let result = store
                .publish_probe_job(publication.clone())
                .await
                .expect("publish");
            let mut apply = ApplyProbeJob {
                job_id: id,
                lease: coordinator.clone(),
                now_ms: 1200,
            };
            if matches!(scenario, "source_changed" | "cancelled") {
                assert!(
                    matches!(result, JobPublishOutcome::LostOwnership),
                    "{backend}: {scenario}"
                );
                assert!(!store.apply_probe_job(apply).await.expect("cannot apply"));
                // Model the worker's joined retirement before the next case
                // asks for the same bounded source-I/O lane.
                store
                    .settle_job(SettleJob {
                        token: publication.token,
                        settlement: JobSettlement::Stop {
                            error_code: "source_or_interest_changed".into(),
                        },
                        now_ms: 1200,
                    })
                    .await
                    .expect("retire refused publication");
            } else {
                assert!(
                    matches!(result, JobPublishOutcome::Published { .. }),
                    "{backend}: {scenario}"
                );
                assert!(matches!(
                    store.publish_probe_job(publication).await.expect("replay"),
                    JobPublishOutcome::AlreadyPublished { .. }
                ));
                assert_ne!(
                    store
                        .get_file(file_id)
                        .await
                        .expect("file")
                        .expect("file")
                        .duration_ms,
                    Some(123456),
                    "worker must not publish catalogue facts"
                );
                if scenario == "wrong_coordinator" {
                    apply.lease.fence += 1;
                    assert!(!store
                        .apply_probe_job(apply)
                        .await
                        .expect("stale coordinator"));
                } else {
                    assert!(store
                        .apply_probe_job(apply.clone())
                        .await
                        .expect("coordinator applies"));
                    assert!(store
                        .apply_probe_job(apply.clone())
                        .await
                        .expect("application replay"));
                    assert_eq!(
                        store
                            .get_file(file_id)
                            .await
                            .expect("file")
                            .expect("file")
                            .duration_ms,
                        Some(123456)
                    );
                    store
                        .upsert_file(
                            file.item_id,
                            file.path.to_str().expect("path"),
                            file.size + 1,
                            file.mtime + 1,
                            &Default::default(),
                        )
                        .await
                        .expect("replace after publication");
                    assert!(!store
                        .apply_probe_job(apply)
                        .await
                        .expect("cannot apply old facts to replacement"));
                }
            }
        }
    })
    .await;
}

async fn integrity_claim(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    id: &str,
    kind: JobKind,
    now: i64,
) -> JobToken {
    integrity_claim_on(store, id, kind, now, "node-a").await
}
async fn integrity_claim_on(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    id: &str,
    kind: JobKind,
    now: i64,
    node: &str,
) -> JobToken {
    let job = store
        .background_job(id)
        .await
        .expect("lookup")
        .expect("job");
    let outcome = store
        .claim_job(ClaimJob {
            job_id: id.into(),
            expected_revision: job.revision,
            node_id: node.into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim");
    let ClaimOutcome::Claimed { job } = outcome else {
        panic!("claim refused: {outcome:?}")
    };
    job.token.expect("token")
}
async fn integrity_verify_claim(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    key: &str,
    now: i64,
) -> JobToken {
    integrity_verify_claim_on(store, key, now, "node-a").await
}

pub(super) async fn integrity_verify_claim_on(
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    key: &str,
    now: i64,
    node: &str,
) -> JobToken {
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::ArtifactVerify {
                artifact_key: key.into(),
                target_node_id: node.into(),
            },
            dedupe_key: id.clone(),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "integrity-contract".into(),
                request_id: id.clone(),
                request_digest: "a".repeat(64),
                consumer_kind: "artifact_verify".into(),
                consumer_ref: id.clone(),
                target_node_id: None,
                deadline_ms: Some(now + 300_000),
                retain_identity: false,
            },
        })
        .await
        .expect("admission");
    integrity_claim_on(store, &id, JobKind::ArtifactVerify, now, node).await
}

#[tokio::test]
async fn background_integrity_repairs_retain_producers_and_stop_after_one_rebuild() {
    use plurx_core::store::background_jobs_integrity::VerifyTranscode;
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "integrity-repair").await;
        let file = store.get_file(file_id).await.expect("file").expect("file");
        let id = uuid::Uuid::new_v4().to_string();
        let input = preparation_request(&file, &id, 1000);
        let producer = input.payload.clone();
        store.enqueue_job(input).await.expect("producer");
        let token = integrity_claim(&store, &id, JobKind::TranscodePrepare, 1000).await;
        let recipe = "a".repeat(64);
        let digest = "d".repeat(64);
        store
            .publish_transcode_job(PublishTranscodeJob {
                token,
                output: TranscodeJobOutput {
                    recipe_hash: recipe.clone(),
                    recipe_version: 1,
                    relative_dir: "aa/first".into(),
                    bytes: 100,
                    expected_previous_bytes: None,
                    manifest_digest: digest.clone(),
                },
                now_ms: 1001,
            })
            .await
            .expect("publication");
        // Retire execution receipts while the cache locator keeps the artifact alive.
        let now = 700_000_000;
        for _ in 0..4 {
            store.maintain_jobs(now).await.expect("retention");
        }
        assert!(
            store
                .background_job(&id)
                .await
                .expect("retired producer")
                .is_none(),
            "{backend}"
        );
        let location = store
            .transcode_verification_candidates("node-a", None)
            .await
            .expect("locations")
            .remove(0);
        let key = format!("transcode:{recipe}:{digest}");
        let token = integrity_verify_claim(&store, &key, now).await;
        let verify = VerifyTranscode {
            token,
            recipe_hash: recipe.clone(),
            manifest_digest: digest.clone(),
            relative_dir: location.relative_dir,
            publication_generation: location.publication_generation,
            valid: false,
            next_object_index: 0,
            now_ms: now + 1,
        };
        let mut stale = verify.clone();
        stale.publication_generation += 1;
        assert!(matches!(
            store
                .verify_transcode_job(stale)
                .await
                .expect("generation fence"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(store
            .artifact_repairs(false)
            .await
            .expect("no plan")
            .is_empty());
        assert!(store
            .cache_hit(&recipe, "node-a")
            .await
            .expect("still present")
            .is_some());
        assert!(matches!(
            store
                .verify_transcode_job(verify.clone())
                .await
                .expect("corruption"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store
                .verify_transcode_job(verify)
                .await
                .expect("lost acknowledgement"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        assert!(store
            .cache_hit(&recipe, "node-a")
            .await
            .expect("retired locator")
            .is_none());
        let mut plan = store
            .artifact_repairs(true)
            .await
            .expect("repair")
            .remove(0);
        assert_eq!(
            serde_json::to_value(plan.producer_payload.clone()).expect("stored producer"),
            serde_json::to_value(Some(producer)).expect("original producer"),
            "{backend}: producer survives history"
        );
        assert_eq!(plan.phase, "copy");
        assert!(matches!(
            store
                .enqueue_artifact_repair(plan.clone(), now + 2)
                .await
                .expect("no copy opportunity"),
            EnqueueOutcome::NoDemand
        ));
        assert!(matches!(
            store
                .enqueue_artifact_repair(plan.clone(), now + 2)
                .await
                .expect("stale replay"),
            EnqueueOutcome::NoDemand
        ));
        plan = store.artifact_repairs(true).await.expect("build").remove(0);
        assert_eq!(plan.phase, "build");
        let EnqueueOutcome::Accepted { job_id: build, .. } = store
            .enqueue_artifact_repair(plan, now + 5)
            .await
            .expect("rebuild")
        else {
            panic!("{backend}: build admission")
        };
        let token = integrity_claim(&store, &build, JobKind::TranscodePrepare, now + 5).await;
        store
            .settle_job(SettleJob {
                token,
                settlement: JobSettlement::Fail {
                    error_code: "producer_failed".into(),
                },
                now_ms: now + 6,
            })
            .await
            .expect("build exhausted");
        store.maintain_jobs(now + 7).await.expect("terminal repair");
        assert!(store
            .artifact_repairs(true)
            .await
            .expect("no automatic loop")
            .is_empty());
        assert_eq!(
            store
                .artifact_repairs(false)
                .await
                .expect("visible failure")[0]
                .phase,
            "failed"
        );
        for _ in 0..3 {
            store.maintain_jobs(now + 8).await.expect("repeat upkeep");
        }
        assert!(store
            .artifact_repairs(true)
            .await
            .expect("still stopped")
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn background_artwork_verification_repairs_copy_then_rebuild_then_deliver() {
    artwork_repair_contract(false).await;
}
#[tokio::test]
async fn background_repair_rebuilds_after_last_holder_disappears_after_admission() {
    artwork_repair_contract(true).await;
}
async fn artwork_repair_contract(holder_disappears: bool) {
    use plurx_core::store::{
        background_jobs_artwork::{
            ArtworkLocation, ArtworkVariantSpec, PublishArtworkJob, ARTWORK_PIPELINE,
        },
        background_jobs_integrity::VerifyArtwork,
    };
    for_each_backend(move |store, backend| async move {
        let spec = ArtworkVariantSpec {
            source_name: "poster.png".into(),
            source_sha256: "a".repeat(64),
            width: 300,
            format: "png".into(),
            pipeline: ARTWORK_PIPELINE.into(),
        };
        let key = spec.artifact_key();
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_artwork_demand(spec.clone(), "node-a", 1000)
            .await
            .expect("demand")
        else {
            panic!("{backend}: demand")
        };
        let token = integrity_claim(&store, &job_id, JobKind::ArtworkDerivative, 1000).await;
        let mut location = ArtworkLocation {
            artifact_key: key.clone(),
            node_id: "node-a".into(),
            spec,
            blob_sha256: "b".repeat(64),
            bytes: 100,
            built_by_node_id: "node-a".into(),
            built_at_ms: 1001,
            verified_at_ms: 1001,
        };
        store
            .publish_artwork_job(PublishArtworkJob {
                token,
                location: location.clone(),
                now_ms: 1001,
            })
            .await
            .expect("original");
        // Retain one advertised peer. The copy phase below models that peer
        // failing actual byte verification, so exactly one rebuild may follow.
        let peer_id = uuid::Uuid::new_v4().to_string();
        store
            .enqueue_job(EnqueueJob {
                id: peer_id.clone(),
                payload: JobPayload::ArtifactHydrate {
                    artifact_key: format!("artwork:{key}"),
                    target_node_id: "node-b".into(),
                },
                dedupe_key: peer_id.clone(),
                priority: 0,
                not_before_ms: 1001,
                now_ms: 1001,
                request: JobRequest {
                    scope: "integrity-peer".into(),
                    request_id: peer_id.clone(),
                    request_digest: "a".repeat(64),
                    consumer_kind: "artifact_copy".into(),
                    consumer_ref: peer_id.clone(),
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("peer copy");
        let token =
            integrity_claim_on(&store, &peer_id, JobKind::ArtifactHydrate, 1001, "node-b").await;
        let mut peer_location = location.clone();
        peer_location.node_id = "node-b".into();
        assert!(matches!(
            store
                .publish_artwork_job(PublishArtworkJob {
                    token,
                    location: peer_location,
                    now_ms: 1001
                })
                .await
                .expect("peer holder"),
            JobPublishOutcome::Published { .. }
        ));
        let token = integrity_verify_claim(&store, &format!("artwork:{key}"), 1002).await;
        let observation = VerifyArtwork {
            token,
            location: location.clone(),
            valid: false,
            now_ms: 1003,
        };
        let mut forged = observation.clone();
        forged.location.verified_at_ms += 1;
        assert!(matches!(
            store
                .verify_artwork_job(forged)
                .await
                .expect("locator fence"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(matches!(
            store
                .verify_artwork_job(observation.clone())
                .await
                .expect("bad bytes"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store.verify_artwork_job(observation).await.expect("replay"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        assert!(store
            .artwork_locations(&key, 1004)
            .await
            .expect("retired bad holder")
            .iter()
            .all(|holder| holder.node_id != "node-a"));
        for (now, phase, kind) in [
            (1004, "copy", JobKind::ArtifactHydrate),
            (1008, "build", JobKind::ArtworkDerivative),
            (1012, "deliver", JobKind::ArtifactHydrate),
        ] {
            let plan = store.artifact_repairs(true).await.expect("plan").remove(0);
            assert_eq!(plan.phase, phase, "{backend}");
            let EnqueueOutcome::Accepted { job_id, .. } = store
                .enqueue_artifact_repair(plan, now)
                .await
                .expect("repair admission")
            else {
                panic!("{backend}: {phase}")
            };
            if phase == "copy" && holder_disappears {
                store
                    .put_setting("internal.cluster_job_owner_removed.node-b", "1")
                    .await
                    .expect("holder removed");
                store
                    .maintain_jobs(now + 2)
                    .await
                    .expect("repair discovers lost holder");
                assert_eq!(
                    store.artifact_repairs(true).await.expect("rebuild")[0].phase,
                    "build"
                );
                assert_eq!(
                    store
                        .background_job(&job_id)
                        .await
                        .expect("copy")
                        .expect("copy job")
                        .state,
                    JobState::Cancelled
                );
                continue;
            }
            let token = integrity_claim(&store, &job_id, kind, now).await;
            if phase == "copy" {
                store
                    .settle_job(SettleJob {
                        token,
                        settlement: JobSettlement::Fail {
                            error_code: "no_valid_peer".into(),
                        },
                        now_ms: now + 1,
                    })
                    .await
                    .expect("copy failed");
            } else {
                location.verified_at_ms = now + 1;
                assert!(matches!(
                    store
                        .publish_artwork_job(PublishArtworkJob {
                            token,
                            location: location.clone(),
                            now_ms: now + 1
                        })
                        .await
                        .expect("repaired bytes"),
                    JobPublishOutcome::Published { .. }
                ));
            }
            store.maintain_jobs(now + 2).await.expect("advance plan");
        }
        assert!(store
            .artifact_repairs(true)
            .await
            .expect("settled")
            .is_empty());
        assert_eq!(
            store.artifact_repairs(false).await.expect("ready")[0].phase,
            "ready"
        );
        assert_eq!(
            store
                .artwork_locations(&key, 1020)
                .await
                .expect("restored")
                .len(),
            if holder_disappears { 1 } else { 2 }
        );
    })
    .await;
}

#[tokio::test]
async fn background_subtitles_source_changes_cancel_selected_candidates() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "subtitle-obsolete").await;
        let request = store
            .enqueue_or_promote_subtitle_source(
                &super::subtitle_source_stamp(file_id),
                "foreground",
                1_000,
            )
            .await
            .expect("subtitle reconciliation fixture")
            .expect("subtitle reconciliation fixture");
        let admitted = store
            .enqueue_subtitle_job(request, 1_000)
            .await
            .expect("subtitle reconciliation fixture");
        let EnqueueOutcome::Accepted { job_id, .. } = admitted else {
            panic!("{backend}: admission")
        };
        let query = CandidateQuery {
            node_id: "node-a".into(),
            kinds: vec![JobKind::SubtitleExtract],
            after: None,
            now_ms: 1_001,
            limit: 100,
        };
        assert_eq!(
            store
                .job_candidates(query.clone())
                .await
                .expect("subtitle reconciliation fixture")
                .jobs
                .len(),
            1
        );
        let file = store
            .get_file(file_id)
            .await
            .expect("subtitle reconciliation fixture")
            .expect("subtitle reconciliation fixture");
        store
            .upsert_file(
                file.item_id,
                &file.path.to_string_lossy(),
                file.size + 1,
                file.mtime + 1,
                &plurx_core::domain::ProbeResult::default(),
            )
            .await
            .expect("subtitle reconciliation fixture");
        // The source can change after candidate selection. The atomic claim must
        // return an ordinary refusal, not throw the subtitle projection trigger.
        let job = store
            .background_job(&job_id)
            .await
            .expect("subtitle reconciliation fixture")
            .expect("subtitle reconciliation fixture");
        let outcome = store
            .claim_artifact_job(ClaimJob {
                job_id: job_id.clone(),
                expected_revision: job.revision,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleExtract,
                payload_version: 1,
                now_ms: 1_001,
                dispatched_at_ms: 1_001,
            })
            .await
            .expect("obsolete demand is not an ambiguous database error");
        assert!(
            !matches!(outcome, ClaimOutcome::Claimed { .. }),
            "{backend}"
        );
        assert!(store
            .job_candidates(query)
            .await
            .expect("subtitle reconciliation fixture")
            .jobs
            .is_empty());
        store
            .maintain_jobs(1_002)
            .await
            .expect("source-change upkeep");
        let retired = store
            .background_job(&job_id)
            .await
            .expect("subtitle reconciliation fixture")
            .expect("subtitle reconciliation fixture");
        assert_eq!(retired.state, JobState::Cancelled, "{backend}");
        assert_eq!(
            retired.last_error_code.as_deref(),
            Some("subtitle_request_cancelled")
        );
        assert!(store
            .job_attempts(&job_id)
            .await
            .expect("subtitle reconciliation fixture")
            .is_empty());
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn background_subtitles_replicated_ready_orphans_are_refused_and_retired() {
    use plurx_core::store::Store;
    use std::sync::Arc;
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store: Arc<dyn Store> = Arc::new(open_contract_hiqlite_store(&cluster).await);
    let (_, file_id) = seed_file(&store, "subtitle-ready-orphan").await;
    let request = store
        .enqueue_or_promote_subtitle_source(
            &super::subtitle_source_stamp(file_id),
            "foreground",
            1_000,
        )
        .await
        .expect("request")
        .expect("request");
    let EnqueueOutcome::Accepted { job_id, .. } = store
        .enqueue_subtitle_job(request.clone(), 1_000)
        .await
        .expect("admit")
    else {
        panic!("admission")
    };
    let query = CandidateQuery {
        node_id: "node-a".into(),
        kinds: vec![JobKind::SubtitleExtract],
        after: None,
        now_ms: 1_001,
        limit: 100,
    };
    assert_eq!(
        store
            .job_candidates(query.clone())
            .await
            .expect("candidate")
            .jobs
            .len(),
        1
    );
    // Manufacture the historical split projection through the replicated log.
    // Ordinary source changes already cancel both records together.
    let client = hiqlite::Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        super::CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("fixture client");
    client.execute("UPDATE analysis_requests SET state='ready', result_cache_key='published-source' WHERE request_id=$1",
        hiqlite::params!(request.request_id.clone())).await.expect("historical ready demand");
    // v61 differs only in this trigger. Upgrade the exact predecessor with the
    // historical orphan present, rather than reconstructing unrelated v10 DDL.
    drop(store);
    let old_projection = include_str!("../../src/store/background_jobs_subtitle.sql")
        .split("-- next statement\n")
        .find(|sql| sql.contains("CREATE TRIGGER IF NOT EXISTS background_subtitle_settled"))
        .expect("predecessor projection trigger");
    for result in client
        .txn(vec![
            (
                "DROP TRIGGER background_subtitle_settled".to_owned(),
                hiqlite::params!(),
            ),
            (old_projection.to_owned(), hiqlite::params!()),
            (
                "UPDATE cluster_meta SET schema_version=61 WHERE singleton=1".to_owned(),
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("install predecessor fixture")
    {
        result.expect("predecessor statement");
    }
    let store: Arc<dyn Store> = Arc::new(
        plurx_core::store::HiqliteAuthStore::open_or_migrate(
            client.clone(),
            &cluster._root.path().join("subtitle-upgrade-telemetry.db"),
        )
        .await
        .expect("upgrade v61 to current schema"),
    );
    let job = store
        .background_job(&job_id)
        .await
        .expect("job")
        .expect("job");
    let outcome = store
        .claim_artifact_job(ClaimJob {
            job_id: job_id.clone(),
            expected_revision: job.revision,
            node_id: "node-a".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::SubtitleExtract,
            payload_version: 1,
            now_ms: 1_001,
            dispatched_at_ms: 1_001,
        })
        .await
        .expect("obsolete demand must not raise an ambiguous SQL error");
    assert!(matches!(outcome, ClaimOutcome::Contended));
    assert!(store
        .job_candidates(query)
        .await
        .expect("candidates")
        .jobs
        .is_empty());
    assert!(store.maintain_jobs(1_002).await.expect("reconcile"));
    let retired = store
        .background_job(&job_id)
        .await
        .expect("read")
        .expect("retained job");
    assert_eq!(retired.state, JobState::Cancelled);
    assert_eq!(
        retired.last_error_code.as_deref(),
        Some("subtitle_demand_obsolete")
    );
    let preserved = store
        .analysis_request(&request.request_id)
        .await
        .expect("read")
        .expect("retained request");
    assert_eq!(preserved.state, "ready");
    assert_eq!(preserved.result_cache_key, "published-source");
    assert!(store
        .job_attempts(&job_id)
        .await
        .expect("no fabricated attempt")
        .is_empty());
    assert!(!store.maintain_jobs(1_003).await.expect("idle upkeep"));
}

/// The seeded settled rows are UUID-shaped, as the job id validator requires.
#[cfg(feature = "hiqlite-contract-tests")]
fn history_id(i: usize) -> String {
    format!("00000000-0000-4000-8000-{i:012}")
}

#[cfg(feature = "hiqlite-contract-tests")]
fn settled_history_fixture(now_ms: i64) -> EnqueueJob {
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::FragmentIndexBuild {
            file_id: 1,
            source_generation: "source:1".to_owned(),
            source_size: 100,
            source_mtime: 1,
            source_sha256: "d".repeat(64),
            cache_key: "c".repeat(64),
            pipeline_digest: "a".repeat(64),
        },
        dedupe_key: "fragment:history".to_owned(),
        priority: 1,
        not_before_ms: now_ms,
        now_ms,
        request: JobRequest {
            scope: "user:1".to_owned(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "b".repeat(64),
            consumer_kind: "analysis".to_owned(),
            consumer_ref: "analysis:1".to_owned(),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn background_jobs_replicated_settled_history_yields_after_the_v63_upgrade() {
    use plurx_core::store::Store;
    use std::sync::Arc;
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store: Arc<dyn Store> = Arc::new(open_contract_hiqlite_store(&cluster).await);
    let template = settled_history_fixture(1_000);
    store.enqueue_job(template.clone()).await.expect("enqueue");
    store
        .cancel_job(CancelJob {
            job_id: template.id.clone(),
            now_ms: 1_001,
        })
        .await
        .expect("settle the template");
    let client = hiqlite::Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        super::CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("fixture client");
    // Put the v62 enqueue and upkeep triggers back and stamp v62, so the
    // migration every deployed node takes is the one under test.
    drop(store);
    let base = include_str!("../../src/store/background_jobs_schema.sql");
    let predecessor = |name: &str| {
        base.split("-- next statement\n")
            .find(|sql| sql.contains(&format!("CREATE TRIGGER IF NOT EXISTS {name}\n")))
            .expect("predecessor trigger")
            .to_owned()
    };
    for result in client
        .txn(vec![
            ("DROP TRIGGER background_job_enqueue_command".to_owned(), hiqlite::params!()),
            (predecessor("background_job_enqueue_command"), hiqlite::params!()),
            ("DROP TRIGGER background_job_maintenance_command".to_owned(), hiqlite::params!()),
            (predecessor("background_job_maintenance_command"), hiqlite::params!()),
            ("UPDATE cluster_meta SET schema_version=62 WHERE singleton=1".to_owned(), hiqlite::params!()),
            ("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<9999)
              INSERT INTO background_jobs (id, kind, payload_version, payload_json, dedupe_key, priority,
                  state, not_before_ms, created_at_ms, updated_at_ms)
              SELECT '00000000-0000-4000-8000-'||printf('%012d', i), kind, payload_version, payload_json, 'history-'||i, priority,
                  'succeeded', 1000, 1000, 2000+i FROM n, background_jobs WHERE id = $1".to_owned(),
             hiqlite::params!(template.id.clone())),
        ])
        .await
        .expect("install predecessor fixture")
    {
        result.expect("predecessor statement");
    }
    let store: Arc<dyn Store> = Arc::new(
        plurx_core::store::HiqliteAuthStore::open_or_migrate(
            client.clone(),
            &cluster._root.path().join("retention-upgrade-telemetry.db"),
        )
        .await
        .expect("upgrade v62 to current schema"),
    );
    let fresh = settled_history_fixture(20_000);
    assert!(
        matches!(
            store.enqueue_job(fresh.clone()).await.expect("admit"),
            EnqueueOutcome::Accepted { .. }
        ),
        "settled history must not refuse live work on the replicated store"
    );
    assert!(store
        .background_job(&template.id)
        .await
        .expect("read")
        .is_none());
    assert!(store
        .background_job(&history_id(127))
        .await
        .expect("read")
        .is_none());
    assert!(store
        .background_job(&history_id(128))
        .await
        .expect("read")
        .is_some());
    assert!(store
        .background_job(&fresh.id)
        .await
        .expect("read")
        .is_some());
    assert!(store.maintain_jobs(20_001).await.expect("pressure upkeep"));
    assert!(store
        .background_job(&history_id(255))
        .await
        .expect("read")
        .is_none());
    assert!(store
        .background_job(&history_id(256))
        .await
        .expect("read")
        .is_some());
}
