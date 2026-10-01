//! Private completed-copy publication. A locator is a hint, never restore authority.
use super::background_jobs::{CopyOutputIntent, JobToken};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyOutputJobOutput {
    pub artifact_id: String,
    pub output_identity: String,
    pub source_object_version: String,
    pub wire_bytes: i64,
    pub duration_micros: i64,
    pub average_bps: u64,
    pub peak_bps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishCopyOutputJob {
    pub token: JobToken,
    pub intent: CopyOutputIntent,
    pub output: CopyOutputJobOutput,
    pub now_ms: i64,
}

pub(super) const PUBLISH_COPY_OUTPUT_SQL: &str = r#"
WITH request AS (
 SELECT json($1) AS body, json_extract($1,'$.output') AS artifact
), snapshot AS (
 SELECT request.*, job.id, job.state, job.result_ref,
  CASE WHEN job.kind='copy_output_prepare' AND job.payload_version=1
   AND job.state='running' AND job.target_node_id=job.owner_node_id
   AND job.owner_node_id=json_extract(body,'$.token.node_id')
   AND job.owner_boot_id=json_extract(body,'$.token.boot_id')
   AND job.claim_id=json_extract(body,'$.token.claim_id')
   AND job.fence=json_extract(body,'$.token.fence')
   AND job.revision=json_extract(body,'$.token.revision')
   AND job.lease_expires_ms=json_extract(body,'$.token.lease_expires_ms')
   AND job.lease_expires_ms>json_extract(body,'$.now_ms')
   AND job.revision<9223372036854775807
   AND json_extract(job.payload_json,'$.copy_output_version')=1
   AND json_extract(job.payload_json,'$.intent')=json_extract(body,'$.intent')
   AND json_extract(job.payload_json,'$.source_object_version')=json_extract(body,'$.output.source_object_version')
   AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||job.owner_node_id)
  THEN 1 ELSE 0 END AS owns,
  CASE WHEN file.id IS NOT NULL AND file.size=json_extract(job.payload_json,'$.source_size')
   AND file.mtime=json_extract(job.payload_json,'$.source_mtime') THEN 1 ELSE 0 END AS source_matches,
  CASE WHEN EXISTS(SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id=job.id
   AND attempt.fence=json_extract(body,'$.token.fence')
   AND attempt.claim_id=json_extract(body,'$.token.claim_id')
   AND attempt.owner_node_id=json_extract(body,'$.token.node_id')
   AND attempt.owner_boot_id=json_extract(body,'$.token.boot_id')) THEN 1 ELSE 0 END AS same_attempt
 FROM request LEFT JOIN background_jobs job ON job.id=json_extract(body,'$.token.job_id')
 LEFT JOIN files file ON file.id=json_extract(job.payload_json,'$.file_id')
), classified AS (
 SELECT *, CASE WHEN state='succeeded' AND same_attempt=1 AND result_ref=artifact THEN 'already_published'
  WHEN owns=0 THEN 'lost_ownership' WHEN source_matches=0 THEN 'source_changed' ELSE 'published' END AS outcome FROM snapshot
)
INSERT INTO background_job_commands(id,operation,request_json,result_json)
SELECT json_extract(body,'$.token.claim_id'),'publish_copy_output',body,
 CASE WHEN outcome IN ('published','already_published')
  THEN json_object('outcome',outcome,'job_id',id,'result_ref',artifact||'')
  ELSE json_object('outcome',outcome) END
FROM classified RETURNING result_json
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use crate::store::{background_jobs::*, sqlite::SqliteStore, LibraryStore, MediaStore};

    #[tokio::test]
    async fn copy_output_publication_fences_source_intent_token_and_reconciles_only_exact_result() {
        let store = SqliteStore::open_in_memory().expect("copy publication store");
        let library = store
            .create_library(&NewLibrary {
                name: "Copy".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/media".into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Source".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = store
            .upsert_file(item, "/media/copy.mkv", 100, 1, &ProbeResult::default())
            .await
            .expect("file");
        let intent = CopyOutputIntent {
            target_node_id: "node-a".into(),
            audio_index: None,
            audio_offset_ms: 0,
            audio_claim: Some(crate::playback::audio::AudioClaim {
                decoders: vec!["aac".into()],
                sinks: vec![],
            }),
            audio_delivery: crate::playback::audio::AudioDelivery {
                action: crate::playback::audio::AudioAction::None,
                downmix: None,
                reason: "no_audio".into(),
            },
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
            hdr10_requested: false,
            grade: crate::transcode::OutputGrade::Sdr,
            normalized_geometry: true,
            profile: None,
            width: 1920,
            height: 1080,
            video_identity: "a".repeat(64),
            pipeline_identity: "b".repeat(64),
        };
        let id = uuid::Uuid::new_v4().to_string();
        let request = EnqueueJob {
            id: id.clone(),
            payload: JobPayload::CopyOutputPrepare {
                copy_output_version: 1,
                file_id: file,
                source_generation: "source:1".into(),
                source_size: 100,
                source_mtime: 1,
                source_object_version: "object:1".into(),
                policy_generation: "copy:1".into(),
                intent: intent.clone(),
                scratch_bytes: 1000,
                reason: "recent_demand".into(),
            },
            dedupe_key: "copy:1".into(),
            priority: 1,
            not_before_ms: 1000,
            now_ms: 1000,
            request: JobRequest {
                scope: "copy".into(),
                request_id: "copy:1".into(),
                request_digest: "c".repeat(64),
                consumer_kind: "copy_output".into(),
                consumer_ref: file.to_string(),
                target_node_id: Some("node-a".into()),
                deadline_ms: None,
                retain_identity: false,
            },
        };
        assert!(matches!(
            store.enqueue_job(request).await.expect("enqueue"),
            EnqueueOutcome::Accepted { .. }
        ));
        let job = match store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::CopyOutputPrepare,
                payload_version: 1,
                now_ms: 1001,
                dispatched_at_ms: 1001,
            })
            .await
            .expect("claim")
        {
            ClaimOutcome::Claimed { job } => job,
            outcome => panic!("unexpected claim {outcome:?}"),
        };
        let publication = PublishCopyOutputJob {
            token: job.token.expect("exact token"),
            intent,
            output: CopyOutputJobOutput {
                artifact_id: uuid::Uuid::new_v4().to_string(),
                output_identity: "d".repeat(64),
                source_object_version: "object:1".into(),
                wire_bytes: 100,
                duration_micros: 1_000_000,
                average_bps: 800,
                peak_bps: 1000,
            },
            now_ms: 1002,
        };
        let mut refused = publication.clone();
        refused.token.claim_id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            store
                .publish_copy_output_job(refused)
                .await
                .expect("foreign claim"),
            JobPublishOutcome::LostOwnership
        ));
        let mut refused = publication.clone();
        refused.intent.audio_offset_ms = 1;
        assert!(matches!(
            store
                .publish_copy_output_job(refused)
                .await
                .expect("foreign logical tuple"),
            JobPublishOutcome::LostOwnership
        ));
        let mut refused = publication.clone();
        refused.output.source_object_version = "object:2".into();
        assert!(matches!(
            store
                .publish_copy_output_job(refused)
                .await
                .expect("foreign object"),
            JobPublishOutcome::LostOwnership
        ));
        let mut refused = publication.clone();
        refused.output.average_bps = 799;
        assert!(
            store.publish_copy_output_job(refused).await.is_err(),
            "incoherent full-mux average refuses"
        );
        assert!(matches!(
            store
                .publish_copy_output_job(publication.clone())
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store
                .publish_copy_output_job(publication.clone())
                .await
                .expect("same-attempt ack"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        let mut contradictory = publication;
        contradictory.output.output_identity = "e".repeat(64);
        assert!(matches!(
            store
                .publish_copy_output_job(contradictory)
                .await
                .expect("contradictory ack"),
            JobPublishOutcome::LostOwnership
        ));
        assert_eq!(
            store
                .background_job(&id)
                .await
                .expect("job")
                .expect("retained job")
                .state,
            JobState::Succeeded
        );
    }
}
