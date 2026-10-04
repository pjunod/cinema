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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishEncodedOutputJob {
    pub token: JobToken,
    pub intent: super::background_jobs::EncodedOutputIntent,
    pub output: CopyOutputJobOutput,
    pub now_ms: i64,
}

/// Identical ownership and terminal-effect contract, but a distinct closed
/// kind/version and typed intent. The shared command trigger settles jobs and
/// waiters; its name does not grant Copy execution authority.
pub(super) fn encoded_publication_sql() -> String {
    PUBLISH_COPY_OUTPUT_SQL
        .replace(
            "SELECT request.*, job.id, job.state, job.result_ref,",
            "SELECT request.*, job.id, job.state, job.result_ref, job.kind AS publication_kind, job.payload_version AS publication_version, job.payload_json AS publication_payload,",
        )
        .replace(
            "state='succeeded' AND same_attempt=1 AND result_ref=artifact",
            "state='succeeded' AND publication_kind='encoded_output_prepare' AND publication_version=1 AND json_extract(publication_payload,'$.encoded_output_version')=1 AND json_extract(publication_payload,'$.intent')=json_extract(body,'$.intent') AND same_attempt=1 AND result_ref=artifact",
        )
        .replace(
            "job.kind='copy_output_prepare'",
            "job.kind='encoded_output_prepare'",
        )
        .replace(
            "'$.copy_output_version') IN (1,2)",
            "'$.encoded_output_version')=1",
        )
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
   AND json_extract(job.payload_json,'$.copy_output_version') IN (1,2)
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

    #[test]
    fn encoded_output_upgrade_preserves_copy_publication_and_adds_exact_source_guards() {
        // The encoded-output step and its predecessor on the composed chain
        // (v96 candidate recovery -> v97 encoded output).
        let encoded = super::super::background_jobs::ENCODED_OUTPUT_SQLITE_SCHEMA;
        assert_eq!(encoded, 97);
        let predecessor = encoded - 1;
        let base = tempfile::tempdir().expect("owned upgrade");
        let path = base.path().join("store.sqlite");
        let store = crate::store::SqliteStore::open(&path).expect("fresh schema");
        drop(store);
        let connection = rusqlite::Connection::open(&path).expect("connection");
        let publication: String = connection.query_row("SELECT sql FROM sqlite_master WHERE name='background_job_publish_copy_output_command'", [], |row| row.get(0)).expect("incumbent publication");
        connection
            .execute_batch(super::super::background_jobs::COPY_OUTPUT_SCHEMA)
            .expect("exact predecessor source guards");
        crate::queue_fixture::remove_jellyfin_compatibility_schema(&connection);
        connection
            .execute_batch(&format!(
                "DROP TRIGGER background_job_encoded_output_target; PRAGMA user_version={predecessor};"
            ))
            .expect("predecessor checkpoint");
        drop(connection);
        let upgraded =
            crate::store::SqliteStore::open(&path).expect("real predecessor to encoded migration");
        drop(upgraded);
        let connection = rusqlite::Connection::open(&path).expect("upgraded connection");
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            // The chain continues past the encoded step (Jellyfin v98–v101).
            crate::store::SQLITE_SCHEMA_VERSION
        );
        assert_eq!(connection.query_row("SELECT sql FROM sqlite_master WHERE name='background_job_publish_copy_output_command'", [], |row| row.get::<_,String>(0)).expect("publication unchanged"), publication);
        for name in [
            "background_job_source_changed",
            "background_job_source_deleted",
        ] {
            let guard: String = connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name=?1",
                    [name],
                    |row| row.get(0),
                )
                .expect("source guard");
            for kind in [
                "transcode_prepare",
                "fragment_index_build",
                "copy_output_prepare",
                "encoded_output_prepare",
            ] {
                assert!(guard.contains(kind), "{name} retains {kind}");
            }
        }
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_master WHERE name='background_job_encoded_output_target'", [], |row| row.get::<_,i64>(0)).expect("exact target guard"), 1);
    }

    #[test]
    fn encoded_settlement_reconciliation_refuses_other_kind_version_or_intent() {
        let base = tempfile::tempdir().expect("owned reconciliation store");
        let path = base.path().join("store.sqlite");
        let _store = crate::store::SqliteStore::open(&path).expect("schema");
        let connection = rusqlite::Connection::open(path).expect("connection");
        let artifact = serde_json::json!({"artifact_id":"artifact", "output_identity":"identity"});
        let intent = serde_json::json!({"selected":"actual"});
        for (index, kind, version, stored_intent, expected) in [
            (
                0,
                "copy_output_prepare",
                1,
                intent.clone(),
                "lost_ownership",
            ),
            (
                1,
                "encoded_output_prepare",
                2,
                intent.clone(),
                "lost_ownership",
            ),
            (
                2,
                "encoded_output_prepare",
                1,
                serde_json::json!({"selected":"foreign"}),
                "lost_ownership",
            ),
            (
                3,
                "encoded_output_prepare",
                1,
                intent.clone(),
                "already_published",
            ),
        ] {
            let id = format!("reconcile-{index}");
            let claim = format!("claim-{index}");
            let payload =
                serde_json::json!({"encoded_output_version":version,"intent":stored_intent});
            connection.execute("INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,result_ref,created_at_ms,updated_at_ms) VALUES(?1,?2,1,?3,?1,1,'succeeded',0,?4,0,0)",
                rusqlite::params![id,kind,payload.to_string(),artifact.to_string()]).expect("historical job");
            connection.execute("INSERT INTO background_job_attempts(job_id,fence,claim_id,owner_node_id,owner_boot_id,started_at_ms,resolve_until_ms,finished_at_ms,outcome) VALUES(?1,1,?2,'node','boot',0,1000,1,'succeeded')",
                rusqlite::params![id,claim]).expect("same historical attempt");
            let request = serde_json::json!({"token":{"job_id":id,"node_id":"node","boot_id":"boot","claim_id":claim,"fence":1},"intent":intent,"output":artifact,"now_ms":2});
            let result: String = connection
                .query_row(&encoded_publication_sql(), [request.to_string()], |row| {
                    row.get(0)
                })
                .expect("actual settlement SQL");
            let result: serde_json::Value = serde_json::from_str(&result).expect("typed result");
            assert_eq!(result["outcome"], expected);
        }
    }

    #[test]
    fn encoded_output_intent_refuses_unpaired_burn_and_forged_candidate_or_kind() {
        let document = serde_json::json!({
            "target_node_id":"node-a", "target_height":360,
            "requested_height":360, "copy_for_burn":null,
            "audio_index":null, "audio_offset_ms":0,
            "audio_claim":{"decoders":["aac"],"sinks":[]},
            "audio_delivery":{"action":{"kind":"none"},"downmix":null,"reason":"no_audio"},
            "subtitle_burn":null,"subtitle_digest":null,
            "hdr10_requested":false,"grade":"sdr","normalized_geometry":false,
            "profile":null,"width":640,"height":360,
            "plan_digest":"a".repeat(64),"executable_digest":"b".repeat(64),
            "engine_digest":"c".repeat(64),"candidate_id":null,"candidate_digest":null
        });
        let valid: EncodedOutputIntent =
            serde_json::from_value(document.clone()).expect("closed intent");
        assert!(valid.valid());
        for (field, value) in [
            ("subtitle_burn", serde_json::json!(1)),
            ("subtitle_digest", serde_json::json!("d".repeat(64))),
            ("requested_height", serde_json::Value::Null),
            ("copy_for_burn", serde_json::json!([false, false, false])),
            ("audio_offset_ms", serde_json::json!(15001)),
            ("candidate_digest", serde_json::json!(vec![1_u8; 32])),
        ] {
            let mut changed = document.clone();
            changed[field] = value;
            let changed: EncodedOutputIntent =
                serde_json::from_value(changed).expect("typed refusal");
            assert!(
                !changed.valid(),
                "{field} cannot acquire execution authority"
            );
        }
        let mut unknown = document;
        unknown["retained_authority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EncodedOutputIntent>(unknown).is_err());
    }
    use crate::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    use crate::store::{background_jobs::*, sqlite::SqliteStore, LibraryStore, MediaStore};

    #[tokio::test]
    async fn encoded_output_closed_intent_and_exact_settlement_preserve_copy_history() {
        let directory = tempfile::tempdir().expect("encoded store");
        let path = directory.path().join("store.sqlite");
        let store = SqliteStore::open(&path).expect("current store");
        // Exercise the new migration's exact statements without fabricating
        // a preceding, still separately owned recovery migration version.
        let connection = rusqlite::Connection::open(&path).expect("guard connection");
        connection
            .execute_batch(super::super::background_jobs::ENCODED_OUTPUT_SCHEMA)
            .expect("encoded guards");
        let library = store
            .create_library(&NewLibrary {
                name: "encoded".into(),
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
                title: "encoded".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = store
            .upsert_file(item, "/media/encoded.mkv", 100, 1, &ProbeResult::default())
            .await
            .expect("file");
        let intent = EncodedOutputIntent {
            target_node_id: "node-a".into(),
            target_height: 360,
            requested_height: Some(360),
            copy_for_burn: None,
            audio_index: None,
            audio_offset_ms: 0,
            audio_claim: crate::playback::audio::AudioClaim {
                decoders: vec!["aac".into()],
                sinks: vec![],
            },
            audio_delivery: crate::playback::audio::AudioDelivery {
                action: crate::playback::audio::AudioAction::None,
                downmix: None,
                reason: "no_audio".into(),
            },
            subtitle_burn: None,
            subtitle_digest: None,
            hdr10_requested: false,
            grade: crate::transcode::OutputGrade::Sdr,
            normalized_geometry: false,
            profile: None,
            width: 640,
            height: 360,
            plan_digest: "a".repeat(64),
            executable_digest: "b".repeat(64),
            engine_digest: "c".repeat(64),
            candidate_id: None,
            candidate_digest: None,
        };
        assert!(intent.valid());
        let mut invalid = intent.clone();
        invalid.candidate_digest = Some([1; 32]);
        assert!(!invalid.valid());
        let id = uuid::Uuid::new_v4().to_string();
        let payload = JobPayload::EncodedOutputPrepare {
            encoded_output_version: 1,
            file_id: file,
            source_generation: "source:1".into(),
            source_size: 100,
            source_mtime: 1,
            source_object_version: "object:1".into(),
            policy_generation: "policy:1".into(),
            intent: intent.clone(),
            candidate_catalog: None,
            scratch_bytes: 1000,
            reason: "recent_demand".into(),
        };
        let mut unsupported = payload.clone();
        if let JobPayload::EncodedOutputPrepare {
            encoded_output_version,
            ..
        } = &mut unsupported
        {
            *encoded_output_version = 2;
        }
        assert!(unsupported.validate().is_err());
        assert!(matches!(
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload,
                    dedupe_key: "encoded:1".into(),
                    priority: 1,
                    not_before_ms: 1000,
                    now_ms: 1000,
                    request: JobRequest {
                        scope: "encoded".into(),
                        request_id: "encoded:1".into(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "encoded_output".into(),
                        consumer_ref: file.to_string(),
                        target_node_id: Some("node-a".into()),
                        deadline_ms: None,
                        retain_identity: false
                    },
                })
                .await
                .expect("enqueue"),
            EnqueueOutcome::Accepted { .. }
        ));
        let queued = store.background_job(&id).await.expect("job").expect("row");
        let claimed = match store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: queued.revision,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::EncodedOutputPrepare,
                payload_version: 1,
                now_ms: 1001,
                dispatched_at_ms: 1001,
            })
            .await
            .expect("claim")
        {
            ClaimOutcome::Claimed { job } => job,
            other => panic!("claim {other:?}"),
        };
        let publication = PublishEncodedOutputJob {
            token: claimed.token.expect("token"),
            intent,
            output: CopyOutputJobOutput {
                artifact_id: uuid::Uuid::new_v4().to_string(),
                output_identity: "e".repeat(64),
                source_object_version: "object:1".into(),
                wire_bytes: 100,
                duration_micros: 1_000_000,
                average_bps: 800,
                peak_bps: 1000,
            },
            now_ms: 1002,
        };
        let mut foreign = publication.clone();
        foreign.intent.audio_offset_ms = 1;
        assert!(matches!(
            store
                .publish_encoded_output_job(foreign)
                .await
                .expect("foreign"),
            JobPublishOutcome::LostOwnership
        ));
        assert!(matches!(
            store
                .publish_encoded_output_job(publication.clone())
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        assert!(matches!(
            store
                .publish_encoded_output_job(publication)
                .await
                .expect("reconcile"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        assert_eq!(
            store
                .background_job(&id)
                .await
                .expect("job")
                .expect("row")
                .state,
            JobState::Succeeded
        );
    }

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
                candidate_catalog: None,
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
