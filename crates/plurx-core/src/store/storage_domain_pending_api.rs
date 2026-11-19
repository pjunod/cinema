//! One replicated decision saves desired identities independently of readers,
//! then applies them only when capacity and exact job ownership are preserved.
use super::background_jobs::{decode, encode, QueueSql};
use super::background_jobs_resources::{validate, StorageDomainMapping};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageDomainWait {
    pub holder_kind: String,
    pub work_id: String,
    pub fence: i64,
    pub node_id: Option<String>,
    pub resource_key: Option<String>,
    pub library_id: Option<i64>,
    pub reason: String,
    pub stuck: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageDomainPending {
    pub revision: i64,
    pub mappings: Vec<StorageDomainMapping>,
    pub saved_at_ms: i64,
    pub saved_by: Option<i64>,
    pub state: String,
    pub settled_at_ms: Option<i64>,
    pub settled_reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageDomainState {
    pub applied: Vec<StorageDomainMapping>,
    pub pending: Option<StorageDomainPending>,
    pub waiting_on: Vec<StorageDomainWait>,
    pub waiting_total: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageDomainApply {
    Applied { revision: i64 },
    Waiting { revision: i64 },
    Superseded { revision: i64 },
    NothingPending,
}

// Shared by advisory diagnostics and the atomic application predicate. A holder
// with bare source_io continues to conflict with every named domain. Running
// jobs additionally need exact required keys to remain unchanged for renew and
// claim acknowledgement, including SQL sent by older nodes.
const WAITING: &str = r#"
WITH clock AS (SELECT json_extract($1, '$.now_ms') AS ms),
live AS (SELECT h.* FROM background_storage_domain_holders h, clock
    WHERE h.expires_at_ms > clock.ms OR h.custody_held = 1),
readers AS (
  SELECT h.holder_kind, h.work_id, h.fence, h.node_id, h.resource_key,
    NULL AS library_id, 'old_domain_reader' AS reason,
    CASE WHEN h.expires_at_ms <= clock.ms THEN 1 ELSE 0 END AS stuck
  FROM live h JOIN background_storage_domain_disturbed d ON d.resource_key = h.resource_key, clock
  WHERE NOT EXISTS (SELECT 1 FROM live b WHERE b.holder_kind = h.holder_kind
    AND b.work_id = h.work_id AND b.fence = h.fence AND b.resource_key = 'source_io')),
jobs AS (
  SELECT 'job', j.job_id, j.fence, j.node_id, NULL, j.library_id, 'library_job', 0
  FROM background_storage_domain_affected_jobs j, clock WHERE j.lease_expires_ms > clock.ms)
SELECT * FROM readers UNION ALL SELECT * FROM jobs
"#;

const SAVE: &str = r#"
INSERT INTO background_storage_domain_pending(singleton, revision, mappings_json, base_json,
    saved_at_ms, saved_by, state, settled_at_ms, settled_reason)
SELECT 1, COALESCE((SELECT revision FROM background_storage_domain_pending WHERE singleton = 1), 0) + 1,
    json_extract($1, '$.mappings'),
    (SELECT json_group_array(json_object('library_id', library_id, 'root_path', root_path, 'domain_id', domain_id))
       FROM background_storage_domains_live),
    json_extract($1, '$.now_ms'), json_extract($1, '$.saved_by'), 'pending', NULL, NULL
WHERE NOT EXISTS (SELECT 1 FROM json_each($1, '$.mappings') mapping
    WHERE NOT EXISTS (SELECT 1 FROM libraries library,
        json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
      WHERE library.id = json_extract(mapping.value, '$.library_id')
        AND root.value = json_extract(mapping.value, '$.root_path')))
ON CONFLICT(singleton) DO UPDATE SET revision = excluded.revision, mappings_json = excluded.mappings_json,
    base_json = excluded.base_json, saved_at_ms = excluded.saved_at_ms, saved_by = excluded.saved_by,
    state = 'pending', settled_at_ms = NULL, settled_reason = NULL
RETURNING CAST(revision AS TEXT) AS result_json
"#;

// Filter the base through live roots too: deleting a root after save does not
// mean an older binary changed the applied configuration behind our back.
const BASE_DIFFERS: &str = r#"
EXISTS (
 SELECT library_id, root_path, domain_id FROM background_storage_domains_live
 EXCEPT SELECT json_extract(base.value, '$.library_id'), json_extract(base.value, '$.root_path'),
   json_extract(base.value, '$.domain_id') FROM json_each(pending.base_json) base
) OR EXISTS (
 SELECT json_extract(base.value, '$.library_id'), json_extract(base.value, '$.root_path'),
   json_extract(base.value, '$.domain_id') FROM json_each(pending.base_json) base
 WHERE EXISTS (SELECT 1 FROM libraries library,
   json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
   WHERE library.id = json_extract(base.value, '$.library_id')
     AND root.value = json_extract(base.value, '$.root_path'))
 EXCEPT SELECT library_id, root_path, domain_id FROM background_storage_domains_live
)
"#;

fn clock(now_ms: i64) -> Result<(), StoreError> {
    if now_ms < 0 {
        return Err(StoreError::Task("invalid storage domain clock".into()));
    }
    Ok(())
}
pub(super) async fn save<T: QueueSql>(
    store: &T,
    mappings: Vec<StorageDomainMapping>,
    saved_by: Option<i64>,
    now_ms: i64,
) -> Result<Option<i64>, StoreError> {
    validate(&mappings)?;
    clock(now_ms)?;
    let body =
        encode(&serde_json::json!({"mappings":mappings,"saved_by":saved_by,"now_ms":now_ms}))?;
    if body.len() > 64 * 1024 {
        return Err(StoreError::Task(
            "storage domain configuration exceeds 64 KiB".into(),
        ));
    }
    store
        .queue_sql(SAVE.into(), body, true, true)
        .await?
        .first()
        .map(|row| decode(row))
        .transpose()
}

pub(super) async fn apply<T: QueueSql>(
    store: &T,
    now_ms: i64,
) -> Result<StorageDomainApply, StoreError> {
    clock(now_ms)?;
    // This advisory read can postpone an application until the next upkeep,
    // but cannot authorize one. Avoid an idle Raft mutation on every node's
    // tick; the replicated command below still checks the latest revision and
    // every blocker atomically when a pending revision is observed.
    let pending = store.queue_sql(
        "SELECT 'true' AS result_json FROM background_storage_domain_pending WHERE singleton=json_extract($1,'$.singleton') AND state='pending'".into(),
        "{\"singleton\":1}".into(), false, false,
    ).await?;
    if pending.is_empty() {
        return Ok(StorageDomainApply::NothingPending);
    }
    let sql = format!(
        r#"
INSERT INTO background_job_commands(id, operation, request_json, result_json)
SELECT json_extract($1, '$.id'), 'storage_domains_apply', $1,
 json_object('revision', pending.revision, 'verdict', CASE
   WHEN pending.state != 'pending' THEN 'nothing'
   WHEN ({BASE_DIFFERS}) THEN 'superseded'
   WHEN EXISTS ({WAITING}) THEN 'waiting' ELSE 'applied' END,
   'reason', CASE WHEN pending.state = 'pending' AND ({BASE_DIFFERS}) THEN 'applied_changed_elsewhere' END)
FROM background_storage_domain_pending pending WHERE pending.singleton = 1
RETURNING result_json
"#
    );
    let body = encode(&serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"now_ms":now_ms}))?;
    let rows = store.queue_sql(sql, body, true, true).await?;
    #[derive(Deserialize)]
    struct Verdict {
        revision: i64,
        verdict: String,
    }
    let Some(row) = rows.first() else {
        return Ok(StorageDomainApply::NothingPending);
    };
    let result: Verdict = decode(row)?;
    match result.verdict.as_str() {
        "applied" => Ok(StorageDomainApply::Applied {
            revision: result.revision,
        }),
        "waiting" => Ok(StorageDomainApply::Waiting {
            revision: result.revision,
        }),
        "superseded" => Ok(StorageDomainApply::Superseded {
            revision: result.revision,
        }),
        "nothing" => Ok(StorageDomainApply::NothingPending),
        _ => Err(StoreError::Database(
            "invalid storage domain application verdict".into(),
        )),
    }
}

pub(super) async fn state<T: QueueSql>(
    store: &T,
    now_ms: i64,
) -> Result<StorageDomainState, StoreError> {
    clock(now_ms)?;
    // One authoritative snapshot keeps applied/pending/blocker diagnostics
    // consistent during a concurrent save or application on another node.
    let sql = format!(
        r#"
WITH waiting AS ({WAITING})
SELECT json_object(
 'applied', json((SELECT json_group_array(json_object('library_id', library_id, 'root_path', root_path,
   'domain_id', domain_id)) FROM (SELECT * FROM background_storage_domains ORDER BY library_id, root_path))),
 'pending', json((SELECT json_object('revision', revision, 'mappings', json(mappings_json),
   'saved_at_ms', saved_at_ms, 'saved_by', saved_by, 'state', state,
   'settled_at_ms', settled_at_ms, 'settled_reason', settled_reason)
   FROM background_storage_domain_pending WHERE singleton = 1)),
 'waiting_on', json((SELECT json_group_array(json_object('holder_kind',holder_kind,'work_id',work_id,
   'fence',fence,'node_id',node_id,'resource_key',resource_key,'library_id',library_id,'reason',reason,
   'stuck',json(CASE WHEN stuck = 1 THEN 'true' ELSE 'false' END))) FROM
   (SELECT * FROM waiting ORDER BY reason, resource_key, holder_kind, work_id LIMIT 32))),
 'waiting_total', (SELECT count(DISTINCT json_array(work_id,fence)) FROM waiting)) AS result_json
"#
    );
    let body = encode(&serde_json::json!({"now_ms":now_ms}))?;
    let rows = store.queue_sql(sql, body, false, true).await?;
    decode(
        rows.first()
            .ok_or_else(|| StoreError::Database("missing storage domain state".into()))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LibraryKind, NewLibrary};
    use crate::store::background_jobs::BackgroundJobStore;
    use crate::store::sqlite::SqliteStore;
    use crate::store::LibraryStore;

    async fn library(store: &SqliteStore, paths: Vec<std::path::PathBuf>) -> i64 {
        store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths,
                anime: false,
            })
            .await
            .expect("library")
            .id
    }

    fn mapping(library_id: i64, root_path: &str, domain_id: &str) -> StorageDomainMapping {
        StorageDomainMapping {
            library_id,
            root_path: root_path.into(),
            domain_id: domain_id.into(),
        }
    }

    #[tokio::test]
    async fn removing_a_library_root_after_save_applies_without_retaining_obsolete_domains() {
        let store = SqliteStore::open_in_memory().expect("store");
        let library_id = library(&store, vec!["/media".into()]).await;
        let first = store
            .save_storage_domains(vec![mapping(library_id, "/media", "nas-a")], None, 1)
            .await
            .expect("initial save")
            .expect("valid root");
        assert_eq!(
            store
                .apply_pending_storage_domains(2)
                .await
                .expect("initial apply"),
            StorageDomainApply::Applied { revision: first }
        );
        let revision = store
            .save_storage_domains(vec![mapping(library_id, "/media", "nas-b")], None, 3)
            .await
            .expect("desired save")
            .expect("valid root");
        assert!(revision > first);
        QueueSql::queue_sql(
            &store,
            "UPDATE libraries SET paths = '[]' WHERE id = json_extract($1, '$.library_id')".into(),
            serde_json::json!({"library_id": library_id}).to_string(),
            true,
            true,
        )
        .await
        .expect("remove library root");

        assert_eq!(
            store
                .apply_pending_storage_domains(4)
                .await
                .expect("apply after root removal"),
            StorageDomainApply::Applied { revision }
        );
        // The raw applied listing includes stale mappings too, so emptiness
        // proves the obsolete nas-a row was removed rather than only hidden.
        assert!(store
            .storage_domains()
            .await
            .expect("applied rows")
            .is_empty());
        let state = store.storage_domain_state(4).await.expect("state");
        assert!(state.applied.is_empty());
        let pending = state.pending.expect("settled revision");
        assert_eq!(pending.revision, revision);
        assert_eq!(pending.state, "applied");
        assert_eq!(pending.settled_at_ms, Some(4));
        assert_eq!(pending.settled_reason, None);
    }

    #[tokio::test]
    async fn oversized_invalid_clock_and_invalid_root_saves_preserve_the_pending_revision() {
        let store = SqliteStore::open_in_memory().expect("store");
        let paths: Vec<std::path::PathBuf> = std::iter::once("/media".into())
            .chain((0..20).map(|index| format!("/media/{index}/{}", "x".repeat(3500)).into()))
            .collect();
        let library_id = library(&store, paths.clone()).await;
        let revision = store
            .save_storage_domains(vec![mapping(library_id, "/media", "nas-a")], Some(7), 10)
            .await
            .expect("save")
            .expect("valid root");
        let before = store.storage_domain_state(10).await.expect("before");
        assert_eq!(before.pending.as_ref().expect("pending").revision, revision);

        let oversized: Vec<_> = paths
            .iter()
            .map(|path| mapping(library_id, &path.to_string_lossy(), "nas-b"))
            .collect();
        let body =
            encode(&serde_json::json!({"mappings": &oversized,"saved_by": null,"now_ms": 11}))
                .expect("large payload");
        assert!(body.len() > 64 * 1024);
        let error = store
            .save_storage_domains(oversized, None, 11)
            .await
            .expect_err("body bound");
        assert!(error.to_string().contains("64 KiB"));
        assert_eq!(
            store
                .storage_domain_state(11)
                .await
                .expect("after large save"),
            before
        );

        assert!(store
            .save_storage_domains(vec![mapping(library_id, "/media", "nas-b")], None, -1)
            .await
            .is_err());
        assert_eq!(
            store
                .storage_domain_state(12)
                .await
                .expect("after invalid clock"),
            before
        );

        assert_eq!(
            store
                .save_storage_domains(vec![mapping(library_id, "/removed", "nas-b")], None, 13)
                .await
                .expect("invalid root refusal"),
            None
        );
        assert_eq!(
            store
                .storage_domain_state(13)
                .await
                .expect("after invalid root"),
            before
        );
    }
}
