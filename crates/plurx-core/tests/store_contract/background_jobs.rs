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
