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
//!
//! **Removed paths** (contract amendment `removed_paths`) travel in the same
//! `expected_files_json` column as `{"removed_path": "…"}` entries rather
//! than in a new column: the extension's shape is verified exactly by every
//! binary that knows it, so a new column would make an older binary refuse
//! to open the catalogue at all — a rollback blocker. An older node that
//! claims such a request fails to decode its sidecar (`ExpectedFile` refuses
//! unknown fields) and leaves the job to a node that understands it, instead
//! of walking the request's common parent as a plain targeted scan.
//!
//! **Offloaded paths** (contract amendment `offload`) are removed paths whose
//! titles cinema keeps: stored as `{"offloaded_path": "…"}` entries, a form an
//! older binary refuses exactly as it refuses `removed_path`, so an older node
//! never turns an offload into a delete of the title and its watch history.
//!
//! **What an older node does with such a row** (verified in
//! `process_library_requests`): it reads the sidecar with
//! `scan_request_v2(id)?`, the decode fails, and that error ends the whole
//! library turn. The job settles as a charged `Retry` (`library_execution_failed`,
//! 5 s / 30 s / 2 min / 10 min backoff) and the request stays pending, so a
//! node that understands the row can still claim and finish it. It is not a
//! terminal failure of the request until the job's attempt limit is spent, and
//! it does not hold the library fence between attempts. Because every pending
//! targeted request of a library shares one job, a row an older node cannot read
//! would also delay its neighbours; so probe-only requests (removed paths, or an
//! explicit `probe_only`) are admitted as their own job,
//! `library:targeted-probe:{library}`, and share a job only with each other.
//! An older node's status `GET` of such a row fails to decode and answers `500`;
//! that, too, ends when every cinema node is upgraded. Deploy order: all cinema
//! nodes first, Curator second (INTEGRATION §1.10).
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
/// As many removed paths as expected files; more go in further requests.
pub const MAX_REMOVED_PATHS: usize = 64;
/// The stored `expected_files_json` bound (the column's own CHECK is 16 KiB).
const MAX_EXPECTED_JSON_BYTES: usize = 16 * 1024;
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
    /// A removed path: absent on disk, plurx had it, and its file record is
    /// gone. `item_id` is the item of the first file removed; `item_ids` every
    /// distinct item whose files were removed (a folder can span several).
    /// `item_removed` says whether that left every one of them with no files,
    /// so they went too.
    Gone {
        path: String,
        item_id: i64,
        #[serde(default)]
        item_ids: Vec<i64>,
        item_removed: bool,
    },
    /// An offloaded path: absent on disk, its file records are gone, and the
    /// item(s) stay. `item_id` is the item of the first file removed;
    /// `item_ids` every distinct item. `offloaded_items` counts those that
    /// were left with no files and now carry `offloaded_at` (an item with
    /// another copy elsewhere keeps its remaining file and stays on disk).
    Offloaded {
        path: String,
        item_id: i64,
        #[serde(default)]
        item_ids: Vec<i64>,
        #[serde(default)]
        offloaded_items: u64,
    },
    /// A removed path that is absent and that plurx never had.
    Unknown { path: String },
    /// A removed path that still exists; nothing was removed. `reason` says
    /// which kind of "still there" it is.
    Present {
        path: String,
        #[serde(default)]
        reason: PresentReason,
    },
    /// A removed path that is absent, but whose record plurx kept. `code` is
    /// the machine reason, `retryable` whether sending the same removal again
    /// later can succeed without anyone changing anything, and `reason` the
    /// words for an operator.
    Retained {
        path: String,
        item_id: i64,
        code: RetainedCode,
        retryable: bool,
        reason: String,
    },
}

/// Why a removed path counts as present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentReason {
    /// The path exists.
    #[default]
    Exists,
    /// A symlink stands in the chain; plurx never follows it to decide
    /// absence, so it removes nothing.
    SymlinkInChain,
    /// The path exists only because the filesystem folds case: the folder
    /// lists the name with different capitals (a case-only rename). Nothing is
    /// removed; the entry that is there is the one that was moved.
    CaseVariant,
    /// It was absent at the first look and present again just before removal.
    ReappearedBeforeRemoval,
}

/// Why plurx kept the record of an absent removed path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainedCode {
    /// No root identity is recorded for the library yet; any full scan
    /// establishes it. Retryable.
    RootIdentityUnset,
    /// This node's library roots are not the recorded ones. An operator
    /// matter; not retryable.
    RootIdentityMismatch,
    /// This node could not compute its root identity. Retryable.
    RootIdentityUnavailable,
    /// More files than the deletion bound allow in one request. Send fewer;
    /// not retryable as sent.
    PruneBound,
    /// The record is still there after the removal ran, which the removal's
    /// one known exclusion — a Dolby Vision conversion holding the file —
    /// explains but plurx did not verify. Retryable once the conversion ends.
    RecordKept,
    /// An expected file of the same request was not placed, so nothing was
    /// removed: the old copy stays until the new one is indexed. Retryable
    /// (send a new attempt).
    ExpectedNotPlaced,
    /// The chain could not be re-read just before removal. Retryable.
    ChainUnreadable,
    /// An offload emptied the library root on its parent's own filesystem,
    /// which this node cannot tell from an unmounted share, so it removed
    /// nothing. Lasting rather than `path_unreadable`: sending it again
    /// changes nothing until a full scan or a new placement runs.
    RootEmpty,
}

impl RetainedCode {
    pub const fn retryable(self) -> bool {
        !matches!(
            self,
            Self::RootIdentityMismatch | Self::PruneBound | Self::RootEmpty
        )
    }
}

impl PlacedReport {
    pub fn path(&self) -> &str {
        match self {
            Self::Placed { path, .. }
            | Self::Skipped { path, .. }
            | Self::Missing { path }
            | Self::Gone { path, .. }
            | Self::Offloaded { path, .. }
            | Self::Unknown { path }
            | Self::Present { path, .. }
            | Self::Retained { path, .. } => path,
        }
    }
}

/// One stored entry of `expected_files_json`: an expected file, or a removed
/// path (see the module documentation for why they share the column).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredPath {
    Expected(ExpectedFile),
    Removed(RemovedPath),
    Offloaded(OffloadedPath),
    ProbeOnly(ProbeOnlyMarker),
}

/// A path Curator offloaded: its files go, its item stays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OffloadedPath {
    offloaded_path: String,
}

/// The explicit `probe_only` flag, stored as an entry an older binary's
/// `ExpectedFile` refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeOnlyMarker {
    probe_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovedPath {
    removed_path: String,
}

fn stored_paths(seed: &ScanRequestV2Seed) -> Vec<StoredPath> {
    seed.expected_files
        .iter()
        .cloned()
        .map(StoredPath::Expected)
        .chain(seed.removed_paths.iter().map(|path| {
            StoredPath::Removed(RemovedPath {
                removed_path: path.clone(),
            })
        }))
        .chain(seed.offloaded_paths.iter().map(|path| {
            StoredPath::Offloaded(OffloadedPath {
                offloaded_path: path.clone(),
            })
        }))
        .chain(
            seed.probe_only
                .then_some(StoredPath::ProbeOnly(ProbeOnlyMarker { probe_only: true })),
        )
        .collect()
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
    /// Files or emptied folders under the request path that no longer
    /// exist. A request that names any is probe-only: plurx looks at exactly
    /// its expected and removed paths and walks nothing else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_paths: Vec<String>,
    /// Like `removed_paths`, but the title is kept: the files' records go, the
    /// item stays with `offloaded_at` set once it has no files left.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offloaded_paths: Vec<String>,
    /// Explicit probe-only: record exactly the expected files and walk
    /// nothing, never sharing a pass with a request that did not ask for it.
    /// A request with removed paths is probe-only whatever this says.
    #[serde(default)]
    pub probe_only: bool,
}

impl ScanRequestV2Seed {
    /// Whether the request is probe-only, asked for or implied.
    pub fn is_probe_only(&self) -> bool {
        self.probe_only || !self.removed_paths.is_empty() || !self.offloaded_paths.is_empty()
    }

    /// The `scan_request_v2` object of the enqueue command. Its
    /// `expected_files` is what the admission trigger stores verbatim, so
    /// removed paths are folded into it here.
    pub(super) fn admission_value(&self) -> Result<serde_json::Value, StoreError> {
        let self_with_flag = Self {
            probe_only: self.is_probe_only(),
            ..self.clone()
        };
        Ok(serde_json::json!({
            "key_id": self.key_id,
            "key_hash": self.key_hash,
            "request_key": self.request_key,
            "expected_files": stored_paths(&self_with_flag),
        }))
    }

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
        if self.removed_paths.len() > MAX_REMOVED_PATHS {
            return invalid("at most 64 removed_paths");
        }
        if self
            .removed_paths
            .iter()
            .any(|path| path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control))
        {
            return invalid("removed_paths need a path of at most 4096 non-control bytes");
        }
        if self.offloaded_paths.len() > MAX_REMOVED_PATHS {
            return invalid("at most 64 offloaded_paths");
        }
        if self
            .offloaded_paths
            .iter()
            .any(|path| path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control))
        {
            return invalid("offloaded_paths need a path of at most 4096 non-control bytes");
        }
        if self
            .offloaded_paths
            .iter()
            .any(|path| self.removed_paths.contains(path))
        {
            return invalid("a path is either removed or offloaded, not both");
        }
        if encode(&stored_paths(self))?.len() > MAX_EXPECTED_JSON_BYTES {
            return invalid("expected_files and removed_paths exceed 16 KiB together");
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
    /// See [`ScanRequestV2Seed::removed_paths`].
    #[serde(default)]
    pub removed_paths: Vec<String>,
    /// See [`ScanRequestV2Seed::offloaded_paths`].
    #[serde(default)]
    pub offloaded_paths: Vec<String>,
    /// See [`ScanRequestV2Seed::probe_only`]; true too when removed or
    /// offloaded paths are named.
    #[serde(default)]
    pub probe_only: bool,
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
        .map(|row| decode::<StoredRow>(row).map(ScanRequestV2::from))
        .transpose()
}

/// A sidecar row as stored: `expected_files` holds removed paths too.
#[derive(Deserialize)]
struct StoredRow {
    request_id: String,
    key_id: i64,
    key_hash: String,
    request_key: String,
    expected_files: Vec<StoredPath>,
    waiting_on: String,
    files: Vec<PlacedReport>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<StoredRow> for ScanRequestV2 {
    fn from(row: StoredRow) -> Self {
        let (mut expected_files, mut removed_paths) = (Vec::new(), Vec::new());
        let mut offloaded_paths = Vec::new();
        let mut probe_only = false;
        for entry in row.expected_files {
            match entry {
                StoredPath::Expected(file) => expected_files.push(file),
                StoredPath::Removed(removed) => removed_paths.push(removed.removed_path),
                StoredPath::Offloaded(off) => offloaded_paths.push(off.offloaded_path),
                StoredPath::ProbeOnly(marker) => probe_only |= marker.probe_only,
            }
        }
        probe_only |= !removed_paths.is_empty() || !offloaded_paths.is_empty();
        Self {
            request_id: row.request_id,
            key_id: row.key_id,
            key_hash: row.key_hash,
            request_key: row.request_key,
            expected_files,
            removed_paths,
            offloaded_paths,
            probe_only,
            waiting_on: row.waiting_on,
            files: row.files,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        }
    }
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
            removed_paths: Vec::new(),
            offloaded_paths: Vec::new(),
            probe_only: false,
        }
    }

    /// Offloaded paths are stored as `{"offloaded_path"}` entries, a form an
    /// older binary refuses exactly as it refuses `removed_path`, so an older
    /// node can never turn an offload into a delete of the title.
    #[test]
    fn offloaded_paths_are_stored_in_a_form_older_decoders_refuse() {
        let mut offload = seed(3, "op/2/abc/1");
        offload.offloaded_paths = vec!["/m/Heat/Heat.mkv".into()];
        offload.validate().expect("valid");
        assert!(offload.is_probe_only(), "an offload is probe-only");
        let value = offload.admission_value().expect("admission");
        let stored = value["expected_files"].as_array().expect("array");
        assert_eq!(
            stored.len(),
            3,
            "expected, offloaded path, probe-only marker"
        );
        assert_eq!(
            stored[1],
            serde_json::json!({"offloaded_path": "/m/Heat/Heat.mkv"})
        );
        assert!(
            serde_json::from_value::<Vec<ExpectedFile>>(value["expected_files"].clone()).is_err(),
            "an older binary cannot mistake an offloaded path for an expected file"
        );
        let decoded: Vec<StoredPath> =
            serde_json::from_value(value["expected_files"].clone()).expect("decode");
        assert!(
            matches!(&decoded[1], StoredPath::Offloaded(o) if o.offloaded_path == "/m/Heat/Heat.mkv")
        );
        // A path is removed or offloaded, never both, and the paths are bounded.
        let mut both = offload.clone();
        both.removed_paths = both.offloaded_paths.clone();
        assert!(both.validate().is_err());
        let mut many = seed(3, "k");
        many.offloaded_paths = (0..=MAX_REMOVED_PATHS)
            .map(|i| format!("/m/{i}.mkv"))
            .collect();
        assert!(many.validate().is_err());
        assert_eq!(
            serde_json::to_value(PlacedReport::Offloaded {
                path: "/m/Heat/Heat.mkv".into(),
                item_id: 4,
                item_ids: vec![4],
                offloaded_items: 1
            })
            .expect("wire"),
            serde_json::json!({"state": "offloaded", "path": "/m/Heat/Heat.mkv", "item_id": 4,
                "item_ids": [4], "offloaded_items": 1})
        );
    }

    /// Removed paths share `expected_files_json` with the expected files.
    /// The stored entry is one an older binary's `ExpectedFile` refuses, so
    /// an older node fails the row rather than running the request as a
    /// plain walk of its common parent; the report states keep their names.
    #[test]
    fn removed_paths_are_stored_beside_expected_files_and_refused_by_older_decoders() {
        let mut moved = seed(3, "op/1/abc/1");
        moved.removed_paths = vec!["/tv/Old/Show.S01E01.mkv".into()];
        moved.validate().expect("valid");
        let value = moved.admission_value().expect("admission");
        let stored = value["expected_files"].as_array().expect("array");
        assert_eq!(stored.len(), 3, "expected, removed path, probe-only marker");
        assert_eq!(
            stored[1],
            serde_json::json!({"removed_path": "/tv/Old/Show.S01E01.mkv"})
        );
        assert_eq!(stored[2], serde_json::json!({"probe_only": true}));
        assert!(value.get("removed_paths").is_none());
        assert!(
            serde_json::from_value::<Vec<ExpectedFile>>(value["expected_files"].clone()).is_err(),
            "an older binary cannot mistake a removed path for an expected file"
        );
        let decoded: Vec<StoredPath> =
            serde_json::from_value(value["expected_files"].clone()).expect("decode");
        assert!(
            matches!(&decoded[0], StoredPath::Expected(file) if file == &moved.expected_files[0])
        );
        assert!(
            matches!(&decoded[1], StoredPath::Removed(r) if r.removed_path == "/tv/Old/Show.S01E01.mkv")
        );
        assert!(matches!(&decoded[2], StoredPath::ProbeOnly(_)));

        let wire = |report: PlacedReport| serde_json::to_value(report).expect("wire");
        assert_eq!(
            wire(PlacedReport::Gone {
                path: "/a".into(),
                item_id: 4,
                item_ids: vec![4, 5],
                item_removed: true
            }),
            serde_json::json!({"state": "gone", "path": "/a", "item_id": 4, "item_ids": [4, 5],
                "item_removed": true})
        );
        assert_eq!(
            wire(PlacedReport::Unknown { path: "/a".into() }),
            serde_json::json!({"state": "unknown", "path": "/a"})
        );
        assert_eq!(
            wire(PlacedReport::Present {
                path: "/a".into(),
                reason: PresentReason::SymlinkInChain
            }),
            serde_json::json!({"state": "present", "path": "/a", "reason": "symlink_in_chain"})
        );
        // A report written before `reason` existed still decodes.
        assert_eq!(
            serde_json::from_value::<PlacedReport>(
                serde_json::json!({"state": "present", "path": "/a"})
            )
            .expect("old present"),
            PlacedReport::Present {
                path: "/a".into(),
                reason: PresentReason::Exists
            }
        );
        let retained = PlacedReport::Retained {
            path: "/a".into(),
            item_id: 9,
            code: RetainedCode::RootIdentityUnset,
            retryable: RetainedCode::RootIdentityUnset.retryable(),
            reason: "no root identity".into(),
        };
        assert_eq!(
            wire(retained.clone()),
            serde_json::json!({"state": "retained", "path": "/a", "item_id": 9,
                "code": "root_identity_unset", "retryable": true, "reason": "no root identity"})
        );
        assert_eq!(
            serde_json::from_value::<PlacedReport>(wire(retained.clone())).expect("round trip"),
            retained
        );
        assert!(!RetainedCode::RootIdentityMismatch.retryable());
        assert!(!RetainedCode::PruneBound.retryable());
        for code in [
            RetainedCode::RootIdentityUnavailable,
            RetainedCode::RecordKept,
            RetainedCode::ExpectedNotPlaced,
            RetainedCode::ChainUnreadable,
        ] {
            assert!(code.retryable(), "{code:?}");
        }

        // The explicit flag is stored as a marker an older decoder refuses,
        // and reads back; removed paths imply it.
        let mut flagged = seed(3, "op/2/abc/1");
        flagged.probe_only = true;
        let value = flagged.admission_value().expect("admission");
        assert_eq!(
            value["expected_files"][1],
            serde_json::json!({"probe_only": true})
        );
        assert!(
            serde_json::from_value::<Vec<ExpectedFile>>(value["expected_files"].clone()).is_err()
        );
        assert!(moved.is_probe_only() && flagged.is_probe_only() && !seed(3, "k").is_probe_only());

        let mut too_many = seed(3, "k");
        too_many.removed_paths = (0..=MAX_REMOVED_PATHS)
            .map(|n| format!("/tv/{n}"))
            .collect();
        assert!(too_many.validate().is_err());
        let mut control = seed(3, "k");
        control.removed_paths = vec!["/tv/a\nb".into()];
        assert!(control.validate().is_err());
        let mut oversized = seed(3, "k");
        oversized.removed_paths = (0..8)
            .map(|n| format!("/tv/{n}/{}", "x".repeat(4000)))
            .collect();
        assert!(oversized.validate().is_err(), "the column holds 16 KiB");
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
