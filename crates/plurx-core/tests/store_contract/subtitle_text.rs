use super::{for_each_backend, seed_file};
use plurx_core::store::background_jobs::*;
use plurx_core::store::shared_caption_read::{
    GrantCaptionPrincipal, GrantCaptionQuery, GrantCaptionRead,
};
use plurx_core::store::subtitle_text_translation::{PublishTextSlice, TextTranslationCue};

#[tokio::test]
async fn subtitle_text_contract_preserves_receipts_and_immutable_cues() {
    for_each_backend(|store, backend| async move {
        let (viewer_id, file_id) = seed_file(&store, "text-contract").await;
        let admin = store
            .create_user("text-contract-admin", "hash", true)
            .await
            .expect("admin");
        let file = store
            .get_file(file_id)
            .await
            .expect("file lookup")
            .expect("file");
        store
            .upsert_file(
                file.item_id,
                &file.path.to_string_lossy(),
                file.size,
                file.mtime,
                &plurx_core::domain::ProbeResult {
                    raw_json: Some("{}".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("source probe");
        store
            .put_setting(
                "subtitles.text_translation.config.v1",
                r#"{"version":1,"enabled":true}"#,
            )
            .await
            .expect("enabled text translation");
        let source = JobPayload::SubtitleTextTranslate {
            file_id,
            source_size: file.size,
            source_mtime: file.mtime,
            subtitle_index: 0,
            input_sha256: "a".repeat(64),
            source_language: "de".into(),
            target_language: "en".into(),
            engine_sha256: "e".repeat(64),
            model_id: "opus-mt-mul-en".into(),
            model_sha256: "b".repeat(64),
            adapter_contract: "candle-marian-0.11-v1".into(),
            target_node_id: "text-worker".into(),
            admin_user_id: admin.id,
        };
        let job_id = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let digest = "d".repeat(64);
        assert!(store
            .text_translation_request_receipt(admin.id, &request_id, &digest)
            .await
            .expect("absent receipt")
            .is_none());
        let queued = store
            .enqueue_job(EnqueueJob {
                id: job_id.clone(),
                payload: source.clone(),
                dedupe_key: "text-contract".into(),
                priority: 1,
                not_before_ms: 1000,
                now_ms: 1000,
                request: JobRequest {
                    scope: format!("subtitle-text-user:{}", admin.id),
                    request_id: request_id.clone(),
                    request_digest: digest.clone(),
                    consumer_kind: "subtitle_text_translation".into(),
                    consumer_ref: "contract".into(),
                    target_node_id: Some("text-worker".into()),
                    deadline_ms: Some(86400000),
                    retain_identity: false,
                },
            })
            .await
            .expect("text admission");
        assert!(
            matches!(queued, EnqueueOutcome::Accepted { .. }),
            "{backend}: {queued:?}"
        );
        let receipt = store
            .text_translation_request_receipt(admin.id, &request_id, &digest)
            .await
            .expect("receipt")
            .expect("durable receipt");
        assert_eq!(receipt["outcome"], "existing", "{backend}");
        assert_eq!(receipt["job_id"], job_id, "{backend}");
        assert_eq!(
            store
                .text_translation_request_receipt(admin.id, &request_id, &"f".repeat(64))
                .await
                .expect("receipt conflict")
                .expect("conflict")["outcome"],
            "conflict",
            "{backend}"
        );
        assert!(
            store
                .text_translation_request_receipt(viewer_id, &request_id, &digest)
                .await
                .expect("viewer receipt denied")
                .is_none(),
            "{backend}"
        );
        let job = store
            .background_job(&job_id)
            .await
            .expect("job")
            .expect("queued job");
        let claimed = store
            .claim_artifact_job(ClaimJob {
                job_id: job_id.clone(),
                expected_revision: job.revision,
                node_id: "text-worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleTextTranslate,
                payload_version: 1,
                now_ms: 1100,
                dispatched_at_ms: 1100,
            })
            .await
            .expect("claim");
        let ClaimOutcome::Claimed { job } = claimed else {
            panic!("{backend}: claim {claimed:?}")
        };
        let input = PublishTextSlice {
            token: job.token.expect("claim token"),
            source: source.clone(),
            cues: vec![TextTranslationCue {
                cue_index: 0,
                input_sha256: "c".repeat(64),
                id: "original-cue".into(),
                start_ms: 0,
                end_ms: 1000,
                text: "Translated caption".into(),
            }],
            now_ms: 1200,
        };
        assert!(
            store
                .publish_text_translation_slice(input.clone())
                .await
                .expect("publish"),
            "{backend}"
        );
        assert!(
            store
                .publish_text_translation_slice(input.clone())
                .await
                .expect("exact replay"),
            "{backend}"
        );
        let mut conflict = input.clone();
        conflict.cues[0].text = "Changed caption".into();
        assert!(
            !store
                .publish_text_translation_slice(conflict)
                .await
                .expect("immutable conflict"),
            "{backend}"
        );
        assert_eq!(
            store
                .read_text_translation_cues(source.clone())
                .await
                .expect("durable cues"),
            input.cues,
            "{backend}"
        );
        // A Source grant reads only committed progressive assets. Merely
        // reading with a valid principal cannot create demand or a work job.
        let before = serde_json::to_value(
            store
                .job_counts(1250)
                .await
                .expect("job counts before grant read"),
        )
        .expect("serializable counts");
        let read = GrantCaptionRead {
            principal: GrantCaptionPrincipal {
                grant_id: uuid::Uuid::new_v4().to_string(),
                binding_sha256: "9".repeat(64),
                file_id,
            },
            now_ms: 1250,
            query: GrantCaptionQuery::Assets { after: None },
        };
        assert!(
            store
                .read_shared_captions(read.clone())
                .await
                .expect("empty grant projection")
                .is_empty(),
            "{backend}"
        );
        let mut invalid = read;
        invalid.principal.binding_sha256 = "invalid".into();
        assert!(
            store.read_shared_captions(invalid).await.is_err(),
            "{backend}"
        );
        assert_eq!(
            serde_json::to_value(
                store
                    .job_counts(1250)
                    .await
                    .expect("job counts after grant read")
            )
            .expect("serializable counts"),
            before,
            "{backend}: grant read cannot enqueue work"
        );
        store
            .settle_job(SettleJob {
                token: input.token.clone(),
                settlement: JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: 1400,
                },
                now_ms: 1300,
            })
            .await
            .expect("retire old claim");
        let mut stale = input.clone();
        stale.cues[0].cue_index = 1;
        stale.now_ms = 1350;
        assert!(
            !store
                .publish_text_translation_slice(stale)
                .await
                .expect("stale claim refused"),
            "{backend}"
        );
        assert_eq!(
            store
                .read_text_translation_cues(source)
                .await
                .expect("preserved immutable cues"),
            input.cues,
            "{backend}"
        );
    })
    .await;
}
