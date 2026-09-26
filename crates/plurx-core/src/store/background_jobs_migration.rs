//! Finite legacy cutover inbox. Only the schema can add entries; runtime
//! drains bounded, atomic pages and leaves capacity refusals pending.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::background_jobs::{EnqueueJob, JobPayload, JobRequest, QueueSql, ENQUEUE_SQL};
use crate::error::StoreError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobMigrationStatus {
    pub accepted: i64,
    pub awaiting_import: i64,
    pub materialized: i64,
    pub failed: i64,
    pub cancelled: i64,
}

#[derive(Deserialize)]
struct LegacyEntry {
    legacy_key: String,
    kind: String,
    snapshot: Value,
}

pub(super) const STATUS_SQL: &str = r#"
SELECT json_object('accepted', COALESCE((SELECT source_count FROM background_job_migration), 0),
    'awaiting_import', COUNT(*) FILTER (WHERE state = 'awaiting_import'),
    'materialized', COUNT(*) FILTER (WHERE state = 'materialized'),
    'failed', COUNT(*) FILTER (WHERE state = 'failed'),
    'cancelled', COUNT(*) FILTER (WHERE state = 'cancelled')) AS result_json
FROM background_job_legacy WHERE json_extract($1, '$.unused') IS NULL
"#;

const PAGE_SQL: &str = r#"
SELECT json_object('legacy_key', legacy_key, 'kind', kind, 'snapshot', json(snapshot_json)) AS result_json
FROM background_job_legacy WHERE state = 'awaiting_import' AND json_extract($1, '$.unused') IS NULL
ORDER BY legacy_key LIMIT 128
"#;

const REJECT_SQL: &str = r#"
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract($1, '$.command_id'), 'legacy_reject', $1, '{}'
WHERE EXISTS (SELECT 1 FROM background_job_legacy WHERE legacy_key = json_extract($1, '$.legacy_key')
    AND state = 'awaiting_import') RETURNING result_json
"#;

fn invalid() -> StoreError {
    StoreError::Task("legacy queue entry has an invalid bounded payload".into())
}
fn number(row: &Value, field: &str) -> Result<i64, StoreError> {
    row[field].as_i64().ok_or_else(invalid)
}
fn string(row: &Value, field: &str) -> Result<String, StoreError> {
    row[field].as_str().map(str::to_owned).ok_or_else(invalid)
}
fn identity(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn canonical_id(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes[..16]);
    uuid::Uuid::from_bytes(id).to_string()
}

fn prepare(entry: &LegacyEntry, now_ms: i64) -> Result<Value, StoreError> {
    let row = &entry.snapshot;
    if row["attempt_errors"]
        .as_str()
        .is_some_and(|s| s.len() > 1980)
        || row["index_diagnostic_json"]
            .as_str()
            .is_some_and(|s| s.len() > 8192)
        || number(row, "attempts")? < 0
    {
        return Err(invalid());
    }
    let (mut request, failures) = if entry.kind == "transcode_prepare" {
        let legacy = crate::domain::NewPretranscodeJob {
            id: string(row, "id")?,
            dedupe_key: string(row, "dedupe_key")?,
            file_id: number(row, "file_id")?,
            source_size: number(row, "source_size")?,
            source_mtime: number(row, "source_mtime")?,
            target_height: number(row, "target_height")?,
            policy_generation: string(row, "policy_generation")?,
            requirements_json: string(row, "requirements_json")?,
            reason: string(row, "reason")?,
            priority: number(row, "priority")?,
            not_before_ms: number(row, "not_before_ms")?,
            created_at_ms: now_ms,
        };
        let mut request = super::background_jobs_pretranscode::enqueue_request(&legacy)?;
        // Keeping the old unique active job ID preserves the existing staging
        // identity. Workers still verify source, recipe and every retained part.
        request.request.scope = "legacy:transcode".into();
        request.request.request_id = identity(&entry.legacy_key);
        (
            request,
            number(row, "attempts")?.saturating_add(i64::from(row["state"] == "running")),
        )
    } else if entry.kind == "fragment_index_build" {
        let job: super::NewClusterFragmentIndexJob =
            serde_json::from_value(row.clone()).map_err(|_| invalid())?;
        let payload = JobPayload::FragmentIndexBuild {
            file_id: job.file_id,
            source_generation: job.source_sha256.clone(),
            source_size: job.source_size,
            source_mtime: job.source_mtime,
            source_sha256: job.source_sha256,
            cache_key: job.cache_key.clone(),
            pipeline_digest: job.pipeline_sha256,
        };
        let dedupe_key = format!("fragment:{}", job.cache_key);
        let request_digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&json!({"payload": payload, "target_node_id": job.target_node_id}))
                .map_err(|_| invalid())?,
        ));
        let analysis_id = row["analysis_request_id"].as_str();
        let request_id = analysis_id
            .map(str::to_owned)
            .unwrap_or_else(|| identity(&entry.legacy_key));
        let request = EnqueueJob {
            id: canonical_id(&dedupe_key),
            payload,
            dedupe_key,
            priority: match job.priority.as_str() {
                "foreground" => 3,
                "forced" => 2,
                _ => 1,
            },
            not_before_ms: job.not_before_ms,
            now_ms,
            request: JobRequest {
                scope: if analysis_id.is_some() {
                    "analysis"
                } else {
                    "legacy:fragment"
                }
                .into(),
                request_id: request_id.clone(),
                request_digest,
                consumer_kind: "fragment_analysis".into(),
                consumer_ref: request_id,
                target_node_id: (!job.target_node_id.is_empty()).then_some(job.target_node_id),
                deadline_ms: None,
                retain_identity: true,
            },
        };
        // Legacy fragment claims already charged the active attempt.
        (request, number(row, "attempts")?)
    } else {
        return Err(invalid());
    };
    request.now_ms = now_ms;
    request.request.deadline_ms = None;
    request.request.retain_identity = true;
    request.validate()?;
    let mut body = serde_json::to_value(request).map_err(|_| invalid())?;
    body["legacy_key"] = entry.legacy_key.clone().into();
    body["legacy_snapshot"] = row.clone();
    body["legacy_failures"] = failures.into();
    body["attempt_limit"] = row["attempt_limit"]
        .as_i64()
        .unwrap_or(5)
        .clamp(1, 20)
        .into();
    // A transport retry reuses request/canonical identities, but its transient
    // SQL command needs no durable identity of its own.
    Ok(body)
}

pub(super) async fn import_page<T: QueueSql>(store: &T, now_ms: i64) -> Result<bool, StoreError> {
    if now_ms < 0 {
        return Err(invalid());
    }
    let rows = store
        .queue_sql(PAGE_SQL.into(), "{}".into(), false, true)
        .await?;
    if rows.is_empty() {
        return Ok(false);
    }
    let mut statements = Vec::new();
    let mut bytes = 1024usize;
    for row in rows {
        let entry: LegacyEntry = serde_json::from_str(&row).map_err(|_| invalid())?;
        let (sql, body) = match prepare(&entry, now_ms) {
            Ok(body) => (ENQUEUE_SQL, body),
            Err(_) => (
                REJECT_SQL,
                json!({"command_id": uuid::Uuid::new_v4().to_string(),
                "legacy_key": entry.legacy_key, "now_ms": now_ms, "code": "legacy_payload_invalid"}),
            ),
        };
        let body = serde_json::to_string(&body).map_err(|_| invalid())?;
        // txn executes mutations without result sets; the durable mapping is
        // the acknowledgement, and replay reads the same sealed identities.
        let sql = sql
            .trim_end()
            .strip_suffix("RETURNING result_json")
            .ok_or_else(invalid)?;
        // Include duplicated statement bytes in the replicated WAL bound.
        let added = sql.len() + body.len() + 256;
        if bytes + added > 512 * 1024 {
            break;
        }
        bytes += added;
        statements.push((sql.to_owned(), body));
    }
    if statements.is_empty() {
        return Err(invalid());
    }
    store.queue_transaction(statements).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::background_jobs::{BackgroundJobStore, CancelJob, JobQuery, WaiterQuery};
    use crate::store::sqlite::SqliteStore;

    fn legacy_database() -> (tempfile::TempDir, std::path::PathBuf, rusqlite::Connection) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("queue.sqlite");
        drop(SqliteStore::open(&path).expect("current schema"));
        let connection = rusqlite::Connection::open(&path).expect("legacy connection");
        connection.execute_batch("INSERT INTO libraries (id, name, kind, paths) VALUES (1, 'Migration', 'movies', '[]');
            INSERT INTO items (id, library_id, kind, title, sort_title) VALUES (1, 1, 'movie', 'Film', 'film');
            INSERT INTO files (id, item_id, path, size, mtime) VALUES (1, 1, '/migration.mkv', 100, 1);
            DELETE FROM background_job_migration;").expect("legacy catalogue");
        (directory, path, connection)
    }

    fn fragment(
        connection: &rusqlite::Connection,
        generation: usize,
        target: &str,
        attempts: i64,
        deadline: i64,
    ) {
        let pipeline = format!("{generation:064x}");
        let cache_key =
            crate::store::cluster_fragment_index_key(1, 100, 1, &"a".repeat(64), &pipeline)
                .expect("key");
        connection.execute("INSERT INTO cluster_fragment_index_jobs
            (cache_key, file_id, source_size, source_mtime, source_sha256, pipeline_sha256,
            priority, trigger, target_node_id, state, attempts, not_before_ms, created_at_ms, updated_at_ms,
            attempt_errors, index_retry_deadline_ms)
            VALUES (?1, 1, 100, 1, ?2, ?3, 'normal', 'background', ?4, 'queued', ?5, 0, 100, 100, ?6, ?7)",
            rusqlite::params![cache_key, "a".repeat(64), pipeline, target, attempts,
                vec!["index_budget_exceeded"; attempts as usize].join(","), deadline]).expect("legacy target");
    }

    fn seal(connection: &rusqlite::Connection) {
        let transaction = connection
            .unchecked_transaction()
            .expect("schema transaction");
        transaction
            .execute_batch(crate::store::background_jobs::SCHEMA)
            .expect("seal legacy source");
        transaction.commit().expect("seal commit");
    }

    #[tokio::test]
    async fn migration_converges_targets_and_preserves_individual_ledgers_across_restart() {
        let (_directory, path, connection) = legacy_database();
        fragment(&connection, 1, "node-a", 1, 600_000);
        fragment(&connection, 1, "node-b", 3, 900_000);
        seal(&connection);
        drop(connection);
        let store = SqliteStore::open(&path).expect("new binary");
        assert_eq!(
            store
                .job_migration_status()
                .await
                .expect("status")
                .awaiting_import,
            2
        );
        assert!(store.import_legacy_jobs(1_000).await.expect("page"));
        let jobs = store
            .list_jobs(JobQuery {
                state: None,
                kind: None,
                after_id: None,
                limit: 100,
            })
            .await
            .expect("jobs");
        assert_eq!(jobs.jobs.len(), 1);
        let job = &jobs.jobs[0];
        assert_eq!(
            job.failed_attempts, 0,
            "old targets are not summed into execution totals"
        );
        assert_eq!(job.retry_deadline_ms, 900_000);
        let interests = store
            .job_waiters(WaiterQuery {
                job_id: job.id.clone(),
                after: None,
                limit: 100,
            })
            .await
            .expect("interests");
        assert_eq!(interests.waiters.len(), 2);
        for waiter in interests.waiters {
            let expected = if waiter.target_node_id.as_deref() == Some("node-a") {
                (1, 600_000)
            } else {
                (3, 900_000)
            };
            assert_eq!((waiter.failed_attempts, waiter.retry_deadline_ms), expected);
        }
        drop(store);
        let restarted = SqliteStore::open(&path).expect("restart");
        assert!(!restarted.import_legacy_jobs(2_000).await.expect("replay"));
        let status = restarted
            .job_migration_status()
            .await
            .expect("conservation");
        assert_eq!(
            (status.accepted, status.materialized, status.awaiting_import),
            (2, 2, 0)
        );
    }

    #[tokio::test]
    async fn migration_overflow_retains_all_accepted_work_until_capacity_frees() {
        let (_directory, path, connection) = legacy_database();
        let transaction = connection
            .unchecked_transaction()
            .expect("legacy seed transaction");
        for index in 0..2_050 {
            fragment(&transaction, index, "node-a", 0, 0);
            transaction.execute("INSERT INTO pretranscode_jobs
                (id, dedupe_key, file_id, source_size, source_mtime, target_height, policy_generation,
                requirements_json, reason, priority, state, not_before_ms, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, 1, 100, 1, 720, 'policy:1', ?3, 'recent', 0, 'queued', 0, 100, 100)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), format!("transcode:{index}"),
                    json!({"version":1,"decoder":"h264","acceptable_encoder_families":["software"],
                        "output_contract":"hls-v1","tone_map":false,"output_grade":"sdr","scratch_bytes":1024}).to_string()]
            ).expect("legacy transcode");
        }
        transaction.commit().expect("legacy commit");
        seal(&connection);
        drop(connection);
        let store = SqliteStore::open(&path).expect("new binary");
        for _ in 0..256 {
            store.import_legacy_jobs(1_000).await.expect("bounded page");
            if store
                .job_migration_status()
                .await
                .expect("progress")
                .awaiting_import
                <= 260
            {
                break;
            }
        }
        let full = store.job_migration_status().await.expect("full status");
        assert_eq!(full.accepted, 4_100);
        assert_eq!(
            full.materialized, 3_840,
            "foreground headroom remains available"
        );
        assert_eq!(full.awaiting_import, 260);
        let mut cursor = None;
        loop {
            let page = store
                .list_jobs(JobQuery {
                    state: None,
                    kind: None,
                    after_id: cursor,
                    limit: 128,
                })
                .await
                .expect("page");
            for job in page.jobs {
                store
                    .cancel_job(CancelJob {
                        job_id: job.id,
                        now_ms: 2_000,
                    })
                    .await
                    .expect("free capacity");
            }
            cursor = page.next_after_id;
            if cursor.is_none() {
                break;
            }
        }
        for _ in 0..16 {
            store.import_legacy_jobs(3_000).await.expect("drain");
        }
        let done = store.job_migration_status().await.expect("done");
        assert_eq!(
            (
                done.accepted,
                done.materialized,
                done.awaiting_import,
                done.failed
            ),
            (4_100, 4_100, 0, 0)
        );
    }
}
