//! Pipeline contract v2 sidecar for targeted scan requests
//! (`PIPELINE-CONTRACT.md` §5.1).
//!
//! A targeted request's durable intent stays in `background_library_requests`
//! exactly as older binaries wrote and read it. What contract v2 adds — the
//! creating API key, Curator's idempotency key, the files it expects, what the
//! request is waiting on and the per-file report — lives here, keyed by
//! request id. `LibraryWorkInput` is `deny_unknown_fields`; a new field in a
//! stored row would make every older node reject the row during a rolling
//! upgrade, so this table is the extension point instead.
//!
//! One implementation serves both catalogues through the queue's SQL bridge,
//! and the schema is installed the same way on each: one SQLite migration
//! step, and an idempotent, shape-verified install on hiqlite.
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::background_jobs::{decode, encode, QueueSql};
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("scan_request_v2.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 125;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'background_scan_request_v2*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM background_scan_request_v2_schema WHERE singleton=1 AND version=1";
/// The objects the extension's triggers reference.
const DEPENDENCIES: &str =
    "CREATE TABLE background_library_requests(request_id TEXT PRIMARY KEY); \
     CREATE TABLE background_job_commands(id TEXT, operation TEXT, request_json TEXT);";

/// The caller's idempotency key, including Curator's attempt suffix.
pub const MAX_REQUEST_KEY_BYTES: usize = 256;
/// Bounded with the request's own 16 KiB intent: a directory's media files.
pub const MAX_EXPECTED_FILES: usize = 64;
pub const MAX_WAITING_ON_BYTES: usize = 1024;
pub const MAX_FILES_JSON_BYTES: usize = 64 * 1024;

/// A media file the caller placed under the request path and expects plurx
/// to index: its size and its modification time in nanoseconds, so a
/// same-path replacement with equal size and whole-second mtime is told apart
/// from the file it replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFile {
    pub path: String,
    pub bytes: i64,
    pub mtime_ns: i64,
}

/// What became of one expected file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlacedReport {
    /// Indexed from a fresh probe; `bytes` and `mtime_ns` are the file as
    /// plurx read it, not the caller's claim.
    Placed {
        path: String,
        item_id: i64,
        file_id: i64,
        bytes: i64,
        mtime_ns: i64,
    },
    /// On disk but not indexed, and why.
    Skipped { path: String, reason: String },
    /// Not on disk when the scan looked.
    Missing { path: String },
}

impl PlacedReport {
    pub fn path(&self) -> &str {
        match self {
            Self::Placed { path, .. } | Self::Skipped { path, .. } | Self::Missing { path } => path,
        }
    }
}

/// What a v2 request carries into its admission transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequestV2Seed {
    /// `ApiKey.id` at admission, retained for audit only: ids may be reused.
    pub key_id: i64,
    /// Stable credential identity: the existing SHA-256 API-key hash.
    pub key_hash: String,
    /// Empty when the caller sent none; only a non-empty key is unique.
    pub request_key: String,
    pub expected_files: Vec<ExpectedFile>,
}

impl ScanRequestV2Seed {
    pub fn validate(&self) -> Result<(), StoreError> {
        let invalid = |what: &str| Err(StoreError::Task(format!("invalid scan request: {what}")));
        if self.key_id <= 0 {
            return invalid("key id");
        }
        if self.key_hash.len() != 64
            || !self
                .key_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return invalid("key hash must be 64 lowercase hexadecimal bytes");
        }
        if self.request_key.len() > MAX_REQUEST_KEY_BYTES
            || self.request_key.chars().any(char::is_control)
        {
            return invalid("request_key must be at most 256 non-control bytes");
        }
        if self.expected_files.len() > MAX_EXPECTED_FILES {
            return invalid("at most 64 expected_files");
        }
        if self.expected_files.iter().any(|file| {
            file.path.is_empty()
                || file.path.len() > 4096
                || file.path.chars().any(char::is_control)
                || file.bytes < 0
                || file.mtime_ns < 0
        }) {
            return invalid("expected_files need a path, non-negative bytes and mtime_ns");
        }
        if encode(&self.expected_files)?.len() > 16 * 1024 {
            return invalid("expected_files exceed 16 KiB");
        }
        Ok(())
    }
}

/// The sidecar row of one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequestV2 {
    pub request_id: String,
    pub key_id: i64,
    pub key_hash: String,
    pub request_key: String,
    pub expected_files: Vec<ExpectedFile>,
    /// `full_scan:<library>` · `root_unreadable:<path>` · `queue_position:<n>`,
    /// or empty when nothing is in the way. The contract also names
    /// `heavy_lane:<holder>`; a targeted job takes no background source
    /// reader, so nothing writes it.
    pub waiting_on: String,
    pub files: Vec<PlacedReport>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn request_id_ok(request_id: &str) -> bool {
    !request_id.is_empty() && request_id.len() <= 256
}

fn waiting_on_ok(waiting_on: &str) -> bool {
    waiting_on.len() <= MAX_WAITING_ON_BYTES && !waiting_on.chars().any(char::is_control)
}

const ROW_JSON: &str =
    "json_object('request_id', request_id, 'key_id', key_id, 'key_hash', key_hash, \
     'request_key', request_key, 'expected_files', json(expected_files_json), \
     'waiting_on', waiting_on, 'files', json(files_json), \
     'created_at_ms', created_at_ms, 'updated_at_ms', updated_at_ms)";

pub(super) async fn get<T: QueueSql>(
    store: &T,
    request_id: &str,
) -> Result<Option<ScanRequestV2>, StoreError> {
    if !request_id_ok(request_id) {
        return Err(StoreError::Task("invalid scan request id".into()));
    }
    store
        .queue_sql(
            format!("SELECT {ROW_JSON} AS result_json FROM background_scan_request_v2 WHERE request_id = json_extract($1, '$.request_id')"),
            encode(&serde_json::json!({ "request_id": request_id }))?,
            false,
            true,
        )
        .await?
        .first()
        .map(|row| decode(row))
        .transpose()
}

pub(super) async fn by_key<T: QueueSql>(
    store: &T,
    key_hash: &str,
    request_key: &str,
) -> Result<Option<String>, StoreError> {
    if request_key.is_empty() || request_key.len() > MAX_REQUEST_KEY_BYTES {
        return Ok(None);
    }
    store
        .queue_sql(
            "SELECT json_quote(request_id) AS result_json FROM background_scan_request_v2 \
             WHERE key_hash = json_extract($1, '$.key_hash') AND request_key = json_extract($1, '$.request_key') \
               AND request_key != ''"
                .into(),
            encode(&serde_json::json!({ "key_hash": key_hash, "request_key": request_key }))?,
            false,
            true,
        )
        .await?
        .first()
        .map(|row| decode(row))
        .transpose()
}

pub(super) async fn set_waiting_on<T: QueueSql>(
    store: &T,
    request_id: &str,
    waiting_on: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !request_id_ok(request_id) || !waiting_on_ok(waiting_on) {
        return Err(StoreError::Task("invalid scan request waiting_on".into()));
    }
    Ok(!store
        .queue_sql(
            "UPDATE background_scan_request_v2 SET waiting_on = json_extract($1, '$.waiting_on'), \
                 updated_at_ms = json_extract($1, '$.now_ms') \
             WHERE request_id = json_extract($1, '$.request_id') \
               AND waiting_on != json_extract($1, '$.waiting_on') \
             RETURNING 'true' AS result_json"
                .into(),
            encode(&serde_json::json!({
                "request_id": request_id, "waiting_on": waiting_on, "now_ms": now_ms,
            }))?,
            true,
            true,
        )
        .await?
        .is_empty())
}

/// Every still-pending request of one library job waits on the same thing.
pub(super) async fn set_job_waiting_on<T: QueueSql>(
    store: &T,
    job_id: &str,
    waiting_on: &str,
    now_ms: i64,
) -> Result<u64, StoreError> {
    if uuid::Uuid::parse_str(job_id).is_err() || !waiting_on_ok(waiting_on) {
        return Err(StoreError::Task("invalid scan request waiting_on".into()));
    }
    Ok(store
        .queue_sql(
            "UPDATE background_scan_request_v2 SET waiting_on = json_extract($1, '$.waiting_on'), \
                 updated_at_ms = json_extract($1, '$.now_ms') \
             WHERE waiting_on != json_extract($1, '$.waiting_on') AND request_id IN ( \
                 SELECT request.request_id FROM background_library_requests request \
                 JOIN background_job_waiters waiter ON waiter.request_scope = 'library' \
                     AND waiter.request_id = request.request_id \
                 WHERE request.job_id = json_extract($1, '$.job_id') AND waiter.state = 'pending') \
             RETURNING 'true' AS result_json"
                .into(),
            encode(&serde_json::json!({
                "job_id": job_id, "waiting_on": waiting_on, "now_ms": now_ms,
            }))?,
            true,
            true,
        )
        .await?
        .len() as u64)
}

pub(super) async fn set_files<T: QueueSql>(
    store: &T,
    request_id: &str,
    files: &[PlacedReport],
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !request_id_ok(request_id) || encode(&files)?.len() > MAX_FILES_JSON_BYTES {
        return Err(StoreError::Task(
            "invalid or oversized scan file report".into(),
        ));
    }
    Ok(!store
        .queue_sql(
            "UPDATE background_scan_request_v2 SET files_json = json_extract($1, '$.files'), \
                 waiting_on = '', updated_at_ms = json_extract($1, '$.now_ms') \
             WHERE request_id = json_extract($1, '$.request_id') \
             RETURNING 'true' AS result_json"
                .into(),
            encode(&serde_json::json!({
                "request_id": request_id, "files": files, "now_ms": now_ms,
            }))?,
            true,
            true,
        )
        .await?
        .is_empty())
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("scan request schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("scan request schema dependencies");
    conn.execute_batch(SCHEMA).expect("scan request schema");
    rows(&conn).expect("scan request schema shape")
});

/// `None` when nothing of the extension exists, `Some(1)` when its exact shape
/// does; anything in between is refused rather than repaired.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| {
                (
                    name.clone(),
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "scan request v2 schema is partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "scan request v2 schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "scan request v2 marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LibraryKind, NewLibrary};
    use crate::store::background_jobs::{BackgroundJobStore, EnqueueOutcome};
    use crate::store::background_jobs_library::{
        LibraryWorkInput, LibraryWorkQuery, NewLibraryWork,
    };
    use crate::store::{LibraryStore, SqliteStore};

    fn fixture() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(DEPENDENCIES).expect("dependencies");
        conn
    }

    fn targeted(library_id: i64, path: &str) -> NewLibraryWork {
        NewLibraryWork {
            request_id: format!("sr-{}", uuid::Uuid::new_v4().simple()),
            library_id,
            input: LibraryWorkInput::Targeted {
                path: path.into(),
                ids: None,
                book: None,
                correlation_id: Some("t-1".into()),
                source: Some("monarr".into()),
            },
            now_ms: 1_000,
        }
    }

    fn seed(key_id: i64, request_key: &str) -> ScanRequestV2Seed {
        ScanRequestV2Seed {
            key_id,
            key_hash: format!("{key_id:064x}"),
            request_key: request_key.into(),
            expected_files: vec![ExpectedFile {
                path: "/tv/Show/Season 1/Show.S01E01.mkv".into(),
                bytes: 8_123_456_789,
                mtime_ns: 1_794_076_138_123_456_789,
            }],
        }
    }

    #[test]
    fn scan_request_schema_installs_whole_and_refuses_a_partial_shape() {
        let conn = fixture();
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch(SCHEMA).expect("install");
        conn.execute_batch(SCHEMA).expect("replay is idempotent");
        verify_sqlite(&conn).expect("complete");
        conn.execute_batch("DROP INDEX background_scan_request_v2_key")
            .expect("corrupt fixture");
        assert!(verify_sqlite(&conn).is_err());
    }

    #[test]
    fn scan_request_hiqlite_statements_are_the_sqlite_schema_split() {
        #[cfg(feature = "hiqlite-store")]
        {
            let conn = fixture();
            for statement in statements() {
                super::super::hiqlite::validate_sql(&statement).expect("replicated statement");
                conn.execute_batch(&statement).expect("statement");
            }
            verify_sqlite(&conn).expect("same shape as the SQLite step");
        }
    }

    /// The regression for the T1 store: the sidecar is written by the
    /// admission itself, reads back whole, updates in place, and a
    /// `(key, request_key)` pair admits exactly one request — a second
    /// admission with it changes nothing at all — while another key, or no
    /// key, is independent. An old-shape stored intent still decodes: the
    /// sidecar never touches `LibraryWorkInput`.
    #[tokio::test]
    async fn sidecar_round_trips_and_key_is_unique_per_api_key() {
        let store = SqliteStore::open_in_memory().expect("store");
        let library = store
            .create_library(&NewLibrary {
                name: "TV".into(),
                kind: LibraryKind::Shows,
                paths: vec!["/tv".into()],
                anime: false,
            })
            .await
            .expect("library");
        let first = targeted(library.id, "/tv/Show/Season 1");
        let outcome = store
            .enqueue_library_work_v2(first.clone(), seed(7, "t-1/0/abc/1"))
            .await
            .expect("first admission");
        assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
        let row = store
            .scan_request_v2(&first.request_id)
            .await
            .expect("read")
            .expect("written in the admission");
        assert_eq!(row.key_id, 7);
        assert_eq!(row.key_hash, seed(7, "").key_hash);
        assert_eq!(row.request_key, "t-1/0/abc/1");
        assert_eq!(row.expected_files, seed(7, "").expected_files);
        assert_eq!(row.waiting_on, "");
        assert!(row.files.is_empty());
        assert_eq!(
            store
                .scan_request_by_key(&seed(7, "").key_hash, "t-1/0/abc/1")
                .await
                .expect("lookup"),
            Some(first.request_id.clone())
        );
        assert_eq!(
            store
                .scan_request_by_key(&seed(8, "").key_hash, "t-1/0/abc/1")
                .await
                .expect("lookup"),
            None,
            "a request key belongs to the API key that created it"
        );

        assert!(store
            .set_scan_request_waiting_on(&first.request_id, "full_scan:TV", 2_000)
            .await
            .expect("waiting_on"));
        let files = vec![
            PlacedReport::Placed {
                path: "/tv/Show/Season 1/Show.S01E01.mkv".into(),
                item_id: 1201,
                file_id: 5531,
                bytes: 8_123_456_789,
                mtime_ns: 1_794_076_138_123_456_789,
            },
            PlacedReport::Skipped {
                path: "/tv/Show/Season 1/sample.mkv".into(),
                reason: "unsupported extension".into(),
            },
            PlacedReport::Missing {
                path: "/tv/Show/Season 1/Show.S01E02.mkv".into(),
            },
        ];
        assert!(store
            .set_scan_request_files(&first.request_id, &files, 3_000)
            .await
            .expect("files"));
        let row = store
            .scan_request_v2(&first.request_id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.files, files);
        assert_eq!(row.waiting_on, "", "a reported request waits on nothing");
        assert_eq!(row.updated_at_ms, 3_000);
        assert_eq!(
            serde_json::to_value(&files[0]).expect("wire")["state"],
            "placed"
        );

        // A concurrent identical request lost: the whole admission aborts,
        // so it leaves neither a request row nor a sidecar behind.
        let duplicate = targeted(library.id, "/tv/Show/Season 1");
        assert!(store
            .enqueue_library_work_v2(duplicate.clone(), seed(7, "t-1/0/abc/1"))
            .await
            .is_err());
        assert!(store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(duplicate.request_id.clone()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("list")
            .is_empty());
        assert_eq!(
            store
                .scan_request_v2(&duplicate.request_id)
                .await
                .expect("read"),
            None
        );

        // The next attempt, another API key and no key at all are each new.
        for (key_id, request_key) in [(7, "t-1/0/abc/2"), (8, "t-1/0/abc/1"), (7, ""), (7, "")] {
            let next = targeted(library.id, "/tv/Show/Season 1");
            store
                .enqueue_library_work_v2(next.clone(), seed(key_id, request_key))
                .await
                .expect("independent admission");
            assert_eq!(
                store
                    .scan_request_v2(&next.request_id)
                    .await
                    .expect("read")
                    .map(|row| (row.key_id, row.request_key)),
                Some((key_id, request_key.to_owned()))
            );
        }
        assert_eq!(
            store
                .scan_request_by_key(&seed(7, "").key_hash, "")
                .await
                .expect("lookup"),
            None
        );

        // A request admitted the old way has no sidecar, and its stored
        // intent is exactly the shape every older binary reads.
        let legacy = targeted(library.id, "/tv/Other");
        store
            .enqueue_library_work(legacy.clone())
            .await
            .expect("legacy admission");
        assert_eq!(
            store
                .scan_request_v2(&legacy.request_id)
                .await
                .expect("read"),
            None
        );
        let records = store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(legacy.request_id.clone()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("list");
        let stored = serde_json::to_value(&records[0].input).expect("stored intent");
        assert_eq!(
            stored,
            serde_json::json!({"kind": "targeted", "path": "/tv/Other", "ids": null, "book": null,
                "correlation_id": "t-1", "source": "monarr"})
        );
        let old_shape: LibraryWorkInput = serde_json::from_str(
            r#"{"kind":"targeted","path":"/tv/Other","ids":null,"book":null,"correlation_id":"t-1","source":"monarr"}"#,
        )
        .expect("an old-shape intent still decodes");
        assert!(matches!(old_shape, LibraryWorkInput::Targeted { .. }));
        assert!(
            serde_json::from_str::<LibraryWorkInput>(
                r#"{"kind":"targeted","path":"/x","ids":null,"book":null,"correlation_id":null,"source":null,"request_key":"k"}"#
            )
            .is_err(),
            "the reason the sidecar exists: stored intents refuse unknown fields"
        );

        // A seed that could not be stored is refused before admission.
        let mut oversized = seed(7, "k");
        oversized.request_key = "k".repeat(MAX_REQUEST_KEY_BYTES + 1);
        assert!(store
            .enqueue_library_work_v2(targeted(library.id, "/tv/Show"), oversized)
            .await
            .is_err());
    }
}
