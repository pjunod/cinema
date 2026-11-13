//! Cluster coordination for content-addressed VOD fragment indexes.
//!
//! Only small ownership and queue facts belong in the replicated store.  The
//! encoded index itself is deliberately returned to the daemon as an opaque,
//! checksummed blob and lives in a node-local or explicitly shared cache.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::fmp4::{CutClass, PromotionInputs};
use crate::segplan::{FragmentIndex, IndexRow, SourceIdentity, SEGPLAN_VERSION};

pub(crate) fn analysis_live_viewer_clause(now: &str) -> String {
    format!(
        "(analysis_requests.component = 'fragment_index' AND EXISTS (
        SELECT 1 FROM background_job_waiters viewer
        WHERE viewer.request_scope = 'playback-analysis'
          AND viewer.job_id = analysis_requests.request_id
          AND viewer.state = 'pending' AND viewer.deadline_ms > {now}))"
    )
}

pub const CLUSTER_FRAGMENT_INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cluster_fragment_index_sources (
    node_id          TEXT NOT NULL,
    file_id          INTEGER NOT NULL,
    object_version   TEXT NOT NULL,
    source_size      INTEGER NOT NULL,
    source_mtime     INTEGER NOT NULL,
    source_sha256    TEXT NOT NULL,
    observed_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (node_id, file_id)
) STRICT;

CREATE TABLE IF NOT EXISTS cluster_fragment_index_jobs (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_due
    ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_artifacts (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    blob_sha256       TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    built_by_node_id  TEXT NOT NULL,
    built_at_ms       INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_artifacts_file
    ON cluster_fragment_index_artifacts(file_id, source_size, source_mtime, pipeline_sha256);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_locations (
    cache_key         TEXT NOT NULL,
    node_id           TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    verified_at_ms    INTEGER NOT NULL,
    last_seen_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (cache_key, node_id)
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_locations_node
    ON cluster_fragment_index_locations(node_id, last_seen_at_ms, cache_key);

CREATE TRIGGER IF NOT EXISTS cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_deleted'
     WHERE file_id = OLD.id AND state IN ('queued', 'running');
END;
"#;

/// The terms of [`analysis_source_capacity_clause`], each with the reason it
/// names. Each term is evaluated for one required resource (`required`) of
/// one `analysis_requests` row; any true term refuses the source read.
///
/// `exclude_self` leaves the row's own holdings out of every count, so the
/// same rule can be asked again after the claim, while the request holds its
/// reservation and custody (`BackgroundJobStore::analysis_source_admission`).
/// Claim selection passes `false`: an unclaimed row holds nothing.
pub(crate) fn analysis_source_capacity_terms(
    now: &str,
    exclude_self: bool,
) -> [(&'static str, String); 4] {
    let (own_occupancy, own_reservation) = if exclude_self {
        (
            " AND NOT (held.owner_kind='analysis' AND held.work_id=analysis_requests.request_id)",
            " AND held.request_id<>analysis_requests.request_id",
        )
    } else {
        ("", "")
    };
    [
        // A reader whose authority lapsed may still be reading.
        (
            "source_custody_unsettled",
            format!(
                r#"EXISTS(SELECT 1 FROM source_io_occupancy held WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io')))
          AND held.custody_id IS NOT NULL AND held.active_occupancy=1 AND held.expires_at_ms<={now}{own_occupancy})"#
            ),
        ),
        // A foreground stream needs this domain now.
        (
            "foreground_source",
            format!(
                r#"EXISTS(SELECT 1 FROM foreground_source_permissions fg WHERE (fg.resource_key=required.resource_key OR fg.resource_key='source_io' OR required.resource_key='source_io')
          AND (fg.lease_expires_at_ms>{now} OR fg.custody_held=1)
          AND (fg.lease_expires_at_ms<={now} OR COALESCE(fg.permits_through_ms,0)<={now}))"#
            ),
        ),
        // Two readers already occupy the domain.
        (
            "domain_readers_full",
            format!(
                r#"(SELECT COUNT(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io')))
          AND held.active_occupancy=1 AND (held.expires_at_ms>{now} OR held.custody_id IS NOT NULL){own_occupancy})>=2"#
            ),
        ),
        // Without a live viewer, a background read takes only an idle domain.
        (
            "background_reader_busy",
            format!(
                r#"(NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
              WHERE viewer.request_scope = 'playback-analysis'
                AND viewer.job_id = analysis_requests.request_id
                AND viewer.state = 'pending' AND viewer.deadline_ms > {now})
          AND (SELECT COUNT(*) FROM background_job_reservations held
              WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io'))) AND held.expires_at_ms > {now}
                AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
                    WHERE viewer.job_id = held.job_id
                      AND viewer.state IN ('pending','awaiting_hydration')
                      AND (viewer.deadline_ms IS NULL OR viewer.deadline_ms > {now})
                      AND (viewer.consumer_kind = 'playback_fragment'
                        OR (viewer.consumer_kind = 'background_delivery' AND EXISTS (
                            SELECT 1 FROM background_job_waiters parent
                            WHERE parent.job_id = viewer.consumer_ref
                              AND parent.target_node_id = viewer.target_node_id
                              AND parent.consumer_kind = 'playback_fragment'
                              AND parent.state IN ('pending','awaiting_hydration')
                              AND parent.deadline_ms > {now}))
                        OR (viewer.consumer_kind = 'subtitle_source' AND EXISTS (
                            SELECT 1 FROM analysis_requests source
                            WHERE source.request_id = viewer.consumer_ref
                              AND source.component = 'subtitle_source'
                              AND source.trigger = 'playback'
                              AND source.state IN ('queued','running'))))))
            + (SELECT COUNT(*) FROM analysis_source_reservations held
                WHERE (held.resource_key=required.resource_key OR (held.resource_key LIKE 'source_io%' AND required.resource_key LIKE 'source_io%' AND (held.resource_key='source_io' OR required.resource_key='source_io'))) AND held.expires_at_ms > {now}{own_reservation}
                  AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
                      WHERE viewer.request_scope = 'playback-analysis'
                        AND viewer.job_id = held.request_id
                        AND viewer.state = 'pending' AND viewer.deadline_ms > {now}))
            + (SELECT COUNT(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held
              WHERE held.owner_kind IN ('lease','offline') AND held.active_occupancy=1
                AND (held.resource_key=required.resource_key OR held.resource_key='source_io' OR required.resource_key='source_io')
                AND (held.expires_at_ms>{now} OR held.custody_id IS NOT NULL)) >= 1)"#
            ),
        ),
    ]
}

/// Shared capacity predicate for the pre-artifact source read. Both Store
/// backends place it in candidate selection and the fenced claim mutation.
pub(crate) fn analysis_source_capacity_clause(now: &str) -> String {
    let [custody, foreground, readers, background] =
        analysis_source_capacity_terms(now, false).map(|(_, term)| term);
    format!(
        "NOT EXISTS (SELECT 1 FROM analysis_required_resources required
      WHERE required.request_id = analysis_requests.request_id AND (
        {custody}
        OR {foreground}
        OR {readers}
        OR {background}))"
    )
}

/// The cross-node arm of the analysis claim (`cross_node`): a fragment-index
/// row another live member targeted. `node` is the claimant's placeholder;
/// the claim's own digest filter already pins `pipeline_version`.
pub(crate) fn analysis_cross_node_clause(node: &str) -> String {
    format!(
        "(analysis_requests.component = 'fragment_index' AND analysis_requests.target_node_id <> ''
        AND analysis_requests.target_node_id <> {node}
        AND NOT EXISTS (SELECT 1 FROM settings
          WHERE key = 'internal.cluster_job_owner_removed.' || analysis_requests.target_node_id))"
    )
}

/// How many copy-video identities one claim may name as ones its executor
/// cannot emit. An identity is an argv fingerprint shared by every file of
/// one pipeline class, so a handful covers a library; the bound keeps the
/// bound parameter small on every claim.
pub const MAX_ANALYSIS_CLAIM_EXCLUDED_IDENTITIES: usize = 256;

/// The claim's executor exclusion: another node's row naming a copy-video
/// identity in `excluded` (a JSON array) is not this executor's to take. The
/// claimant's own and untargeted rows are never excluded -- an identity its
/// own node cannot emit is superseded there, not handed on. `node` and
/// `excluded` are the claim's placeholders.
pub(crate) fn analysis_executor_exclusion_clause(node: &str, excluded: &str) -> String {
    format!(
        "(analysis_requests.target_node_id IN ({node}, '')
        OR analysis_requests.video_identity NOT IN (SELECT value FROM json_each({excluded})))"
    )
}

/// The claim's order: work a viewer is waiting on first, wherever it is
/// targeted (a foreign row with a live playback waiter outranks this node's
/// own background rows), then this node's own and untargeted rows, then age
/// (forced work boosted). `node`, `now` and `boost` are the claim's
/// placeholders.
pub(crate) fn analysis_claim_order(node: &str, now: &str, boost: &str) -> String {
    format!(
        "CASE WHEN (component != 'fragment_index' AND priority = 'foreground')
            OR (component = 'fragment_index' AND EXISTS (
                SELECT 1 FROM background_job_waiters waiter
                WHERE waiter.request_scope = 'playback-analysis'
                  AND waiter.job_id = analysis_requests.request_id
                  AND waiter.state = 'pending' AND waiter.deadline_ms > {now}))
            THEN 0 ELSE 1 END,
         CASE WHEN target_node_id IN ({node}, '') THEN 0 ELSE 1 END,
         created_at_ms - CASE WHEN priority = 'forced' THEN {boost} ELSE 0 END,
         created_at_ms, request_id"
    )
}

/// How many files one [`ClusterFragmentIndexStore::fragment_index_requests_for_files`]
/// read may name.
pub const MAX_FILE_REQUEST_LOOKUP: usize = 256;

/// The request per file for `fragment_index_requests_for_files`. `files` is
/// the JSON array of file ids; columns are `REQUEST_COLS` from `ranked`.
pub(crate) fn fragment_index_requests_for_files_sql(files: &str, columns: &str) -> String {
    format!(
        "WITH ranked AS (
           SELECT analysis_requests.*, ROW_NUMBER() OVER (
             PARTITION BY file_id
             ORDER BY CASE WHEN state IN ('queued', 'running', 'submitted') THEN 0 ELSE 1 END,
                      created_at_ms DESC, request_id DESC) AS file_rank
             FROM analysis_requests
            WHERE component = 'fragment_index'
              AND file_id IN (SELECT CAST(value AS INTEGER) FROM json_each({files})))
         SELECT {columns} FROM ranked WHERE file_rank = 1 ORDER BY file_id"
    )
}

/// Validate a per-file lookup and bind its file ids as one JSON array.
pub(crate) fn file_request_lookup_json(file_ids: &[i64]) -> Result<String, StoreError> {
    if file_ids.len() > MAX_FILE_REQUEST_LOOKUP || file_ids.iter().any(|id| *id <= 0) {
        return Err(StoreError::Task("invalid file request lookup".to_owned()));
    }
    serde_json::to_string(file_ids).map_err(|error| StoreError::Task(error.to_string()))
}

/// Validate a claim's exclusion list and bind it as one JSON array.
pub(crate) fn analysis_claim_exclusions_json(excluded: &[String]) -> Result<String, StoreError> {
    if excluded.len() > MAX_ANALYSIS_CLAIM_EXCLUDED_IDENTITIES
        || excluded
            .iter()
            .any(|identity| identity.is_empty() || identity.len() > 128)
    {
        return Err(StoreError::Task("invalid analysis claim".to_owned()));
    }
    serde_json::to_string(excluded).map_err(|error| StoreError::Task(error.to_string()))
}

/// The same rule as [`analysis_source_capacity_clause`], asked of a claimed
/// request: `"admit"`, the first term's reason, or `"not_owned"` when the
/// row is no longer this owner's running fence (or membership removed the
/// owner, as `background_source_may_continue` also refuses). `$1` is
/// `{request_id, owner_node_id, fence, now_ms}`.
pub(crate) fn analysis_source_admission_sql() -> String {
    let now = "json_extract($1,'$.now_ms')";
    let mut cases = String::new();
    for (reason, term) in analysis_source_capacity_terms(now, true) {
        cases.push_str(&format!(
            "\n  WHEN EXISTS (SELECT 1 FROM analysis_requests JOIN analysis_required_resources required
        ON required.request_id = analysis_requests.request_id
      WHERE analysis_requests.request_id = json_extract($1,'$.request_id') AND {term})
    THEN '\"{reason}\"'"
        ));
    }
    format!(
        "SELECT CASE
  WHEN NOT EXISTS (SELECT 1 FROM analysis_requests
      WHERE request_id = json_extract($1,'$.request_id') AND state = 'running'
        AND owner_node_id = json_extract($1,'$.owner_node_id')
        AND fence = json_extract($1,'$.fence') AND cancel_requested = 0
        AND lease_expires_ms > {now}
        AND NOT EXISTS (SELECT 1 FROM settings
          WHERE key = 'internal.cluster_job_owner_removed.' || analysis_requests.owner_node_id))
    THEN '\"not_owned\"'{cases}
  ELSE '\"admit\"' END AS result_json"
    )
}

/// v31/v13 schema for durable operator requests before content addressing.
/// Kept separate from [`CLUSTER_FRAGMENT_INDEX_SCHEMA`] so migration fixtures
/// and deployed v30/v12 databases retain their historical shape.
pub const ANALYSIS_REQUESTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS analysis_requests (
    request_id         TEXT PRIMARY KEY,
    file_id            INTEGER NOT NULL,
    source_size        INTEGER NOT NULL,
    source_mtime       INTEGER NOT NULL,
    component          TEXT NOT NULL CHECK (component IN ('fragment_index')),
    force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
    target_node_id     TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
    owner_node_id      TEXT,
    fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms   INTEGER,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms      INTEGER NOT NULL,
    result_cache_key   TEXT,
    last_error_code    TEXT,
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS analysis_requests_due
    ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX IF NOT EXISTS analysis_requests_status
    ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE UNIQUE INDEX IF NOT EXISTS analysis_requests_one_active_source
    ON analysis_requests(file_id, source_size, source_mtime, component, target_node_id)
    WHERE state IN ('queued', 'running', 'submitted');

CREATE TRIGGER IF NOT EXISTS analysis_requests_cancel_source BEFORE DELETE ON files
BEGIN
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_deleted'
     WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
END;
CREATE TRIGGER IF NOT EXISTS analysis_requests_supersede_source
AFTER UPDATE OF size, mtime ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
BEGIN
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_superseded'
     WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
END;
CREATE TRIGGER IF NOT EXISTS analysis_requests_bound_terminal_history
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.state IN ('ready', 'failed', 'cancelled')
BEGIN
    DELETE FROM analysis_requests
     WHERE request_id IN (
       SELECT request_id FROM analysis_requests
        WHERE state IN ('ready', 'failed', 'cancelled')
          AND request_id <> NEW.request_id
        ORDER BY updated_at_ms, request_id
        LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
END;
"#;

/// v33/v15 query indexes for the paginated operator history and its compact
/// polling projection. This is a separate migration from the v31/v13 table so
/// deployed databases receive it instead of only fresh installations.
pub const ANALYSIS_HISTORY_INDEX_SCHEMA: &str = r#"
CREATE INDEX IF NOT EXISTS analysis_requests_result_history
    ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
    WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_status_history
    ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key);
"#;

/// Keeps the terminal-history retention trigger from searching every request
/// for each candidate after the retained window fills. The predicate exactly
/// matches the trigger's `newer` subquery so SQLite can prove that this partial
/// index is eligible.
pub const ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA: &str = r#"
CREATE INDEX IF NOT EXISTS analysis_requests_terminal_identity
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation, target_node_id,
                         updated_at_ms, request_id)
    WHERE state IN ('ready', 'failed', 'cancelled') AND force_rebuild = 0;
"#;

/// Durable content-analysis diagnostics, retry-cycle fencing, exact request
/// identity, and once-per-source repair receipts.
pub const CONTENT_ANALYSIS_REPAIR_SCHEMA: &str = r#"
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN index_retry_deadline_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN index_diagnostic_json TEXT NOT NULL DEFAULT '';
CREATE TABLE analysis_index_repairs (
    repair_revision       TEXT NOT NULL,
    file_id               INTEGER NOT NULL,
    source_size           INTEGER NOT NULL,
    source_mtime          INTEGER NOT NULL,
    source_sha256         TEXT NOT NULL,
    pipeline_sha256       TEXT NOT NULL,
    target_node_id        TEXT NOT NULL,
    video_identity        TEXT NOT NULL,
    predecessor_cache_key TEXT NOT NULL,
    predecessor_fence     INTEGER NOT NULL,
    successor_request_id  TEXT NOT NULL,
    created_at_ms         INTEGER NOT NULL,
    PRIMARY KEY (
      repair_revision, file_id, source_size, source_mtime, source_sha256,
      pipeline_sha256, target_node_id
    )
) STRICT;
CREATE INDEX analysis_index_repairs_successor
    ON analysis_index_repairs(successor_request_id);
CREATE TRIGGER analysis_index_repairs_delete_source AFTER DELETE ON files
BEGIN
    DELETE FROM analysis_index_repairs WHERE file_id = OLD.id;
END;
DROP INDEX IF EXISTS analysis_requests_one_active_forced_successor;
DROP INDEX IF EXISTS analysis_requests_one_active_source;
CREATE UNIQUE INDEX analysis_requests_one_active_source
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, video_identity,
                         requested_generation, target_node_id)
    WHERE state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_skip_successor
    ON analysis_requests(file_id, source_size, source_mtime, component)
    WHERE component = 'skip_markers' AND force_rebuild = 1
      AND state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_fragment_successor
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, video_identity)
    WHERE component = 'fragment_index' AND force_rebuild = 1
      AND state IN ('queued', 'running', 'submitted');
"#;

/// v41/v22 widens the already-durable request identity to the replicated
/// semantic component. A table rebuild is required because SQLite cannot
/// alter a CHECK constraint in place.
pub const ANALYSIS_COMPONENTS_SCHEMA: &str = r#"
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal'
      CHECK (priority IN ('normal','forced','foreground'));
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN trigger TEXT NOT NULL DEFAULT 'background'
      CHECK (trigger IN ('admin','background','foreground'));
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN target_node_id TEXT NOT NULL DEFAULT '';
DROP TRIGGER IF EXISTS cluster_fragment_indexes_cancel_source;
DROP INDEX IF EXISTS cluster_fragment_index_jobs_status_history;
DROP INDEX IF EXISTS cluster_fragment_index_jobs_due;
ALTER TABLE cluster_fragment_index_jobs RENAME TO cluster_fragment_index_jobs_v40;
CREATE TABLE cluster_fragment_index_jobs (
    cache_key         TEXT NOT NULL,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    priority          TEXT NOT NULL DEFAULT 'normal'
      CHECK (priority IN ('normal','forced','foreground')),
    trigger           TEXT NOT NULL DEFAULT 'background'
      CHECK (trigger IN ('admin','background','foreground')),
    target_node_id    TEXT NOT NULL DEFAULT '',
    state             TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (cache_key, target_node_id)
) STRICT;
INSERT INTO cluster_fragment_index_jobs
    (cache_key, file_id, source_size, source_mtime, source_sha256,
     pipeline_sha256, priority, trigger, target_node_id, state,
     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
     last_error_code, created_at_ms, updated_at_ms)
SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
       pipeline_sha256, priority, trigger,
       COALESCE((SELECT request.target_node_id FROM analysis_requests request
                  WHERE request.result_cache_key = cluster_fragment_index_jobs_v40.cache_key
                  ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), target_node_id), state,
       owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
       last_error_code, created_at_ms, updated_at_ms
  FROM cluster_fragment_index_jobs_v40;
DROP TABLE cluster_fragment_index_jobs_v40;
CREATE INDEX cluster_fragment_index_jobs_due
    ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key, target_node_id);
CREATE INDEX cluster_fragment_index_jobs_status_history
    ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key, target_node_id);
CREATE TRIGGER cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, last_error_code = 'source_deleted',
           updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
     WHERE file_id = OLD.id AND state IN ('queued', 'running');
END;
ALTER TABLE timeline_annotation_sets
    ADD COLUMN publication_priority TEXT NOT NULL DEFAULT 'normal'
      CHECK (publication_priority IN ('normal','forced'));
DROP TABLE IF EXISTS analysis_attempts;
DROP TABLE IF EXISTS cluster_fragment_index_heads;
DROP TRIGGER IF EXISTS analysis_requests_bound_terminal_history;
DROP TRIGGER IF EXISTS analysis_requests_supersede_source;
DROP TRIGGER IF EXISTS analysis_requests_cancel_source;
DROP INDEX IF EXISTS analysis_requests_result_history;
DROP INDEX IF EXISTS analysis_requests_one_active_forced_successor;
DROP INDEX IF EXISTS analysis_requests_one_active_source;
DROP INDEX IF EXISTS analysis_requests_status;
DROP INDEX IF EXISTS analysis_requests_due;
ALTER TABLE analysis_requests RENAME TO analysis_requests_v40;
CREATE TABLE analysis_requests (
    request_id         TEXT PRIMARY KEY,
    file_id            INTEGER NOT NULL,
    source_size        INTEGER NOT NULL,
    source_mtime       INTEGER NOT NULL,
    component          TEXT NOT NULL CHECK (component IN ('fragment_index','skip_markers')),
    pipeline_version   TEXT NOT NULL DEFAULT 'legacy-fragment-index',
    requested_generation TEXT NOT NULL DEFAULT 'legacy-generation',
    expected_predecessor_generation TEXT NOT NULL DEFAULT '',
    priority           TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','forced')),
    trigger            TEXT NOT NULL DEFAULT 'admin' CHECK (trigger IN ('admin','background')),
    force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
    target_node_id     TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
    owner_node_id      TEXT,
    fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms   INTEGER,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms      INTEGER NOT NULL,
    result_cache_key   TEXT,
    last_error_code    TEXT,
    cancel_requested   INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
INSERT INTO analysis_requests
    (request_id, file_id, source_size, source_mtime, component,
     pipeline_version, requested_generation, expected_predecessor_generation, priority, trigger,
     force_rebuild, target_node_id, state, owner_node_id, fence,
     lease_expires_ms, attempts, not_before_ms, result_cache_key,
     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
SELECT request_id, file_id, source_size, source_mtime, component,
       CASE component WHEN 'fragment_index' THEN 'legacy-fragment-index'
                      ELSE 'chapter-classifier-v1' END,
       request_id, '', CASE force_rebuild WHEN 1 THEN 'forced' ELSE 'normal' END,
       'admin', force_rebuild, target_node_id, state, owner_node_id, fence,
       lease_expires_ms, attempts, not_before_ms, result_cache_key,
       last_error_code, 0, created_at_ms, updated_at_ms
  FROM analysis_requests_v40;
DROP TABLE analysis_requests_v40;
CREATE INDEX analysis_requests_due
    ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX analysis_requests_status
    ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE UNIQUE INDEX analysis_requests_one_active_source
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation, target_node_id)
    WHERE state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_successor
    ON analysis_requests(file_id, source_size, source_mtime, component)
    WHERE force_rebuild = 1 AND state IN ('queued', 'running', 'submitted');
CREATE INDEX analysis_requests_result_history
    ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
    WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE TRIGGER analysis_requests_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs WHERE file_id = OLD.id);
    UPDATE analysis_attempts
       SET phase = 'canceled', phase_updated_at_ms = MAX(phase_updated_at_ms, OLD.scanned_at * 1000),
           terminal_code = 'source_deleted'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = OLD.id AND state IN ('running', 'submitted'));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_deleted', updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
     WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
END;
CREATE TRIGGER analysis_requests_supersede_source
AFTER UPDATE OF size, mtime ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs
        WHERE file_id = NEW.id
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, last_error_code = 'source_superseded',
           updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
    UPDATE analysis_attempts
       SET phase = 'stale', phase_updated_at_ms = MAX(phase_updated_at_ms, NEW.scanned_at * 1000),
           terminal_code = 'source_superseded'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = NEW.id AND state IN ('running', 'submitted')
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_superseded', updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
END;
CREATE TRIGGER analysis_requests_bound_terminal_history
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.state IN ('ready', 'failed', 'cancelled')
BEGIN
    DELETE FROM analysis_requests
     WHERE request_id IN (
       SELECT candidate.request_id FROM analysis_requests candidate
        WHERE candidate.state IN ('ready', 'failed', 'cancelled')
          AND candidate.request_id <> NEW.request_id
          AND (candidate.force_rebuild = 1
            OR NOT EXISTS (SELECT 1 FROM files
                 WHERE files.id = candidate.file_id
                   AND files.size = candidate.source_size
                   AND files.mtime = candidate.source_mtime)
            OR EXISTS (SELECT 1 FROM analysis_requests newer
                 WHERE newer.state IN ('ready', 'failed', 'cancelled')
                   AND newer.force_rebuild = 0
                   AND newer.file_id = candidate.file_id
                   AND newer.source_size = candidate.source_size
                   AND newer.source_mtime = candidate.source_mtime
                   AND newer.component = candidate.component
                   AND newer.pipeline_version = candidate.pipeline_version
                   AND newer.requested_generation = candidate.requested_generation
                   AND newer.target_node_id = candidate.target_node_id
                   AND (newer.updated_at_ms > candidate.updated_at_ms
                     OR (newer.updated_at_ms = candidate.updated_at_ms
                       AND newer.request_id > candidate.request_id))))
        ORDER BY candidate.updated_at_ms, candidate.request_id
        LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
END;
CREATE TABLE analysis_attempts (
    request_id          TEXT NOT NULL REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
    attempt             INTEGER NOT NULL CHECK (attempt > 0),
    claim_node_id       TEXT NOT NULL,
    claim_epoch         INTEGER NOT NULL CHECK (claim_epoch > 0),
    claim_expires_at_ms INTEGER NOT NULL,
    phase               TEXT NOT NULL CHECK (
        phase IN ('claimed','source_probe','hashing','staged','running','publishing','retry_wait','published','failed','canceled','stale')),
    started_at_ms       INTEGER NOT NULL,
    phase_updated_at_ms INTEGER NOT NULL,
    terminal_code       TEXT,
    PRIMARY KEY (request_id, claim_epoch)
) STRICT;
CREATE INDEX analysis_attempts_recent
    ON analysis_attempts(request_id, claim_epoch DESC);
CREATE TABLE cluster_fragment_index_heads (
    logical_cache_key   TEXT PRIMARY KEY,
    generation_cache_key TEXT NOT NULL,
    request_id          TEXT NOT NULL,
    updated_at_ms       INTEGER NOT NULL
) STRICT;
CREATE TABLE analysis_lifecycle_counters (
    event  TEXT NOT NULL CHECK (event IN ('claim','lease_loss','retry','cancel','stale','failure','publication')),
    reason TEXT NOT NULL CHECK (reason IN (
      'all','lease_expired','source_catalog_read_failed','source_probe_timeout',
      'source_unavailable','source_attestation_failed','foreground_preempted',
      'source_attestation_timeout','source_record_failed','queue_write_failed',
      'queue_full_or_busy','pipeline_version_unavailable','admin_cancelled',
      'source_deleted','source_identity_changed','attempt_limit','stored_probe_invalid',
      'source_duration_missing','invalid_cache_identity','unsupported','other','validated')),
    count INTEGER NOT NULL DEFAULT 0 CHECK (count >= 0),
    PRIMARY KEY (event, reason)
) STRICT;
CREATE TRIGGER analysis_requests_lifecycle_counters
AFTER UPDATE OF state ON analysis_requests
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'retry', CASE NEW.last_error_code
        WHEN 'lease_expired' THEN 'lease_expired'
        WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
        WHEN 'source_probe_timeout' THEN 'source_probe_timeout'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
        WHEN 'foreground_preempted' THEN 'foreground_preempted'
        WHEN 'source_attestation_timeout' THEN 'source_attestation_timeout'
        WHEN 'source_record_failed' THEN 'source_record_failed'
        WHEN 'queue_write_failed' THEN 'queue_write_failed'
        WHEN 'queue_full_or_busy' THEN 'queue_full_or_busy'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        ELSE 'other' END, 1
       WHERE OLD.state = 'running' AND NEW.state = 'queued'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'cancel', CASE NEW.last_error_code
        WHEN 'admin_cancelled' THEN 'admin_cancelled'
        WHEN 'source_deleted' THEN 'source_deleted'
        ELSE 'other' END, 1
       WHERE NEW.state = 'cancelled'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'stale', 'source_identity_changed', 1
       WHERE NEW.state IN ('failed','cancelled')
         AND NEW.last_error_code IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'failure', CASE NEW.last_error_code
        WHEN 'attempt_limit' THEN 'attempt_limit'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        WHEN 'stored_probe_invalid' THEN 'stored_probe_invalid'
        WHEN 'source_duration_missing' THEN 'source_duration_missing'
        WHEN 'invalid_cache_identity' THEN 'invalid_cache_identity'
        WHEN 'unsupported' THEN 'unsupported'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        ELSE 'other' END, 1
       WHERE NEW.state = 'failed'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'publication', 'validated', 1
       WHERE NEW.state = 'ready' AND NEW.component = 'skip_markers'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
END;
CREATE TRIGGER cluster_fragment_index_lifecycle_counters
AFTER UPDATE OF state ON cluster_fragment_index_jobs
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'retry', CASE NEW.last_error_code
        WHEN 'lease_expired' THEN 'lease_expired'
        WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
        WHEN 'foreground_preempted' THEN 'foreground_preempted'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        ELSE 'other' END, 1
       WHERE OLD.state = 'running' AND NEW.state = 'queued'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'cancel', CASE NEW.last_error_code
        WHEN 'source_deleted' THEN 'source_deleted' ELSE 'other' END, 1
       WHERE NEW.state = 'cancelled'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'stale', 'source_identity_changed', 1
       WHERE NEW.state IN ('failed','cancelled')
         AND NEW.last_error_code IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'failure', CASE NEW.last_error_code
        WHEN 'attempt_limit' THEN 'attempt_limit'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        WHEN 'unsupported' THEN 'unsupported'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        ELSE 'other' END, 1
       WHERE NEW.state = 'failed'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'publication', 'validated', 1 WHERE NEW.state = 'ready'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
END;
"#;

/// v47: cluster subtitle extraction requests and portable publication metadata.
/// This rebuild preserves live v46 requests and their charged attempt history.
pub const SUBTITLE_SOURCE_SCHEMA: &str = r#"
CREATE TABLE analysis_attempts_v47_backup AS SELECT * FROM analysis_attempts;
DROP TABLE analysis_attempts;
DROP TRIGGER IF EXISTS analysis_requests_cancel_source;
DROP TRIGGER IF EXISTS analysis_requests_supersede_source;
DROP TRIGGER IF EXISTS analysis_requests_bound_terminal_history;
DROP TRIGGER IF EXISTS analysis_requests_lifecycle_counters;
ALTER TABLE analysis_requests RENAME TO analysis_requests_v46;
CREATE TABLE analysis_requests (
    request_id         TEXT PRIMARY KEY,
    file_id            INTEGER NOT NULL,
    source_size        INTEGER NOT NULL,
    source_mtime       INTEGER NOT NULL,
    component          TEXT NOT NULL CHECK (component IN ('fragment_index','skip_markers','subtitle_source')),
    video_identity     TEXT NOT NULL DEFAULT '',
    pipeline_version   TEXT NOT NULL DEFAULT 'legacy-fragment-index',
    requested_generation TEXT NOT NULL DEFAULT 'legacy-generation',
    expected_predecessor_generation TEXT NOT NULL DEFAULT '',
    priority           TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','forced','foreground')),
    trigger            TEXT NOT NULL DEFAULT 'admin' CHECK (trigger IN ('admin','background','playback')),
    force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
    target_node_id     TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
    owner_node_id      TEXT,
    fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms   INTEGER,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms      INTEGER NOT NULL,
    result_cache_key   TEXT,
    last_error_code    TEXT,
    cancel_requested   INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
INSERT INTO analysis_requests (request_id, file_id, source_size, source_mtime, component, video_identity, pipeline_version, requested_generation, expected_predecessor_generation, priority, trigger, force_rebuild, target_node_id, state, owner_node_id, fence, lease_expires_ms, attempts, not_before_ms, result_cache_key, last_error_code, cancel_requested, created_at_ms, updated_at_ms) SELECT request_id, file_id, source_size, source_mtime, component, video_identity, pipeline_version, requested_generation, expected_predecessor_generation, priority, trigger, force_rebuild, target_node_id, state, owner_node_id, fence, lease_expires_ms, attempts, not_before_ms, result_cache_key, last_error_code, cancel_requested, created_at_ms, updated_at_ms FROM analysis_requests_v46;
DROP TABLE analysis_requests_v46;
CREATE INDEX analysis_requests_due
    ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX analysis_requests_status
    ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE UNIQUE INDEX analysis_requests_one_active_source ON analysis_requests(file_id, source_size, source_mtime, component, pipeline_version, video_identity, requested_generation, target_node_id) WHERE state IN ('queued','running','submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_skip_successor ON analysis_requests(file_id, source_size, source_mtime, component) WHERE component = 'skip_markers' AND force_rebuild = 1 AND state IN ('queued','running','submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_fragment_successor ON analysis_requests(file_id, source_size, source_mtime, component, pipeline_version, video_identity) WHERE component = 'fragment_index' AND force_rebuild = 1 AND state IN ('queued','running','submitted');
CREATE INDEX analysis_requests_result_history
    ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
    WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE INDEX analysis_requests_terminal_identity
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation, target_node_id,
                         updated_at_ms, request_id)
    WHERE state IN ('ready', 'failed', 'cancelled') AND force_rebuild = 0;
CREATE TRIGGER analysis_requests_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs WHERE file_id = OLD.id);
    UPDATE analysis_attempts
       SET phase = 'canceled', phase_updated_at_ms = MAX(phase_updated_at_ms, OLD.scanned_at * 1000),
           terminal_code = 'source_deleted'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = OLD.id AND state IN ('running', 'submitted'));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_deleted', updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
     WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
END;
CREATE TRIGGER analysis_requests_supersede_source
AFTER UPDATE OF size, mtime ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs
        WHERE file_id = NEW.id
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, last_error_code = 'source_superseded',
           updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
    UPDATE analysis_attempts
       SET phase = 'stale', phase_updated_at_ms = MAX(phase_updated_at_ms, NEW.scanned_at * 1000),
           terminal_code = 'source_superseded'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = NEW.id AND state IN ('running', 'submitted')
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_superseded', updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
END;
CREATE TRIGGER analysis_requests_bound_terminal_history
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.state IN ('ready', 'failed', 'cancelled')
BEGIN
    DELETE FROM analysis_requests
     WHERE request_id IN (
       SELECT candidate.request_id FROM analysis_requests candidate
        WHERE candidate.state IN ('ready', 'failed', 'cancelled')
          AND candidate.request_id <> NEW.request_id
          AND (candidate.force_rebuild = 1
            OR NOT EXISTS (SELECT 1 FROM files
                 WHERE files.id = candidate.file_id
                   AND files.size = candidate.source_size
                   AND files.mtime = candidate.source_mtime)
            OR EXISTS (SELECT 1 FROM analysis_requests newer
                 WHERE newer.state IN ('ready', 'failed', 'cancelled')
                   AND newer.force_rebuild = 0
                   AND newer.file_id = candidate.file_id
                   AND newer.source_size = candidate.source_size
                   AND newer.source_mtime = candidate.source_mtime
                   AND newer.component = candidate.component
                   AND newer.pipeline_version = candidate.pipeline_version
                   AND newer.requested_generation = candidate.requested_generation
                   AND newer.target_node_id = candidate.target_node_id
                   AND (newer.updated_at_ms > candidate.updated_at_ms
                     OR (newer.updated_at_ms = candidate.updated_at_ms
                       AND newer.request_id > candidate.request_id))))
        ORDER BY candidate.updated_at_ms, candidate.request_id
        LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
END;
CREATE TABLE analysis_attempts (
    request_id          TEXT NOT NULL REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
    attempt             INTEGER NOT NULL CHECK (attempt > 0),
    claim_node_id       TEXT NOT NULL,
    claim_epoch         INTEGER NOT NULL CHECK (claim_epoch > 0),
    claim_expires_at_ms INTEGER NOT NULL,
    phase               TEXT NOT NULL CHECK (
        phase IN ('claimed','source_probe','hashing','staged','running','publishing','retry_wait','published','failed','canceled','stale')),
    started_at_ms       INTEGER NOT NULL,
    phase_updated_at_ms INTEGER NOT NULL,
    terminal_code       TEXT,
    PRIMARY KEY (request_id, claim_epoch)
) STRICT;
CREATE INDEX analysis_attempts_recent
    ON analysis_attempts(request_id, claim_epoch DESC);
INSERT INTO analysis_attempts SELECT * FROM analysis_attempts_v47_backup;
DROP TABLE analysis_attempts_v47_backup;
CREATE TRIGGER analysis_requests_lifecycle_counters
AFTER UPDATE OF state ON analysis_requests
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'retry', CASE NEW.last_error_code
        WHEN 'lease_expired' THEN 'lease_expired'
        WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
        WHEN 'source_probe_timeout' THEN 'source_probe_timeout'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
        WHEN 'foreground_preempted' THEN 'foreground_preempted'
        WHEN 'source_attestation_timeout' THEN 'source_attestation_timeout'
        WHEN 'source_record_failed' THEN 'source_record_failed'
        WHEN 'queue_write_failed' THEN 'queue_write_failed'
        WHEN 'queue_full_or_busy' THEN 'queue_full_or_busy'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        ELSE 'other' END, 1
       WHERE OLD.state = 'running' AND NEW.state = 'queued'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'cancel', CASE NEW.last_error_code
        WHEN 'admin_cancelled' THEN 'admin_cancelled'
        WHEN 'source_deleted' THEN 'source_deleted'
        ELSE 'other' END, 1
       WHERE NEW.state = 'cancelled'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'stale', 'source_identity_changed', 1
       WHERE NEW.state IN ('failed','cancelled')
         AND NEW.last_error_code IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'failure', CASE NEW.last_error_code
        WHEN 'attempt_limit' THEN 'attempt_limit'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        WHEN 'stored_probe_invalid' THEN 'stored_probe_invalid'
        WHEN 'source_duration_missing' THEN 'source_duration_missing'
        WHEN 'invalid_cache_identity' THEN 'invalid_cache_identity'
        WHEN 'unsupported' THEN 'unsupported'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        ELSE 'other' END, 1
       WHERE NEW.state = 'failed'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'publication', 'validated', 1
       WHERE NEW.state = 'ready' AND NEW.component = 'skip_markers'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
END;
CREATE TABLE IF NOT EXISTS subtitle_source_publications (
 file_id INTEGER NOT NULL, source_size INTEGER NOT NULL, source_mtime INTEGER NOT NULL,
 source_attestation TEXT NOT NULL, node_id TEXT NOT NULL, ordinal INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('pgs','text','text_styled')),
 format TEXT NOT NULL CHECK(format IN ('sup','webvtt','matroska')),
 verdict TEXT NOT NULL CHECK(verdict IN ('kept','empty','malformed','transient')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
 origin TEXT NOT NULL CHECK(origin IN ('extracted','hydrated')),
 sha256 TEXT NOT NULL DEFAULT '', bytes INTEGER NOT NULL DEFAULT 0 CHECK(bytes >= 0),
 published_at_ms INTEGER NOT NULL,
 PRIMARY KEY(file_id, source_size, source_mtime, node_id, ordinal, format)
) STRICT;
CREATE INDEX IF NOT EXISTS subtitle_source_publications_stamp ON subtitle_source_publications(file_id, source_size, source_mtime, ordinal, format);
CREATE INDEX IF NOT EXISTS subtitle_source_publications_node ON subtitle_source_publications(node_id, file_id);
CREATE TRIGGER IF NOT EXISTS subtitle_source_publications_delete_source AFTER DELETE ON files BEGIN
 DELETE FROM subtitle_source_publications WHERE file_id = OLD.id;
 END;
CREATE TRIGGER IF NOT EXISTS subtitle_source_publications_supersede_source AFTER UPDATE OF size, mtime ON files
 WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime BEGIN
 DELETE FROM subtitle_source_publications WHERE file_id = NEW.id AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
 END;
CREATE TABLE IF NOT EXISTS subtitle_source_repair_epochs (
 file_id INTEGER NOT NULL, source_size INTEGER NOT NULL, source_mtime INTEGER NOT NULL,
 pipeline_version TEXT NOT NULL, next_epoch INTEGER NOT NULL CHECK(next_epoch >= 0),
 last_1_ms INTEGER, last_2_ms INTEGER, last_3_ms INTEGER,
 PRIMARY KEY(file_id, source_size, source_mtime, pipeline_version)
) STRICT;
CREATE TRIGGER IF NOT EXISTS subtitle_source_repair_epochs_advance
 AFTER UPDATE OF state ON analysis_requests
 WHEN OLD.state = 'ready' AND NEW.state = 'cancelled'
  AND NEW.component = 'subtitle_source'
  AND NEW.last_error_code IN ('artifact_lost','coverage_incomplete') BEGIN
 INSERT INTO subtitle_source_repair_epochs
  (file_id, source_size, source_mtime, pipeline_version,
   next_epoch, last_1_ms, last_2_ms, last_3_ms)
 VALUES (NEW.file_id, NEW.source_size, NEW.source_mtime, NEW.pipeline_version,
         1, NEW.updated_at_ms, NULL, NULL)
 ON CONFLICT(file_id, source_size, source_mtime, pipeline_version)
 DO UPDATE SET next_epoch = next_epoch + 1,
               last_3_ms = last_2_ms, last_2_ms = last_1_ms,
               last_1_ms = NEW.updated_at_ms;
 END;
CREATE TRIGGER IF NOT EXISTS subtitle_source_repair_epochs_delete_source AFTER DELETE ON files BEGIN
 DELETE FROM subtitle_source_repair_epochs WHERE file_id = OLD.id;
 END;
CREATE TRIGGER IF NOT EXISTS subtitle_source_repair_epochs_supersede_source AFTER UPDATE OF size, mtime ON files
 WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime BEGIN
 DELETE FROM subtitle_source_repair_epochs WHERE file_id = NEW.id
  AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
 END;
"#;

/// SQLite v46 / replicated v26: the code each *charged* attempt ended with.
///
/// A job row keeps one `last_error_code`, wiped on every claim and overwritten
/// by the terminal `attempt_limit`, so the row that dies carries no trace of
/// why. 1,933 rows died that way in three days and the cause had to be
/// recovered from the aggregate lifecycle counters. This is a column rather
/// than rows in `analysis_attempts` because a job can exist without a request
/// — playback's foreground enqueue makes one — and that table is keyed by
/// request fence.
///
/// Bounded by `MAX_ANALYSIS_MAX_ATTEMPTS` entries of `MAX_ERROR_CODE_BYTES`:
/// only charged attempts append, so an uncharged yield cannot grow it without
/// limit, and every reopen that resets `attempts` resets the history with it.
pub const ANALYSIS_ATTEMPT_ERRORS_SCHEMA: &str = r#"
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN attempt_errors TEXT NOT NULL DEFAULT '';
"#;

/// Which copy-video identity a fragment-index request is for.
///
/// A file has one to three of them — stripped, preserved, converting — and
/// before this the queue could only ever ask for one. A request resolved
/// whichever identity lacked an index first, and its row then became the dedup
/// tombstone that stopped background discovery asking for the others. So a
/// Dolby Vision Profile 7 title got its stripped identity and never its
/// converting one, which is why no converting fragment index existed anywhere
/// on the fleet: only a play attempt or an admin request could reach it.
///
/// Empty for `skip_markers`, which has no copy-video identity, and empty on
/// every row written before this column existed. Empty therefore means
/// "whichever identity is next", which is exactly the old behaviour, so a
/// request in flight across the upgrade resolves the way it always did.
///
/// The forced-successor cancellation is why this has to be a column rather
/// than something folded into an existing one. Forcing a rebuild of one
/// identity must cancel the pending background request for *that* identity and
/// leave its siblings alone; `cancelled` is terminal, so cancelling all three
/// would refuse their generations for good. Nothing else on the row can carry
/// it: `requested_generation` is a fresh UUID on a forced request by design,
/// `pipeline_version` is the engine digest the resolver compares directly and
/// the store dedups and prunes on, and `expected_predecessor_generation` is
/// the heads CAS token.
pub const ANALYSIS_REQUEST_IDENTITY_SCHEMA: &str = r#"
ALTER TABLE analysis_requests
    ADD COLUMN video_identity TEXT NOT NULL DEFAULT '';
"#;

pub const MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES: usize = 32 * 1024 * 1024;
pub const CONTENT_ANALYSIS_REPAIR_REVISION: &str = "video-completion-v1";
pub const CONTENT_ANALYSIS_REPAIR_MAX_CANDIDATES: i64 = 1_500;
pub const CONTENT_ANALYSIS_REPAIR_HEADROOM: i64 = 512;
pub const DEFAULT_ANALYSIS_MAX_ATTEMPTS: i64 = 5;
pub const MAX_ANALYSIS_MAX_ATTEMPTS: i64 = 20;
pub const DEFAULT_ANALYSIS_LEASE_SECS: i64 = 60;
pub const MAX_ANALYSIS_LEASE_SECS: i64 = 600;
pub const DEFAULT_ANALYSIS_BACKOFF_BASE_SECS: i64 = 5;
pub const MAX_ANALYSIS_BACKOFF_BASE_SECS: i64 = 300;
pub const DEFAULT_ANALYSIS_BACKOFF_MAX_SECS: i64 = 300;
pub const MAX_ANALYSIS_BACKOFF_MAX_SECS: i64 = 3_600;
/// Forward subtitle materialization span. Kept with the other bounded server
/// settings so every consumer resolves the same default and clamp.
pub const DEFAULT_SUBTITLE_WINDOW_SECS: i64 = 200;
pub const MIN_SUBTITLE_WINDOW_SECS: i64 = 30;
pub const MAX_SUBTITLE_WINDOW_SECS: i64 = 900;
/// Virtual head start for forced work. Ordering by the adjusted creation time
/// lets an operator request pass recent background work while guaranteeing
/// that background work older than this window cannot be starved by a stream
/// of newer forced requests.
pub const ANALYSIS_FORCED_PRIORITY_BOOST_MS: i64 = 5 * 60 * 1_000;

pub fn bounded_analysis_max_attempts(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_ANALYSIS_MAX_ATTEMPTS)
        .clamp(1, MAX_ANALYSIS_MAX_ATTEMPTS)
}

fn bounded_analysis_seconds(value: Option<&str>, default: i64, min: i64, max: i64) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

pub fn bounded_analysis_lease_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_LEASE_SECS,
        15,
        MAX_ANALYSIS_LEASE_SECS,
    )
}

pub fn bounded_analysis_backoff_base_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_BACKOFF_BASE_SECS,
        1,
        MAX_ANALYSIS_BACKOFF_BASE_SECS,
    )
}

pub fn bounded_analysis_backoff_max_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_BACKOFF_MAX_SECS,
        1,
        MAX_ANALYSIS_BACKOFF_MAX_SECS,
    )
}

/// Read a stored boolean setting, one way, in one place.
///
/// Three separate parses of the same stored string is how a server ends up
/// serving overlays while the Settings card's checkbox says off: the reader,
/// the settings DTO and the readiness route each decided independently what
/// `"On"` meant. `default_on` carries the only real difference between these
/// switches — most are off unless turned on, and the Dolby Vision conversion
/// is on unless turned off.
///
/// Every spelling an operator plausibly writes by hand is accepted in both
/// directions, case-folded and trimmed. A value that is neither takes the
/// default rather than guessing, because a typo is not an instruction.
#[must_use]
pub fn stored_switch(value: Option<&str>, default_on: bool) -> bool {
    match value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => true,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => false,
        _ => default_on,
    }
}

pub fn bounded_subtitle_window_seconds(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_SUBTITLE_WINDOW_SECS,
        MIN_SUBTITLE_WINDOW_SECS,
        MAX_SUBTITLE_WINDOW_SECS,
    )
}

/// Stable capped exponential jitter shared by request workers and both store
/// backends when they recover an expired lease. Queue identities are ASCII
/// (UUID request ids or hexadecimal content keys), which keeps this formula
/// identical to the SQLite expression used by replicated and local stores.
pub fn analysis_backoff_ms(identity: &str, attempt: i64, base_ms: i64, max_ms: i64) -> i64 {
    let attempt = attempt.max(1);
    let shift = u32::try_from(attempt.saturating_sub(1).min(30)).unwrap_or(0);
    let exponential = base_ms
        .max(1)
        .saturating_mul(1_i64.checked_shl(shift).unwrap_or(i64::MAX));
    let mut chars = identity.chars();
    let first = i64::from(chars.next().map(u32::from).unwrap_or_default());
    let last = i64::from(
        identity
            .chars()
            .next_back()
            .map(u32::from)
            .unwrap_or_default(),
    );
    let seed = i64::try_from(identity.chars().count())
        .unwrap_or(i64::MAX)
        .saturating_mul(17)
        .saturating_add(first.saturating_mul(31))
        .saturating_add(last.saturating_mul(13))
        .saturating_add(attempt.saturating_mul(7));
    let jitter_percent = 75_i64.saturating_add(seed.rem_euclid(51));
    exponential
        .saturating_mul(jitter_percent)
        .saturating_div(100)
        .clamp(1_000, max_ms.max(1_000))
}

/// Portable SQLite/Postgres projection used by both store backends. Retained
/// request generations are authoritative history. A mutable worker job is
/// attached only to the newest request that references its cache key; jobs
/// created solely by background analysis remain standalone rows.
pub(super) const ANALYSIS_CANONICAL_CTE: &str = r#"WITH request_ranked AS (
  SELECT request.*, ROW_NUMBER() OVER (
    PARTITION BY COALESCE(NULLIF(result_cache_key, ''), request_id), target_node_id
    ORDER BY updated_at_ms DESC, request_id DESC) AS cache_rank
    FROM analysis_requests request
), canonical AS (
  SELECT 'request:' || request.request_id AS row_key,
         request.request_id AS request_id,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.cache_key, '') ELSE '' END AS job_id,
         request.file_id AS file_id,
         CASE WHEN files.id IS NULL THEN 0 ELSE 1 END AS file_available,
         COALESCE(files.item_id, 0) AS item_id,
         COALESCE(items.title, '') AS title,
         request.component AS component,
         request.force_rebuild AS force_rebuild,
         request.target_node_id AS target_node_id,
         request.state AS request_state,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.state, '') ELSE '' END AS job_state,
         CASE
           WHEN request.cache_rank = 1 AND job.state = 'ready'
             AND request.component = 'fragment_index'
             AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                  WHERE location.cache_key = job.cache_key
                    AND location.node_id = request.target_node_id)
             THEN request.state
           WHEN request.cache_rank = 1 THEN COALESCE(job.state, request.state)
           ELSE request.state
         END AS state,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.owner_node_id, '') <> ''
              THEN job.owner_node_id ELSE COALESCE(request.owner_node_id, '') END AS owner_node_id,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.fence ELSE request.fence END AS claim_epoch,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.lease_expires_ms, 0) > 0
              THEN job.lease_expires_ms ELSE COALESCE(request.lease_expires_ms, 0) END AS lease_expires_ms,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.attempts ELSE request.attempts END AS attempts,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.not_before_ms ELSE request.not_before_ms END AS not_before_ms,
         COALESCE(request.last_error_code, '') AS request_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.last_error_code, '') ELSE '' END AS job_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.attempt_errors, '') ELSE '' END AS job_attempt_errors,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.index_retry_deadline_ms, 0) ELSE 0 END AS index_retry_deadline_ms,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.index_diagnostic_json, '') ELSE '' END AS index_diagnostic_json,
         request.created_at_ms AS created_at_ms,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.updated_at_ms, 0) > request.updated_at_ms
              THEN job.updated_at_ms ELSE request.updated_at_ms END AS updated_at_ms,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN SUBSTR(job.pipeline_sha256, 1, 12)
              ELSE request.pipeline_version END AS pipeline_version,
         request.requested_generation AS requested_generation,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.priority ELSE request.priority END AS priority,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.trigger ELSE request.trigger END AS trigger,
         request.cancel_requested AS cancel_requested,
         COALESCE((SELECT attempt.phase FROM analysis_attempts attempt
                    WHERE attempt.request_id = request.request_id
                    ORDER BY attempt.claim_epoch DESC LIMIT 1), '') AS phase,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.source_size ELSE request.source_size END AS source_size
    FROM request_ranked request
    LEFT JOIN cluster_fragment_index_jobs job
      ON request.cache_rank = 1 AND job.cache_key = request.result_cache_key
     AND job.target_node_id = request.target_node_id
    LEFT JOIN files ON files.id = request.file_id
    LEFT JOIN items ON items.id = files.item_id
  UNION ALL
  SELECT 'job:' || job.cache_key || ':' || job.target_node_id AS row_key,
         '' AS request_id, job.cache_key AS job_id, job.file_id AS file_id,
         CASE WHEN files.id IS NULL THEN 0 ELSE 1 END AS file_available,
         COALESCE(files.item_id, 0) AS item_id,
         COALESCE(items.title, '') AS title,
         'fragment_index' AS component, 0 AS force_rebuild,
         job.target_node_id AS target_node_id, '' AS request_state, job.state AS job_state,
         job.state AS state, COALESCE(job.owner_node_id, '') AS owner_node_id,
         job.fence AS claim_epoch,
         COALESCE(job.lease_expires_ms, 0) AS lease_expires_ms,
         job.attempts AS attempts, job.not_before_ms AS not_before_ms,
         '' AS request_error_code, COALESCE(job.last_error_code, '') AS job_error_code,
         COALESCE(job.attempt_errors, '') AS job_attempt_errors,
         job.index_retry_deadline_ms AS index_retry_deadline_ms,
         job.index_diagnostic_json AS index_diagnostic_json,
         job.created_at_ms AS created_at_ms, job.updated_at_ms AS updated_at_ms,
         SUBSTR(job.pipeline_sha256, 1, 12) AS pipeline_version,
         job.cache_key AS requested_generation,
         job.priority AS priority, job.trigger AS trigger, 0 AS cancel_requested,
         CASE job.state WHEN 'running' THEN 'hashing' WHEN 'queued' THEN 'claimed'
           WHEN 'ready' THEN 'published' WHEN 'failed' THEN 'failed'
           WHEN 'cancelled' THEN 'canceled' ELSE '' END AS phase,
         job.source_size AS source_size
    FROM cluster_fragment_index_jobs job
    LEFT JOIN files ON files.id = job.file_id
    LEFT JOIN items ON items.id = files.item_id
   WHERE NOT EXISTS (
     SELECT 1 FROM analysis_requests request
      WHERE request.result_cache_key = job.cache_key
        AND request.result_cache_key <> ''
        AND request.target_node_id = job.target_node_id)
), classified_base AS (
  SELECT canonical.*,
         CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
           WHEN 'submitted' THEN 2 WHEN 'failed' THEN 3
           WHEN 'cancelled' THEN 4 WHEN 'ready' THEN 5 ELSE 6 END AS sort_rank,
         CASE
           WHEN state IN ('queued', 'running', 'submitted')
             AND COALESCE(NULLIF(job_error_code, ''), request_error_code) <> '' THEN 'automatic'
           WHEN state IN ('queued', 'running', 'submitted') THEN 'working'
           WHEN state = 'ready' THEN 'ready'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'unsupported' THEN 'unsupported'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code)
             IN ('source_deleted', 'source_superseded') THEN 'expected'
           ELSE 'attention'
         END AS disposition
    FROM canonical
), classified AS (
  SELECT classified_base.*,
         CASE
           WHEN file_available = 0 THEN 'none'
           WHEN state = 'ready' THEN 'rebuild'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'source_superseded'
             THEN 'analyze_current'
           WHEN disposition = 'attention' THEN 'retry'
           ELSE 'none'
         END AS action
    FROM classified_base
)"#;

/// Bounded, join-free projection for the frequently polled status cards.
/// Operator requests already have a hard retained-history cap. Standalone
/// terminal worker rows are restricted to the newest retained window while
/// active and request-referenced jobs are always included.
pub(super) const ANALYSIS_SUMMARY_CTE: &str = r#"WITH request_ranked AS (
  SELECT request.*, ROW_NUMBER() OVER (
    PARTITION BY COALESCE(NULLIF(result_cache_key, ''), request_id), target_node_id
    ORDER BY updated_at_ms DESC, request_id DESC) AS cache_rank
    FROM analysis_requests request
), summary_job_keys AS (
  SELECT result_cache_key AS cache_key, target_node_id FROM analysis_requests
   WHERE result_cache_key IS NOT NULL AND result_cache_key <> ''
  UNION
  SELECT cache_key, target_node_id FROM cluster_fragment_index_jobs
   WHERE state IN ('queued', 'running')
  UNION
  SELECT cache_key, target_node_id FROM (
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'ready'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    UNION ALL
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'failed'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    UNION ALL
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'cancelled'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
), summary_jobs AS (
  SELECT job.* FROM cluster_fragment_index_jobs job
  JOIN summary_job_keys keys ON keys.cache_key = job.cache_key
   AND keys.target_node_id = job.target_node_id
), summary_canonical AS (
  SELECT 'request:' || request.request_id AS row_key,
         request.file_id AS file_id,
         CASE
           WHEN request.cache_rank = 1 AND job.state = 'ready'
             AND request.component = 'fragment_index'
             AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                  WHERE location.cache_key = job.cache_key
                    AND location.node_id = request.target_node_id)
             THEN request.state
           WHEN request.cache_rank = 1 THEN COALESCE(job.state, request.state)
           ELSE request.state
         END AS state,
         COALESCE(request.last_error_code, '') AS request_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.last_error_code, '') ELSE '' END AS job_error_code,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.updated_at_ms, 0) > request.updated_at_ms
              THEN job.updated_at_ms ELSE request.updated_at_ms END AS updated_at_ms
    FROM request_ranked request
    LEFT JOIN summary_jobs job
      ON request.cache_rank = 1 AND job.cache_key = request.result_cache_key
     AND job.target_node_id = request.target_node_id
  UNION ALL
  SELECT 'job:' || job.cache_key || ':' || job.target_node_id AS row_key, job.file_id AS file_id,
         job.state AS state, '' AS request_error_code,
         COALESCE(job.last_error_code, '') AS job_error_code,
         job.updated_at_ms AS updated_at_ms
    FROM summary_jobs job
   WHERE NOT EXISTS (
     SELECT 1 FROM analysis_requests request
      WHERE request.result_cache_key = job.cache_key
        AND request.result_cache_key <> ''
        AND request.target_node_id = job.target_node_id)
), summary_classified AS (
  SELECT summary_canonical.*,
         CASE
           WHEN state IN ('queued', 'running', 'submitted')
             AND COALESCE(NULLIF(job_error_code, ''), request_error_code) <> '' THEN 'automatic'
           WHEN state IN ('queued', 'running', 'submitted') THEN 'working'
           WHEN state = 'ready' THEN 'ready'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'unsupported' THEN 'unsupported'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code)
             IN ('source_deleted', 'source_superseded') THEN 'expected'
           ELSE 'attention'
         END AS disposition
    FROM summary_canonical
)"#;
const BLOB_MAGIC: &[u8; 8] = b"PLRXIDX2";
const BLOB_FORMAT_VERSION: u16 = 2;
const ROW_BYTES: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentIndexSourceObservation {
    pub node_id: String,
    pub file_id: i64,
    pub object_version: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub priority: String,
    pub trigger: String,
    pub target_node_id: String,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub priority: String,
    pub trigger: String,
    pub target_node_id: String,
    pub state: String,
    pub owner_node_id: String,
    pub fence: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_error_code: String,
    /// The code each *charged* attempt ended with, oldest first, comma
    /// separated. Never cleared by a claim; reset only where the retry budget
    /// itself resets. `last_error_code` is the terminal code the UI and the
    /// lifecycle triggers key on — this is the history behind it.
    pub attempt_errors: String,
    /// Fixed seven-day boundary for a typed retry cycle. Zero means no
    /// new-policy cycle has started.
    pub index_retry_deadline_ms: i64,
    /// Bounded versioned diagnostic stored by the same fenced transition.
    pub index_diagnostic_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAnalysisRequest {
    pub request_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub component: String,
    pub pipeline_version: String,
    /// The copy-video identity this request is for, as an argv fingerprint.
    /// Empty means "whichever identity is next", which is what every request
    /// written before the column existed meant, and what `skip_markers` — which
    /// has no copy-video identity at all — always means.
    pub video_identity: String,
    pub requested_generation: String,
    pub priority: String,
    pub trigger: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRequest {
    pub request_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub component: String,
    pub pipeline_version: String,
    /// The copy-video identity this request is for, as an argv fingerprint.
    /// Empty means "whichever identity is next", which is what every request
    /// written before the column existed meant, and what `skip_markers` — which
    /// has no copy-video identity at all — always means.
    pub video_identity: String,
    pub requested_generation: String,
    pub expected_predecessor_generation: String,
    pub priority: String,
    pub trigger: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub state: String,
    pub owner_node_id: String,
    pub fence: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub result_cache_key: String,
    pub last_error_code: String,
    pub cancel_requested: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Persisted analysis IDs are opaque UTF-8 strings. Repair admission has
/// historically allowed 128 bytes, so custody must accept that full range.
pub(super) fn analysis_request_id_is_valid(request_id: &str) -> bool {
    !request_id.is_empty() && request_id.len() <= 128
}

/// Normal creation and reconciliation retain their existing 64-byte bound;
/// the wider persisted-ID policy preserves already-admitted repair owners.
pub(super) fn new_analysis_request_id_is_valid(request_id: &str) -> bool {
    analysis_request_id_is_valid(request_id) && request_id.len() <= 64
}

pub(super) const ANALYSIS_RECONCILE_SQL: &str = include_str!("analysis_reconcile.sql");

pub(super) fn reconciliation_parameters(
    old: &AnalysisRequest,
    new: &NewAnalysisRequest,
) -> Result<(String, String), StoreError> {
    if old.component != "fragment_index"
        || old.requested_generation.starts_with("predict:")
        || new.component != old.component
        || new.file_id != old.file_id
        || new.video_identity != old.video_identity
        || new.force_rebuild != old.force_rebuild
        || new.request_id == old.request_id
        || !new_analysis_request_id_is_valid(&new.request_id)
        || new.priority != old.priority
        || new.trigger != old.trigger
        || new.target_node_id.is_empty()
        || new.target_node_id.len() > 128
        || new.pipeline_version.is_empty()
        || new.pipeline_version.len() > 128
        || new.requested_generation.is_empty()
        || new.requested_generation.len() > 128
    {
        return Err(StoreError::Task("invalid analysis reconciliation".into()));
    }
    Ok((
        serde_json::to_string(old).map_err(|error| StoreError::Task(error.to_string()))?,
        serde_json::to_string(new).map_err(|error| StoreError::Task(error.to_string()))?,
    ))
}

/// Portable source identity for a cluster subtitle extraction request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleSourceStamp {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub pipeline_version: String,
}

impl SubtitleSourceStamp {
    pub fn generation(&self, repair_epoch: i64) -> String {
        let mut digest = Sha256::new();
        digest.update(b"plurx/subtitle-source-request/v1\0");
        update_field(&mut digest, self.file_id.to_string().as_bytes());
        update_field(&mut digest, self.source_size.to_string().as_bytes());
        update_field(&mut digest, self.source_mtime.to_string().as_bytes());
        update_field(&mut digest, b"subtitle_source");
        update_field(&mut digest, self.pipeline_version.as_bytes());
        update_field(&mut digest, repair_epoch.to_string().as_bytes());
        hex::encode(digest.finalize())
    }
}

pub const SUBTITLE_SOURCE_REPAIR_LIMIT: i64 = 3;
pub const SUBTITLE_SOURCE_REPAIR_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;
pub const SUBTITLE_BACKFILL_TERMINAL_SETTLE_MS: i64 = 120_000;

/// An already-ready request can still have an eligible ordinal without
/// settled extraction coverage. Keep this predicate identical in the two
/// store implementations so a partial publication can be repaired without
/// discarding the ordinals that did settle.
pub(crate) const SUBTITLE_SOURCE_READY_UNCOVERED: &str = r#"EXISTS (
    SELECT 1 FROM files AS f,
      json_each(CASE WHEN json_valid(f.subtitle_streams)
                     THEN f.subtitle_streams ELSE '[]' END) AS s
    WHERE f.id = analysis_requests.file_id
      AND f.size = analysis_requests.source_size
      AND f.mtime = analysis_requests.source_mtime
      AND json_type(s.value, '$.index') = 'integer'
      AND CAST(json_extract(s.value, '$.index') AS INTEGER) >= 0
      AND lower(json_extract(s.value, '$.codec')) IN
          ('hdmv_pgs_subtitle','subrip','srt','ass','ssa','mov_text','webvtt','text')
      AND NOT EXISTS (
        SELECT 1 FROM subtitle_source_publications AS p
        WHERE p.file_id = f.id AND p.source_size = f.size
          AND p.source_mtime = f.mtime
          AND p.ordinal = CAST(json_extract(s.value, '$.index') AS INTEGER)
          AND p.origin = 'extracted'
          AND (p.verdict IN ('kept','empty','malformed')
               OR (p.verdict = 'transient' AND p.attempts >= 3))
      )
)"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleBackfillCandidate {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub scanned_at: i64,
    pub uncovered_ordinals: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleBackfillDiagnostics {
    /// Holder of the unexpired, cluster-wide discovery lease, if any.
    pub lease_holder: Option<String>,
    /// Files with at least one eligible scanner ordinal lacking settled
    /// extraction coverage, independent of queue cooldown or active work.
    pub remaining_files: i64,
    pub remaining_bytes: i64,
}

pub(crate) const SUBTITLE_BACKFILL_DIAGNOSTICS: &str = r#"
WITH uncovered AS (
 SELECT f.id, f.size
 FROM files AS f
 WHERE f.size >= 0
   AND f.container IS NOT NULL
   AND lower(f.container) NOT IN ('ts','m2ts','mts','m2t','mpegts','tp','trp')
   AND json_valid(f.subtitle_streams)
   AND EXISTS (
       SELECT 1 FROM json_each(f.subtitle_streams) AS s
       WHERE json_type(s.value, '$.index') = 'integer'
         AND CAST(json_extract(s.value, '$.index') AS INTEGER) >= 0
         AND lower(json_extract(s.value, '$.codec')) IN
             ('hdmv_pgs_subtitle','subrip','srt','ass','ssa','mov_text','webvtt','text')
         AND NOT EXISTS (
             SELECT 1 FROM subtitle_source_publications AS p
             WHERE p.file_id = f.id AND p.source_size = f.size
               AND p.source_mtime = f.mtime
               AND p.ordinal = CAST(json_extract(s.value, '$.index') AS INTEGER)
               AND p.origin = 'extracted'
               AND (p.verdict IN ('kept','empty','malformed')
                    OR (p.verdict = 'transient' AND p.attempts >= 3))
         )
   )
)
SELECT (SELECT owner_node_id FROM job_leases
        WHERE resource = 'media:subtitle-source:backfill' AND expires_at_ms > $1)
       AS lease_holder,
       COUNT(*) AS remaining_files,
       COALESCE(SUM(size), 0) AS remaining_bytes
FROM uncovered
"#;

/// Keyset-paged discovery. Scanner subtitle ordinals are used only to decide
/// whether a file merits a job; the worker still probes a held source fd and
/// maps its live ordinals. Coverage is per ordinal, from settled extracted
/// publications only. Hydrated rows cannot make backfill skip a source.
pub(crate) const SUBTITLE_BACKFILL_CANDIDATES: &str = r#"
WITH candidates AS (
 SELECT f.id AS file_id, f.size AS source_size, f.mtime AS source_mtime,
        f.scanned_at AS scanned_at,
        (SELECT COUNT(DISTINCT CAST(json_extract(s.value, '$.index') AS INTEGER))
         FROM json_each(f.subtitle_streams) AS s
         WHERE json_type(s.value, '$.index') = 'integer'
           AND CAST(json_extract(s.value, '$.index') AS INTEGER) >= 0
           AND lower(json_extract(s.value, '$.codec')) IN
               ('hdmv_pgs_subtitle','subrip','srt','ass','ssa','mov_text','webvtt','text')
           AND NOT EXISTS (
               SELECT 1 FROM subtitle_source_publications AS p
               WHERE p.file_id = f.id AND p.source_size = f.size
                 AND p.source_mtime = f.mtime
                 AND p.ordinal = CAST(json_extract(s.value, '$.index') AS INTEGER)
                 AND p.origin = 'extracted'
                 AND (p.verdict IN ('kept','empty','malformed')
                      OR (p.verdict = 'transient' AND p.attempts >= 3))
           )) AS uncovered_ordinals
 FROM files AS f
 WHERE (f.scanned_at < $1 OR (f.scanned_at = $1 AND f.id < $2))
   AND f.size >= 0
   AND f.container IS NOT NULL
   AND lower(f.container) NOT IN ('ts','m2ts','mts','m2t','mpegts','tp','trp')
   AND json_valid(f.subtitle_streams)
   AND NOT EXISTS (
       SELECT 1 FROM analysis_requests AS r
       WHERE r.file_id = f.id AND r.source_size = f.size
         AND r.source_mtime = f.mtime AND r.component = 'subtitle_source'
         AND (r.state IN ('queued','running','submitted')
              OR (r.state IN ('ready','failed','cancelled')
                  AND r.updated_at_ms >= $3))
   )
)
SELECT file_id,source_size,source_mtime,scanned_at,uncovered_ordinals
FROM candidates WHERE uncovered_ordinals > 0
ORDER BY scanned_at DESC,file_id DESC LIMIT $4
"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleSourcePublication {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    /// Portable sampled source digest (`FragmentIndexSourceObservation.source_sha256`).
    pub source_attestation: String,
    pub node_id: String,
    pub ordinal: i64,
    pub kind: String,
    pub format: String,
    pub verdict: String,
    pub attempts: i64,
    pub origin: String,
    pub sha256: String,
    pub bytes: i64,
    pub published_at_ms: i64,
}

pub(super) fn valid_subtitle_source_publication(p: &SubtitleSourcePublication) -> bool {
    p.file_id > 0
        && p.source_size >= 0
        && p.ordinal >= 0
        && p.attempts >= 0
        && !p.node_id.is_empty()
        && p.node_id.len() <= 128
        && p.source_attestation.len() == 64
        && p.source_attestation.bytes().all(|b| b.is_ascii_hexdigit())
        && matches!(p.kind.as_str(), "pgs" | "text" | "text_styled")
        && matches!(p.format.as_str(), "sup" | "webvtt" | "matroska")
        && matches!(
            p.verdict.as_str(),
            "kept" | "empty" | "malformed" | "transient"
        )
        && matches!(p.origin.as_str(), "extracted" | "hydrated")
        && (p.verdict != "kept"
            || (p.bytes > 0
                && p.sha256.len() == 64
                && p.sha256.bytes().all(|b| b.is_ascii_hexdigit())))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisIndexRepairCandidate {
    pub candidate_id: String,
    pub repair_revision: String,
    pub predecessor_cache_key: String,
    pub predecessor_fence: i64,
    pub predecessor_updated_at_ms: i64,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub pipeline_version: String,
    pub video_identity: String,
    pub target_node_id: String,
    pub cause: String,
    pub eligibility: String,
    pub successor_request_id: String,
}

impl AnalysisIndexRepairCandidate {
    pub fn recompute_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"plurx/content-analysis-repair-candidate\0");
        update_field(&mut digest, self.repair_revision.as_bytes());
        update_field(&mut digest, self.predecessor_cache_key.as_bytes());
        update_field(&mut digest, self.file_id.to_string().as_bytes());
        update_field(&mut digest, self.source_size.to_string().as_bytes());
        update_field(&mut digest, self.source_mtime.to_string().as_bytes());
        update_field(&mut digest, self.source_sha256.as_bytes());
        update_field(&mut digest, self.pipeline_sha256.as_bytes());
        update_field(&mut digest, self.pipeline_version.as_bytes());
        update_field(&mut digest, self.video_identity.as_bytes());
        update_field(&mut digest, self.target_node_id.as_bytes());
        update_field(&mut digest, self.predecessor_fence.to_string().as_bytes());
        update_field(
            &mut digest,
            self.predecessor_updated_at_ms.to_string().as_bytes(),
        );
        hex::encode(digest.finalize())
    }

    pub fn has_valid_id(&self) -> bool {
        self.candidate_id == self.recompute_id()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisIndexRepairResult {
    pub candidate_id: String,
    pub status: String,
    pub successor_request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisAttempt {
    pub request_id: String,
    pub attempt: i64,
    pub claim_node_id: String,
    pub claim_epoch: i64,
    pub claim_expires_at_ms: i64,
    pub phase: String,
    pub started_at_ms: i64,
    pub phase_updated_at_ms: i64,
    pub terminal_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisFileLabel {
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalysisHistoryFilter {
    #[default]
    All,
    Working,
    Attention,
    Ready,
    Expected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryCursor {
    pub sort_rank: i64,
    pub updated_at_ms: i64,
    pub row_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryQuery {
    pub limit: i64,
    pub cursor: Option<AnalysisHistoryCursor>,
    pub filter: AnalysisHistoryFilter,
    pub search: String,
    /// Canonical durable states (`queued`, `claimed`, `retry_wait`, ...).
    /// Empty means all states. Values are validated at the HTTP boundary.
    pub states: Vec<String>,
    /// Server-stamped time used to distinguish queued work from retry wait.
    pub now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisHistoryRow {
    pub row_key: String,
    pub request_id: String,
    pub job_id: String,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    pub component: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub request_state: String,
    pub job_state: String,
    pub state: String,
    pub disposition: String,
    pub action: String,
    pub owner_node_id: String,
    pub claim_epoch: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub request_error_code: String,
    pub job_error_code: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub pipeline_version: String,
    pub requested_generation: String,
    pub priority: String,
    pub trigger: String,
    pub cancel_requested: bool,
    pub phase: String,
    pub source_size: i64,
    /// The code each charged attempt of the joined job ended with, oldest
    /// first. Empty for a request with no job, and for the non-current
    /// generations of one.
    pub job_attempt_errors: String,
    pub index_retry_deadline_ms: i64,
    pub index_diagnostic_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryPage {
    pub rows: Vec<AnalysisHistoryRow>,
    pub filtered_total: i64,
    pub next_cursor: Option<AnalysisHistoryCursor>,
}

/// The ceiling `enqueue_analysis_request` refuses at. Exposed so a bulk
/// operator action can leave headroom rather than filling the table every
/// other producer needs to be able to enqueue into at all.
pub const MAX_ACTIVE_ANALYSIS_REQUESTS: i64 = 4_096;

/// The hard bound on *all* active analysis requests, runnable or not, as a
/// multiple of [`MAX_ACTIVE_ANALYSIS_REQUESTS`]: stranded backlog may not
/// grow the table without limit while it waits to be reconciled.
pub const ANALYSIS_BACKLOG_BOUND_FACTOR: i64 = 4;

/// The prefix of the durable fence membership writes for a removed (or
/// removal-pending) node. A request targeted at a fenced node is never worked
/// for: the claim's cross-node arm skips it, removal retires it
/// (`retire_analysis_requests_for_node`) and the settle pass catches any it
/// missed.
pub(crate) const REMOVED_TARGET_FENCE_PREFIX: &str = "internal.cluster_job_owner_removed.";

/// What the last worker inventory read knows about who can run a
/// fragment-index request: the fresh engine digest each reporting node runs,
/// and the committed members (this node included) when membership could be
/// read.
///
/// It carries the one rule for *stranded* work, shared by the enqueue cap,
/// the queue health (`incompatible` against `eligible`) and the background
/// reconciler. A pre-worker fragment-index request (`queued`, no worker cache
/// key) is claimable by its own target when that target runs its digest, and
/// by any other idle member that does (the claim's cross-node arm) unless the
/// target is fenced as removed. So it is **stranded** exactly when no live
/// worker runs its digest, or its target is removed
/// ([`analysis_request_stranded_sql`] in SQL, [`Self::is_stranded`] for a row
/// in hand). Which of the two remedies applies depends on the target
/// ([`Self::remedy`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveAnalysisEngines {
    /// Node id to the engine digest it reported inside the freshness window.
    pub engines: std::collections::BTreeMap<String, String>,
    /// Committed members, this node included; `None` when membership could
    /// not be read, which disables the absent-target remedy.
    pub members: Option<std::collections::BTreeSet<String>>,
}

/// What the background reconciler does with a stranded request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrandedRemedy {
    /// The target reports, fresh, this engine: replace the request onto it
    /// under the same target (`engine_changed`).
    TargetEngine(String),
    /// The target is not a committed member and reports nothing: re-home the
    /// request through the reconcile candidate's reassign path
    /// (`target_changed`), source checked on the new node. A removed
    /// target's request is never re-homed — the Store refuses it and removal
    /// retires it.
    TargetAbsent,
    /// The target is a member that has not reported inside the window:
    /// absence of evidence retires nothing. It stays counted `incompatible`
    /// until the target reports again or membership removes it.
    TargetSilent,
}

impl LiveAnalysisEngines {
    /// The distinct digests some live worker runs, the JSON array the SQL
    /// half binds.
    #[must_use]
    pub fn digests(&self) -> Vec<String> {
        self.engines
            .values()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The row-in-hand half of [`analysis_request_stranded_sql`], short of
    /// the removed-target fence (a Store fact; the reconcile statements
    /// refuse a fenced target themselves).
    #[must_use]
    pub fn is_stranded(&self, request: &AnalysisRequest) -> bool {
        request.component == "fragment_index"
            && request.state == "queued"
            && request.result_cache_key.is_empty()
            && !self
                .engines
                .values()
                .any(|engine| engine == &request.pipeline_version)
    }

    /// The remedy for a stranded request, `None` when it is not stranded.
    #[must_use]
    pub fn remedy(&self, request: &AnalysisRequest) -> Option<StrandedRemedy> {
        if !self.is_stranded(request) {
            return None;
        }
        Some(match self.engines.get(&request.target_node_id) {
            Some(engine) => StrandedRemedy::TargetEngine(engine.clone()),
            None if self
                .members
                .as_ref()
                .is_some_and(|members| !members.contains(&request.target_node_id)) =>
            {
                StrandedRemedy::TargetAbsent
            }
            None => StrandedRemedy::TargetSilent,
        })
    }
}

/// [`LiveAnalysisEngines`]'s stranded rule over an unqualified
/// `analysis_requests` row. `live` is the placeholder bound to the JSON array
/// of [`LiveAnalysisEngines::digests`], or NULL when the inventory is
/// unknown — then nothing is stranded, every row counts as before.
pub(crate) fn analysis_request_stranded_sql(live: &str) -> String {
    format!(
        "({live} IS NOT NULL AND component = 'fragment_index' AND state = 'queued' \
          AND COALESCE(result_cache_key, '') = '' \
          AND (pipeline_version NOT IN (SELECT value FROM json_each({live})) \
            OR EXISTS (SELECT 1 FROM settings \
              WHERE key = '{REMOVED_TARGET_FENCE_PREFIX}' || target_node_id)))"
    )
}

/// Room above [`MAX_ACTIVE_ANALYSIS_REQUESTS`] that only a viewer's own
/// request may use: copy preparation (`priority = foreground`,
/// `trigger = playback`) and a viewer's subtitle source (`foreground`). Background discovery that filled the queue to the ceiling refused
/// the one request a waiting viewer needed; the ceiling still bounds the
/// background producers, and this bounds the viewers.
pub const PLAYBACK_ANALYSIS_HEADROOM: i64 = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisStatusSummary {
    pub total: i64,
    pub working: i64,
    pub queued: i64,
    pub running: i64,
    pub submitted: i64,
    pub attention: i64,
    pub expected: i64,
    pub ready: i64,
    pub latest_error_code: String,
    pub latest_error_file_id: i64,
    pub latest_error_updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexArtifact {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub blob_sha256: String,
    pub bytes: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
}

/// One item-detail question for
/// [`ClusterFragmentIndexStore::cluster_fragment_index_availability`]: does
/// this node's attestation of the source object the caller just observed name
/// a published cluster artifact for this pipeline?
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterIndexAvailabilityProbe {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub object_version: String,
    pub pipeline_sha256: String,
}

pub(crate) fn check_cluster_index_availability_batch(
    probes: &[ClusterIndexAvailabilityProbe],
) -> Result<(), StoreError> {
    if probes.len() > super::FRAGMENT_INDEX_STATUS_CHUNK {
        return Err(StoreError::Task(
            "cluster index availability batch exceeds 256 probes".into(),
        ));
    }
    Ok(())
}

/// The logical cache key each probe resolves to through `attested` (this
/// node's source attestations for the probes' files), with the attested
/// source digest. Shared by both backends so they answer identically.
pub(crate) fn cluster_index_availability_keys(
    probes: &[ClusterIndexAvailabilityProbe],
    attested: &[FragmentIndexSourceObservation],
) -> Vec<Option<(String, String)>> {
    probes
        .iter()
        .map(|probe| {
            let observation = attested.iter().find(|observation| {
                observation.file_id == probe.file_id
                    && observation.object_version == probe.object_version
            })?;
            let key = cluster_fragment_index_key(
                probe.file_id,
                probe.source_size,
                probe.source_mtime,
                &observation.source_sha256,
                &probe.pipeline_sha256,
            )?;
            Some((key, observation.source_sha256.clone()))
        })
        .collect()
}

/// Whether each keyed probe has a published artifact: `artifacts` pairs each
/// logical key with the artifact its head currently resolves to. The same
/// acceptance the single-file lookup applies.
pub(crate) fn cluster_index_availability_answers(
    probes: &[ClusterIndexAvailabilityProbe],
    keyed: &[Option<(String, String)>],
    artifacts: &[(String, ClusterFragmentIndexArtifact)],
) -> Vec<bool> {
    probes
        .iter()
        .zip(keyed)
        .map(|(probe, keyed)| {
            keyed.as_ref().is_some_and(|(key, source_sha256)| {
                artifacts.iter().any(|(logical, artifact)| {
                    logical == key
                        && artifact.source_size == probe.source_size
                        && artifact.source_sha256 == *source_sha256
                        && artifact.pipeline_sha256 == probe.pipeline_sha256
                })
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexLocation {
    pub cache_key: String,
    pub node_id: String,
    pub bytes: i64,
    pub verified_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[async_trait]
pub trait ClusterFragmentIndexStore: Send + Sync + 'static {
    /// The Developer diagnostic reads coverage independently of queue state.
    async fn subtitle_backfill_diagnostics(
        &self,
        now_ms: i64,
    ) -> Result<SubtitleBackfillDiagnostics, StoreError>;

    /// Newest eligible live stamps with at least one uncovered scanner ordinal.
    /// Start at `(i64::MAX, i64::MAX)`; pass the last row's cursor onward.
    async fn subtitle_backfill_candidates(
        &self,
        after_scanned_at: i64,
        after_id: i64,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<SubtitleBackfillCandidate>, StoreError>;

    /// Atomically join or create a request. `None` means the bounded repair
    /// budget has been exhausted for this stamp during the preceding day.
    async fn enqueue_or_promote_subtitle_source(
        &self,
        stamp: &SubtitleSourceStamp,
        priority: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        self.enqueue_or_promote_subtitle_source_with(stamp, priority, now_ms, None)
            .await
    }

    /// [`Self::enqueue_or_promote_subtitle_source`] with the same cap as
    /// [`Self::enqueue_analysis_request_with`]: runnable work holds the
    /// ceiling, stranded fragment-index backlog only the hard bound.
    async fn enqueue_or_promote_subtitle_source_with(
        &self,
        stamp: &SubtitleSourceStamp,
        priority: &str,
        now_ms: i64,
        live_engines: Option<&LiveAnalysisEngines>,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    async fn retire_subtitle_source_ready(
        &self,
        stamp: &SubtitleSourceStamp,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Foreground may probe a live ordinal that is absent from stale scanner
    /// metadata. Retire only if that ordinal still lacks settled extracted
    /// coverage for its current portable source digest when the store applies
    /// the update.
    async fn retire_subtitle_source_ready_for_ordinal(
        &self,
        stamp: &SubtitleSourceStamp,
        ordinal: i64,
        source_attestation: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn upsert_subtitle_source_publication(
        &self,
        publication: &SubtitleSourcePublication,
    ) -> Result<(), StoreError>;

    async fn list_subtitle_source_publications(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    ) -> Result<Vec<SubtitleSourcePublication>, StoreError>;

    async fn delete_subtitle_source_publications(
        &self,
        file_id: i64,
        node_id: &str,
    ) -> Result<(), StoreError>;

    async fn delete_subtitle_source_publication(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        node_id: &str,
        ordinal: i64,
        format: &str,
    ) -> Result<(), StoreError>;
    /// Persist an operator request before the source digest (and therefore the
    /// content-addressed worker key) is known. Concurrent duplicate requests
    /// join the one active request for the same file/component/node.
    async fn enqueue_analysis_request(
        &self,
        request: &NewAnalysisRequest,
    ) -> Result<AnalysisRequest, StoreError> {
        self.enqueue_analysis_request_with(request, None).await
    }

    /// [`Self::enqueue_analysis_request`] with the cap measured against
    /// *runnable* work. `live_engines` is the last worker inventory; an
    /// active pre-worker `fragment_index` row it calls stranded
    /// ([`LiveAnalysisEngines`]) is backlog nobody can claim and does not hold the
    /// [`MAX_ACTIVE_ANALYSIS_REQUESTS`] ceiling against new runnable work —
    /// on 2026-11-07 3,878 such rows held it at 4,097/4,096 and refused every
    /// background enqueue. All active rows stay bounded by
    /// [`ANALYSIS_BACKLOG_BOUND_FACTOR`] times the ceiling. `None` (inventory
    /// unknown) counts every active row, as before.
    async fn enqueue_analysis_request_with(
        &self,
        request: &NewAnalysisRequest,
        live_engines: Option<&LiveAnalysisEngines>,
    ) -> Result<AnalysisRequest, StoreError>;

    async fn claim_analysis_request(
        &self,
        node_id: &str,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        self.claim_analysis_request_compatible(node_id, None, now_ms, lease_expires_ms)
            .await
    }

    /// Select and fence only fragment requests this worker can execute.
    /// `None` preserves the administrative/test claim surface; a daemon worker
    /// passes its actual engine identity. Other analysis components retain
    /// their own compatibility rules.
    async fn claim_analysis_request_compatible(
        &self,
        node_id: &str,
        pipeline_version: Option<&str>,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        self.claim_analysis_request_for_capacity(
            node_id,
            pipeline_version,
            now_ms,
            lease_expires_ms,
            false,
            false,
            false,
            &[],
        )
        .await
    }

    /// A busy playback node may attest only work with an unexpired viewer.
    /// Both candidate selection and the fenced claim recheck that interest;
    /// ordinary maintenance remains idle-only and consumes no retry attempt.
    ///
    /// `targeted_only` limits the claim to requests naming `node_id`: a
    /// learner resolves what is targeted at it, and leaves the untargeted
    /// requests any node may take to the voters.
    ///
    /// `cross_node` (with a `pipeline_version`) also admits fragment-index
    /// requests targeted at another node that asked for exactly this engine:
    /// an idle node takes a busy one's work, builds the index under the
    /// requester's `target_node_id`, and delivery hydrates the target. This
    /// node's own rows (and untargeted ones) come after work a viewer waits
    /// on, wherever that is targeted; a target membership has removed is
    /// never worked for.
    ///
    /// `excluded_video_identities` (at most
    /// [`MAX_ANALYSIS_CLAIM_EXCLUDED_IDENTITIES`]) are copy-video identities
    /// this executor has found it cannot emit: another node's row naming one
    /// stays queued for a node that can, instead of being claimed, refused
    /// and requeued uncharged here on every pass. The executor keeps the list
    /// (node-local); the claimant's own rows are never excluded.
    #[allow(clippy::too_many_arguments)]
    async fn claim_analysis_request_for_capacity(
        &self,
        node_id: &str,
        pipeline_version: Option<&str>,
        now_ms: i64,
        lease_expires_ms: i64,
        viewer_only: bool,
        targeted_only: bool,
        cross_node: bool,
        excluded_video_identities: &[String],
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Retire the active (queued, running or submitted) requests targeted at
    /// `node_id`, which membership has removed: nothing but the target ever
    /// claims a targeted request, so they would otherwise stay active --
    /// counted against [`MAX_ACTIVE_ANALYSIS_REQUESTS`] -- for good (a
    /// cross-node claim skips a removed target too). Each is
    /// cancelled with `node_removed`; returns how many.
    async fn retire_analysis_requests_for_node(
        &self,
        node_id: &str,
        now_ms: i64,
    ) -> Result<u64, StoreError>;

    async fn renew_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Advance the current fenced attempt's durable phase. The update is
    /// rejected after cancellation or ownership has moved to another fence.
    async fn record_analysis_request_phase(
        &self,
        request: &AnalysisRequest,
        phase: &str,
        terminal_code: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn analysis_attempts(
        &self,
        request_id: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisAttempt>, StoreError>;

    /// Atomically resolve a current request into its content-addressed worker
    /// row and transition the request to submitted. No stale request owner may
    /// enqueue or reopen work without also committing the fenced handoff.
    async fn submit_fragment_index_analysis(
        &self,
        request: &AnalysisRequest,
        job: &NewClusterFragmentIndexJob,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn retry_analysis_request(
        &self,
        request: &AnalysisRequest,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
        charge_attempt: bool,
    ) -> Result<bool, StoreError>;

    async fn fail_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Fold terminal fragment-index results into submitted operator requests.
    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError>;

    async fn prune_analysis_requests(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<u64, StoreError>;

    async fn analysis_requests(&self, limit: i64) -> Result<Vec<AnalysisRequest>, StoreError>;

    /// For each of `file_ids` (at most [`MAX_FILE_REQUEST_LOOKUP`]), the
    /// fragment-index request that says what "index now" would do: an active
    /// (`queued`, `running`, `submitted`) one when there is any, else the
    /// newest. Files with no request are absent. An advisory read from this
    /// node's replica: it labels an action the server re-validates.
    async fn fragment_index_requests_for_files(
        &self,
        file_ids: &[i64],
    ) -> Result<Vec<AnalysisRequest>, StoreError>;

    /// Complete keyset inventory of active requests, including older work.
    async fn analysis_reconciliation_page(
        &self,
        after: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisRequest>, StoreError>;

    /// The globally unique active forced slot for this source, engine and video.
    async fn analysis_reconciliation_forced_slot(
        &self,
        replacement: &NewAnalysisRequest,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Secure a current successor before retiring the exact queued predecessor.
    /// Running work, worker-owned retries and live playback interests are fenced
    /// out. Publication and historical attempts are never deleted.
    async fn reconcile_analysis_request(
        &self,
        expected: &AnalysisRequest,
        replacement: &NewAnalysisRequest,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Terminal requests that a bulk reopen should actually act on.
    ///
    /// Narrower than "everything failed", and the difference is what makes a
    /// bulk reopen safe to press twice. A row is reopenable only when no
    /// other request for the same source identity, component and target is
    /// `queued`, `running`, `submitted` or `ready` — so a file that a previous
    /// reopen already repaired, or whose successor is still in flight, is not
    /// offered again. Without that predicate the failed row stays in the
    /// operator's attention list forever after its successor succeeds, and
    /// every later press re-forces a library that is already indexed.
    ///
    /// Forced requests are excluded: a forced generation is somebody's
    /// deliberate one-off, not a queue fault to repair.
    ///
    /// Rows are ordered oldest-terminal first, so paging through a backlog
    /// makes progress rather than re-offering the same head.
    ///
    /// `error_code` narrows the selection to terminal rows whose
    /// `last_error_code` is exactly that code, so an operator can put back one
    /// cause (say, `pipeline_version_unavailable` after a deploy) without the
    /// rest.
    async fn reopenable_analysis_requests(
        &self,
        component: Option<&str>,
        error_code: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AnalysisRequest>, StoreError>;

    async fn analysis_request(
        &self,
        request_id: &str,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Explicit administrator retry. This creates a new request generation so
    /// the deterministic terminal identity remains as a discovery tombstone.
    async fn retry_analysis_request_admin(
        &self,
        request_id: &str,
        requested_generation: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    async fn preview_analysis_index_repairs(
        &self,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AnalysisIndexRepairCandidate>, StoreError>;

    async fn apply_analysis_index_repair(
        &self,
        candidate: &AnalysisIndexRepairCandidate,
        successor_request_id: &str,
        now_ms: i64,
    ) -> Result<AnalysisIndexRepairResult, StoreError>;

    /// Cancel an operator request and revoke any live request fence. A shared
    /// fragment artifact already published by another identity is untouched.
    async fn cancel_analysis_request_admin(
        &self,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Publish a non-fragment component directly under the exact live request
    /// fence after its replicated payload has been validated.
    async fn complete_analysis_request(
        &self,
        request: &AnalysisRequest,
        result_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Atomically publish a validated semantic set and consume the exact live
    /// request fence. A stale worker can do neither half.
    async fn publish_timeline_annotation_set_for_request(
        &self,
        request: &AnalysisRequest,
        duration_ms: i64,
        set: &crate::segplan::TimelineAnnotationSet,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Stable keyset page over operator requests plus background-only jobs.
    /// A worker job is attached only to the newest request that references its
    /// cache key, so retained request generations remain distinct history.
    async fn analysis_history(
        &self,
        query: &AnalysisHistoryQuery,
    ) -> Result<AnalysisHistoryPage, StoreError>;

    /// Compact aggregate used by frequently-polled operator surfaces.
    async fn analysis_status_summary(&self) -> Result<AnalysisStatusSummary, StoreError>;

    async fn analysis_file_labels(&self, limit: i64) -> Result<Vec<AnalysisFileLabel>, StoreError>;

    async fn cluster_fragment_index_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterFragmentIndexJob>, StoreError>;

    async fn cluster_fragment_index_job(
        &self,
        cache_key: &str,
        target_node_id: &str,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError>;

    /// Resolve the immutable generation currently published for a logical
    /// file/source/pipeline identity. Coverage work on another node must build
    /// this generation rather than creating an unused normal generation.
    async fn cluster_fragment_index_current_generation(
        &self,
        logical_cache_key: &str,
    ) -> Result<Option<String>, StoreError>;

    async fn record_fragment_index_source(
        &self,
        observation: &FragmentIndexSourceObservation,
    ) -> Result<(), StoreError>;

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError>;

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError>;

    /// Item detail's cluster-artifact fallback for a bounded batch, in input
    /// order: [`Self::fragment_index_source`] then
    /// [`Self::cluster_fragment_index_artifact`] for every probe, as one
    /// Store call. Callers chunk at
    /// [`FRAGMENT_INDEX_STATUS_CHUNK`](super::FRAGMENT_INDEX_STATUS_CHUNK).
    async fn cluster_fragment_index_availability(
        &self,
        node_id: &str,
        probes: &[ClusterIndexAvailabilityProbe],
    ) -> Result<Vec<bool>, StoreError>;

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError>;

    /// Reopen an existing catalog artifact whose physical holders could not
    /// supply a valid blob. The immutable artifact remains the expected
    /// identity while a fenced worker deterministically reconstructs it.
    async fn requeue_cluster_fragment_index(
        &self,
        replacement: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError>;

    async fn put_cluster_fragment_index_location(
        &self,
        location: &ClusterFragmentIndexLocation,
    ) -> Result<(), StoreError>;

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError>;

    /// Who built the fragment-index artifacts `node_id` holds a location
    /// for, for one file's source at `source_size`/`source_mtime`: the
    /// distinct `built_by_node_id` values, empty when this node holds none.
    ///
    /// The subtitle-source store reads it to say why a lookup missed. A node
    /// that only hydrated a peer's index never ran the pass that keeps PGS
    /// tracks, so its miss is `hydrated_only`, not `absent`; a node holding no
    /// index at all is `never_indexed`.
    async fn fragment_index_builders_held_by(
        &self,
        node_id: &str,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    ) -> Result<Vec<String>, StoreError>;

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError>;

    /// Remove a bounded set of terminal catalog generations that have not
    /// had a verified holder within the retention window. Returns artifact
    /// keys whose node-local bytes may now be removed.
    async fn prune_cluster_fragment_indexes(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, StoreError>;
}

/// Canonical structural work and artifact identity. Every component is
/// length-delimited. The file and source generation are intentionally part of
/// the key: byte-identical files remain independent work/coverage identities,
/// and replacing a file cannot attach its request to an older generation.
pub fn cluster_fragment_index_key(
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    source_sha256: &str,
    pipeline_sha256: &str,
) -> Option<String> {
    if file_id <= 0 || source_size < 0 || !is_sha256(source_sha256) || !is_sha256(pipeline_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/cache-key\0");
    digest.update(BLOB_FORMAT_VERSION.to_be_bytes());
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    digest.update(file_id.to_be_bytes());
    digest.update(source_size.to_be_bytes());
    digest.update(source_mtime.to_be_bytes());
    update_field(&mut digest, source_sha256.as_bytes());
    update_field(&mut digest, pipeline_sha256.as_bytes());
    Some(hex::encode(digest.finalize()))
}

/// Physical key for a forced successor. The logical content/pipeline key
/// remains the serving pointer; this key keeps the predecessor's immutable
/// bytes addressable until the successor wins publication CAS.
pub fn cluster_fragment_index_generation_key(
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    source_sha256: &str,
    pipeline_sha256: &str,
    generation: &str,
) -> Option<String> {
    if file_id <= 0
        || source_size < 0
        || !is_sha256(source_sha256)
        || !is_sha256(pipeline_sha256)
        || generation.trim().is_empty()
        || generation.len() > 128
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/generation-key\0");
    digest.update(BLOB_FORMAT_VERSION.to_be_bytes());
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    digest.update(file_id.to_be_bytes());
    digest.update(source_size.to_be_bytes());
    digest.update(source_mtime.to_be_bytes());
    update_field(&mut digest, source_sha256.as_bytes());
    update_field(&mut digest, pipeline_sha256.as_bytes());
    update_field(&mut digest, generation.as_bytes());
    Some(hex::encode(digest.finalize()))
}

/// Cryptographic digest of a pipeline vector. The daemon passes the exact
/// video-deciding argv plus its engine digest; source path is intentionally
/// excluded because the complete source digest already names the bytes.
pub fn cluster_fragment_index_pipeline_digest(
    engine_sha256: &str,
    args: &[String],
) -> Option<String> {
    if !is_sha256(engine_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/pipeline\0");
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    update_field(&mut digest, engine_sha256.as_bytes());
    for arg in args {
        update_field(&mut digest, arg.as_bytes());
    }
    Some(hex::encode(digest.finalize()))
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Serialize, Deserialize)]
struct BlobHeader {
    segplan_version: u32,
    timescale: u32,
    init_sha256: String,
    promotion: PromotionInputs,
    parameter_sets_constant: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dv_refit_origin: Option<crate::segplan::DvRefitOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dv_refit_proofs: Option<Vec<crate::segplan::refit_proof::FittedFragmentProof>>,
    source_sha256: String,
    pipeline_sha256: String,
    rows: usize,
}

pub fn encode_cluster_fragment_index_blob(
    index: &FragmentIndex,
    source_sha256: &str,
    pipeline_sha256: &str,
) -> Result<Vec<u8>, StoreError> {
    if index.version != SEGPLAN_VERSION
        || index.timescale == 0
        || index.rows.is_empty()
        || index
            .dv_refit_proofs
            .as_ref()
            .is_some_and(|proofs| proofs.len() != index.rows.len())
        || !is_sha256(source_sha256)
        || !is_sha256(pipeline_sha256)
    {
        return Err(StoreError::Task(
            "invalid cluster fragment-index artifact".to_owned(),
        ));
    }
    // Portable artifacts contain content-bound proof, never a worker-local
    // inode stamp. The reader binds it to its own fully attested source.
    let mut promotion = index.promotion.clone();
    if let Some(proof) = promotion.hevc_configuration.as_mut() {
        if proof.source_sha256.as_deref() != Some(source_sha256) {
            return Err(StoreError::Task(
                "HEVC proof lacks full source attestation".into(),
            ));
        }
        proof.source_object_version.clear();
        proof.source_node_id.clear();
    }
    let header = serde_json::to_vec(&BlobHeader {
        segplan_version: index.version,
        timescale: index.timescale,
        init_sha256: index.init_sha256.clone(),
        promotion,
        parameter_sets_constant: index.parameter_sets_constant,
        dv_refit_origin: index.dv_refit_origin,
        dv_refit_proofs: index.dv_refit_proofs.clone(),
        source_sha256: source_sha256.to_ascii_lowercase(),
        pipeline_sha256: pipeline_sha256.to_ascii_lowercase(),
        rows: index.rows.len(),
    })
    .map_err(|error| StoreError::Task(format!("encoding fragment-index header: {error}")))?;
    let rows_bytes = index
        .rows
        .len()
        .checked_mul(ROW_BYTES)
        .ok_or_else(|| StoreError::Task("fragment-index row size overflow".to_owned()))?;
    let capacity = 8_usize
        .checked_add(2 + 4)
        .and_then(|bytes| bytes.checked_add(header.len()))
        .and_then(|bytes| bytes.checked_add(rows_bytes))
        .ok_or_else(|| StoreError::Task("fragment-index blob size overflow".to_owned()))?;
    if capacity > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES {
        return Err(StoreError::Task(
            "fragment-index blob exceeds hard limit".to_owned(),
        ));
    }
    let mut blob = Vec::with_capacity(capacity);
    blob.extend_from_slice(BLOB_MAGIC);
    blob.extend_from_slice(&BLOB_FORMAT_VERSION.to_be_bytes());
    blob.extend_from_slice(
        &u32::try_from(header.len())
            .map_err(|_| StoreError::Task("fragment-index header is too large".to_owned()))?
            .to_be_bytes(),
    );
    blob.extend_from_slice(&header);
    for row in &index.rows {
        blob.extend_from_slice(&row.dts.to_le_bytes());
        blob.extend_from_slice(
            &u32::try_from(row.duration)
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        blob.extend_from_slice(&row.bytes.to_le_bytes());
        blob.extend_from_slice(&row.video_bytes.to_le_bytes());
        blob.push(class_code(row.class));
        blob.extend_from_slice(&[0, 0, 0]);
    }
    Ok(blob)
}

pub fn decode_cluster_fragment_index_blob(
    blob: &[u8],
    expected_source_sha256: &str,
    expected_pipeline_sha256: &str,
) -> Result<FragmentIndex, StoreError> {
    if blob.len() > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES || blob.len() < 14 {
        return Err(StoreError::Task(
            "invalid fragment-index blob size".to_owned(),
        ));
    }
    if &blob[..8] != BLOB_MAGIC {
        return Err(StoreError::Task(
            "invalid fragment-index blob magic".to_owned(),
        ));
    }
    let version = u16::from_be_bytes(blob[8..10].try_into().expect("two bytes"));
    if version != BLOB_FORMAT_VERSION {
        return Err(StoreError::Task(format!(
            "unsupported fragment-index blob version {version}"
        )));
    }
    let header_len = u32::from_be_bytes(blob[10..14].try_into().expect("four bytes")) as usize;
    let rows_offset = 14_usize
        .checked_add(header_len)
        .filter(|offset| *offset <= blob.len())
        .ok_or_else(|| StoreError::Task("truncated fragment-index blob header".to_owned()))?;
    let header: BlobHeader = serde_json::from_slice(&blob[14..rows_offset])
        .map_err(|error| StoreError::Task(format!("decoding fragment-index header: {error}")))?;
    if header.segplan_version != SEGPLAN_VERSION
        || header.timescale == 0
        || header.rows == 0
        || !header
            .source_sha256
            .eq_ignore_ascii_case(expected_source_sha256)
        || !header
            .pipeline_sha256
            .eq_ignore_ascii_case(expected_pipeline_sha256)
    {
        return Err(StoreError::Task(
            "fragment-index blob identity mismatch".to_owned(),
        ));
    }
    let packed = &blob[rows_offset..];
    if packed.len() != header.rows.saturating_mul(ROW_BYTES) {
        return Err(StoreError::Task(
            "fragment-index blob row count mismatch".to_owned(),
        ));
    }
    let mut rows = Vec::with_capacity(header.rows);
    for chunk in packed.chunks_exact(ROW_BYTES) {
        let class = class_from_code(chunk[20]).ok_or_else(|| {
            StoreError::Task(format!("fragment-index blob has cut class {}", chunk[20]))
        })?;
        rows.push(IndexRow {
            dts: u64::from_le_bytes(chunk[0..8].try_into().expect("eight bytes")),
            duration: u64::from(u32::from_le_bytes(
                chunk[8..12].try_into().expect("four bytes"),
            )),
            bytes: u32::from_le_bytes(chunk[12..16].try_into().expect("four bytes")),
            video_bytes: u32::from_le_bytes(chunk[16..20].try_into().expect("four bytes")),
            class,
        });
    }
    // The cluster artifact is named by source *content* and pipeline content.
    // Legacy SourceIdentity contains scanner mtime and a non-cryptographic argv
    // fingerprint, so serializing it would make duplicate files produce
    // different bytes for the same cache key. Consumers that bridge this v2
    // artifact into the file-keyed v1 store replace this neutral identity with
    // that file's current local identity.
    let canonical_source = SourceIdentity::new(0, 0, header.pipeline_sha256.clone());
    let mut index =
        FragmentIndex::new(header.timescale, rows, header.init_sha256, canonical_source);
    index.promotion = header.promotion;
    index.parameter_sets_constant = header.parameter_sets_constant;
    index.dv_refit_origin = header.dv_refit_origin;
    if header
        .dv_refit_proofs
        .as_ref()
        .is_some_and(|proofs| proofs.len() != index.rows.len())
    {
        return Err(StoreError::Task("fitted proof row count mismatch".into()));
    }
    index.dv_refit_proofs = header.dv_refit_proofs;
    Ok(index)
}

/// Version of the canonical index-planning observations, not an engine version.
/// Bump when timing, selected-track, conversion, init or row semantics change.
pub const FRAGMENT_INDEX_CONTRACT: u32 = 1;
const V3_BLOB_MAGIC: &[u8; 8] = b"PLRXIDX3";
const V3_BLOB_FORMAT_VERSION: u16 = 3;
const V3_ROW_BYTES: usize = 28;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentIndexOutputIdentity {
    pub contract_version: u32,
    /// Digest of planning observations; this is not a media-sample digest.
    pub keyframe_table_sha256: String,
}

/// Execution provenance never participates in the v3 artifact key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentIndexEngineProvenance {
    pub engine_sha256: String,
    pub pipeline_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragmentIndexProvenance {
    LegacyV2 {
        pipeline_sha256: String,
    },
    OutputV3 {
        identity: FragmentIndexOutputIdentity,
        engine: FragmentIndexEngineProvenance,
    },
}

/// New consumers must inspect provenance and explicitly bind the current
/// attested source before bridging into a file-keyed local index store.
/// V2 retains its historical neutral pipeline fingerprint; V3 has no legacy
/// argv fingerprint and leaves that field empty, never filled with a contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedFragmentIndex {
    pub index: FragmentIndex,
    pub provenance: FragmentIndexProvenance,
}

/// A dual reader must supply the identity expected for each permitted format.
/// Omitting one rejects that format; a v2 pipeline never grants v3 adoption.
pub struct FragmentIndexBlobExpectations<'a> {
    pub source_sha256: &'a str,
    pub legacy_pipeline_sha256: Option<&'a str>,
    pub output_identity: Option<&'a FragmentIndexOutputIdentity>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V3BlobHeader {
    segplan_version: u32,
    timescale: u32,
    init_sha256: String,
    init_contract_sha256: String,
    promotion: PromotionInputs,
    parameter_sets_constant: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dv_refit_origin: Option<crate::segplan::DvRefitOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dv_refit_proofs: Option<Vec<crate::segplan::refit_proof::FittedFragmentProof>>,
    source_sha256: String,
    output_identity: FragmentIndexOutputIdentity,
    engine: FragmentIndexEngineProvenance,
    rows: usize,
}

fn v3_error(message: &str) -> StoreError {
    StoreError::Task(format!("v3 fragment-index {message}"))
}

fn v3_portable_promotion(
    index: &FragmentIndex,
    source_sha256: &str,
) -> Result<PromotionInputs, StoreError> {
    let mut promotion = index.promotion.clone();
    if let Some(proof) = promotion.hevc_configuration.as_mut() {
        // Exactly the v2 writer's content-binding rule: no inferred proof and
        // no case-insensitive substitute for the supplied attestation.
        if proof.source_sha256.as_deref() != Some(source_sha256) {
            return Err(v3_error("HEVC proof lacks full source attestation"));
        }
        proof.source_object_version.clear();
        proof.source_node_id.clear();
        proof.source_sha256 = Some(source_sha256.to_ascii_lowercase());
    }
    Ok(promotion)
}

// Stable typed encoding of promotion's complete serialized shape, including
// DolbyVisionRecord's private retained raw bytes. Object keys are explicitly
// sorted; no serde_json map iteration order or JSON whitespace is hashed.
// Every node has a type tag, integer width, and length-delimited children.
fn hash_v3_promotion_value(
    digest: &mut Sha256,
    value: &serde_json::Value,
) -> Result<(), StoreError> {
    use serde_json::Value;
    match value {
        Value::Null => digest.update([0]),
        Value::Bool(value) => digest.update([1, u8::from(*value)]),
        Value::Number(value) => {
            if let Some(value) = value.as_u64() {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            } else if let Some(value) = value.as_i64() {
                digest.update([3]);
                digest.update(value.to_be_bytes());
            } else {
                return Err(v3_error("promotion contains a non-integer number"));
            }
        }
        Value::String(value) => {
            digest.update([4]);
            update_field(digest, value.as_bytes());
        }
        Value::Array(values) => {
            digest.update([5]);
            digest.update((values.len() as u64).to_be_bytes());
            for value in values {
                hash_v3_promotion_value(digest, value)?;
            }
        }
        Value::Object(values) => {
            digest.update([6]);
            digest.update((values.len() as u64).to_be_bytes());
            let mut fields: Vec<_> = values.iter().collect();
            fields.sort_unstable_by_key(|(left, _)| *left);
            for (key, value) in fields {
                update_field(digest, key.as_bytes());
                hash_v3_promotion_value(digest, value)?;
            }
        }
    }
    Ok(())
}

/// Canonical planning identity of a complete index. All row fields are hashed
/// in order with full-width duration, timescale, semantic init and portable promotion.
/// Worker-local SourceIdentity and engine provenance are deliberately absent.
/// The caller supplies the freshly observed `copy_video_init_contract_sha256`;
/// raw init bytes remain provenance and do not identify the output contract.
/// This measures planning equivalence, not cryptographic sample equivalence.
pub fn cluster_fragment_index_output_identity(
    index: &FragmentIndex,
    source_sha256: &str,
    init_contract_sha256: &str,
) -> Result<FragmentIndexOutputIdentity, StoreError> {
    if index.version != SEGPLAN_VERSION
        || index.timescale == 0
        || index.rows.is_empty()
        || !is_sha256(&index.init_sha256)
        || !is_sha256(source_sha256)
        || !is_sha256(init_contract_sha256)
    {
        return Err(v3_error("has invalid planning observations"));
    }
    let promotion = v3_portable_promotion(index, source_sha256)?;
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/planning-table/v3\0");
    digest.update(FRAGMENT_INDEX_CONTRACT.to_be_bytes());
    digest.update(index.version.to_be_bytes());
    digest.update(index.timescale.to_be_bytes());
    update_field(
        &mut digest,
        init_contract_sha256.to_ascii_lowercase().as_bytes(),
    );
    digest.update([u8::from(index.parameter_sets_constant)]);
    if let Some(origin) = index.dv_refit_origin {
        digest.update([1]);
        digest.update(origin.first_dts.to_be_bytes());
        digest.update(origin.first_pts.to_be_bytes());
        digest.update(origin.time_base_num.to_be_bytes());
        digest.update(origin.time_base_den.to_be_bytes());
    }
    if let Some(proofs) = &index.dv_refit_proofs {
        if proofs.len() != index.rows.len() {
            return Err(v3_error("fitted proof row count mismatch"));
        }
        digest.update(b"plurx/fitted-copy/ordered-coded-cadence/v3\0");
        digest.update((proofs.len() as u64).to_be_bytes());
        for proof in proofs {
            digest.update(proof.as_bytes());
        }
    }
    let value = serde_json::to_value(&promotion)
        .map_err(|error| StoreError::Task(format!("encoding v3 promotion: {error}")))?;
    hash_v3_promotion_value(&mut digest, &value)?;
    digest.update((index.rows.len() as u64).to_be_bytes());
    for row in &index.rows {
        digest.update(row.dts.to_be_bytes());
        digest.update(row.duration.to_be_bytes());
        digest.update(row.bytes.to_be_bytes());
        digest.update(row.video_bytes.to_be_bytes());
        digest.update([class_code(row.class)]);
    }
    Ok(FragmentIndexOutputIdentity {
        contract_version: FRAGMENT_INDEX_CONTRACT,
        keyframe_table_sha256: hex::encode(digest.finalize()),
    })
}

/// A v3 key needs origin catalog identity and full source attestation. Opaque
/// per-node object versions, size/mtime and engine provenance are not keys.
/// The caller must establish complete source attestation before publication.
pub fn cluster_fragment_index_v3_key(
    catalog_namespace: &str,
    file_id: i64,
    source_sha256: &str,
    output: &FragmentIndexOutputIdentity,
) -> Option<String> {
    if catalog_namespace.trim().is_empty()
        || catalog_namespace.len() > 256
        || file_id <= 0
        || !is_sha256(source_sha256)
        || output.contract_version == 0
        || !is_sha256(&output.keyframe_table_sha256)
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/cache-key/v3\0");
    digest.update(V3_BLOB_FORMAT_VERSION.to_be_bytes());
    update_field(&mut digest, catalog_namespace.as_bytes());
    digest.update(file_id.to_be_bytes());
    update_field(&mut digest, source_sha256.to_ascii_lowercase().as_bytes());
    digest.update(output.contract_version.to_be_bytes());
    update_field(
        &mut digest,
        output.keyframe_table_sha256.to_ascii_lowercase().as_bytes(),
    );
    Some(hex::encode(digest.finalize()))
}

/// Additive writer: legacy v2 functions and bytes retain their exact meaning.
pub fn encode_cluster_fragment_index_blob_v3(
    index: &FragmentIndex,
    source_sha256: &str,
    init_contract_sha256: &str,
    engine: &FragmentIndexEngineProvenance,
) -> Result<Vec<u8>, StoreError> {
    if !is_sha256(&engine.engine_sha256) || !is_sha256(&engine.pipeline_sha256) {
        return Err(v3_error("has invalid engine provenance"));
    }
    let output_identity =
        cluster_fragment_index_output_identity(index, source_sha256, init_contract_sha256)?;
    let header = V3BlobHeader {
        segplan_version: index.version,
        timescale: index.timescale,
        init_sha256: index.init_sha256.to_ascii_lowercase(),
        init_contract_sha256: init_contract_sha256.to_ascii_lowercase(),
        promotion: v3_portable_promotion(index, source_sha256)?,
        parameter_sets_constant: index.parameter_sets_constant,
        dv_refit_origin: index.dv_refit_origin,
        dv_refit_proofs: index.dv_refit_proofs.clone(),
        source_sha256: source_sha256.to_ascii_lowercase(),
        output_identity,
        engine: FragmentIndexEngineProvenance {
            engine_sha256: engine.engine_sha256.to_ascii_lowercase(),
            pipeline_sha256: engine.pipeline_sha256.to_ascii_lowercase(),
        },
        rows: index.rows.len(),
    };
    let header = serde_json::to_vec(&header)
        .map_err(|error| StoreError::Task(format!("encoding v3 index header: {error}")))?;
    let capacity = index
        .rows
        .len()
        .checked_mul(V3_ROW_BYTES)
        .and_then(|bytes| bytes.checked_add(header.len()))
        .and_then(|bytes| bytes.checked_add(14))
        .filter(|bytes| *bytes <= MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES)
        .ok_or_else(|| v3_error("blob exceeds hard limit"))?;
    let mut blob = Vec::with_capacity(capacity);
    blob.extend_from_slice(V3_BLOB_MAGIC);
    blob.extend_from_slice(&V3_BLOB_FORMAT_VERSION.to_be_bytes());
    blob.extend_from_slice(
        &u32::try_from(header.len())
            .map_err(|_| v3_error("header is too large"))?
            .to_be_bytes(),
    );
    blob.extend_from_slice(&header);
    for row in &index.rows {
        blob.extend_from_slice(&row.dts.to_le_bytes());
        blob.extend_from_slice(&row.duration.to_le_bytes());
        blob.extend_from_slice(&row.bytes.to_le_bytes());
        blob.extend_from_slice(&row.video_bytes.to_le_bytes());
        blob.push(class_code(row.class));
        blob.extend_from_slice(&[0, 0, 0]);
    }
    Ok(blob)
}

/// Decode only v3, requiring independently expected source/output identities.
/// Engine provenance is returned, never required to equal the local engine.
pub fn decode_cluster_fragment_index_blob_v3(
    blob: &[u8],
    expected_source_sha256: &str,
    expected_output: &FragmentIndexOutputIdentity,
) -> Result<DecodedFragmentIndex, StoreError> {
    if blob.len() < 14 || blob.len() > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES {
        return Err(v3_error("has invalid blob size"));
    }
    if &blob[..8] != V3_BLOB_MAGIC
        || u16::from_be_bytes(blob[8..10].try_into().expect("two bytes")) != V3_BLOB_FORMAT_VERSION
    {
        return Err(v3_error("has unsupported magic/version"));
    }
    let header_len = u32::from_be_bytes(blob[10..14].try_into().expect("four bytes")) as usize;
    let rows_offset = 14_usize
        .checked_add(header_len)
        .filter(|offset| *offset <= blob.len())
        .ok_or_else(|| v3_error("has truncated header"))?;
    let header: V3BlobHeader = serde_json::from_slice(&blob[14..rows_offset])
        .map_err(|error| StoreError::Task(format!("decoding v3 index header: {error}")))?;
    // In particular, reject unrecognized nested promotion fields instead of
    // deserializing them away under an unchanged planning contract version.
    let canonical_header = serde_json::to_vec(&header)
        .map_err(|error| StoreError::Task(format!("validating v3 index header: {error}")))?;
    if canonical_header != blob[14..rows_offset] {
        return Err(v3_error(
            "header is noncanonical or contains unknown fields",
        ));
    }
    if header.segplan_version != SEGPLAN_VERSION
        || header.timescale == 0
        || header.rows == 0
        || !is_sha256(&header.init_sha256)
        || !is_sha256(&header.init_contract_sha256)
        || !is_sha256(expected_source_sha256)
        || !is_sha256(&header.source_sha256)
        || !header
            .source_sha256
            .eq_ignore_ascii_case(expected_source_sha256)
        || header.output_identity.contract_version != FRAGMENT_INDEX_CONTRACT
        || header.output_identity.contract_version != expected_output.contract_version
        || !is_sha256(&header.output_identity.keyframe_table_sha256)
        || !header
            .output_identity
            .keyframe_table_sha256
            .eq_ignore_ascii_case(&expected_output.keyframe_table_sha256)
        || !is_sha256(&header.engine.engine_sha256)
        || !is_sha256(&header.engine.pipeline_sha256)
    {
        return Err(v3_error("identity mismatch"));
    }
    if let Some(proof) = &header.promotion.hevc_configuration {
        if !proof.source_object_version.is_empty()
            || !proof.source_node_id.is_empty()
            || proof.source_sha256.as_deref() != Some(header.source_sha256.as_str())
        {
            return Err(v3_error("contains unbound or worker-local HEVC proof"));
        }
    }
    let packed = &blob[rows_offset..];
    if header.rows.checked_mul(V3_ROW_BYTES) != Some(packed.len()) {
        return Err(v3_error("row count mismatch"));
    }
    let mut rows = Vec::with_capacity(header.rows);
    for chunk in packed.chunks_exact(V3_ROW_BYTES) {
        let class = class_from_code(chunk[24]).ok_or_else(|| v3_error("has invalid cut class"))?;
        if chunk[25..28] != [0, 0, 0] {
            return Err(v3_error("has nonzero reserved row bytes"));
        }
        rows.push(IndexRow {
            dts: u64::from_le_bytes(chunk[0..8].try_into().expect("eight bytes")),
            duration: u64::from_le_bytes(chunk[8..16].try_into().expect("eight bytes")),
            bytes: u32::from_le_bytes(chunk[16..20].try_into().expect("four bytes")),
            video_bytes: u32::from_le_bytes(chunk[20..24].try_into().expect("four bytes")),
            class,
        });
    }
    let mut index = FragmentIndex::new(
        header.timescale,
        rows,
        header.init_sha256,
        SourceIdentity::new(0, 0, ""),
    );
    index.promotion = header.promotion;
    index.parameter_sets_constant = header.parameter_sets_constant;
    index.dv_refit_origin = header.dv_refit_origin;
    if header
        .dv_refit_proofs
        .as_ref()
        .is_some_and(|proofs| proofs.len() != index.rows.len())
    {
        return Err(StoreError::Task("fitted proof row count mismatch".into()));
    }
    index.dv_refit_proofs = header.dv_refit_proofs;
    let observed = cluster_fragment_index_output_identity(
        &index,
        &header.source_sha256,
        &header.init_contract_sha256,
    )?;
    if observed != header.output_identity {
        return Err(v3_error("planning-table digest mismatch"));
    }
    Ok(DecodedFragmentIndex {
        index,
        provenance: FragmentIndexProvenance::OutputV3 {
            identity: header.output_identity,
            engine: header.engine,
        },
    })
}

/// Explicit dual reader. The historical v2 decoder still rejects v3 blobs.
pub fn decode_cluster_fragment_index_blob_with_provenance(
    blob: &[u8],
    expected: &FragmentIndexBlobExpectations<'_>,
) -> Result<DecodedFragmentIndex, StoreError> {
    match blob.get(..8) {
        Some(magic) if magic == BLOB_MAGIC => {
            let pipeline = expected
                .legacy_pipeline_sha256
                .ok_or_else(|| v3_error("v2 provenance expectation missing"))?;
            let index = decode_cluster_fragment_index_blob(blob, expected.source_sha256, pipeline)?;
            let provenance = FragmentIndexProvenance::LegacyV2 {
                pipeline_sha256: index.source.argv_fingerprint.clone(),
            };
            Ok(DecodedFragmentIndex { index, provenance })
        }
        Some(magic) if magic == V3_BLOB_MAGIC => {
            let output = expected
                .output_identity
                .ok_or_else(|| v3_error("output identity expectation missing"))?;
            decode_cluster_fragment_index_blob_v3(blob, expected.source_sha256, output)
        }
        _ => Err(v3_error("has unsupported blob magic")),
    }
}

pub fn cluster_fragment_index_blob_sha256(blob: &[u8]) -> String {
    hex::encode(Sha256::digest(blob))
}

fn class_code(class: CutClass) -> u8 {
    match class {
        CutClass::CleanIdr => 1,
        CutClass::CleanCra => 2,
        CutClass::Dirty => 3,
        CutClass::Unparseable => 4,
    }
}

fn class_from_code(code: u8) -> Option<CutClass> {
    match code {
        1 => Some(CutClass::CleanIdr),
        2 => Some(CutClass::CleanCra),
        3 => Some(CutClass::Dirty),
        4 => Some(CutClass::Unparseable),
        _ => None,
    }
}

/// Unconditional: empty generation keys have the same retention semantics.
/// Every analysis_requests rebuild must recreate this lookup index.
pub(crate) const ANALYSIS_RESULT_TARGET_FORCE_SCHEMA: &str = "CREATE INDEX IF NOT EXISTS analysis_requests_result_target_force ON analysis_requests(result_cache_key, target_node_id, force_rebuild);";

/// Production retention statement, shared by both backends and plan tests.
#[doc(hidden)]
pub const FRAGMENT_PRUNE_CANDIDATES: &str = "SELECT j.cache_key AS cache_key
                   FROM cluster_fragment_index_jobs j
                  WHERE (j.state IN ('ready', 'cancelled') OR (
                    j.state = 'failed' AND (
                      NOT EXISTS (SELECT 1 FROM files current_file
                        WHERE current_file.id = j.file_id
                          AND current_file.size = j.source_size
                          AND current_file.mtime = j.source_mtime)
                      OR EXISTS (SELECT 1 FROM analysis_requests request
                        WHERE request.result_cache_key = j.cache_key
                          AND request.target_node_id = j.target_node_id
                          AND request.force_rebuild = 1))))
                    AND j.updated_at_ms < $1
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_jobs active_job
                       WHERE active_job.cache_key = j.cache_key
                         AND (active_job.state IN ('queued', 'running')
                           OR active_job.updated_at_ms >= $1))
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests active_request
                       WHERE active_request.result_cache_key = j.cache_key
                         AND active_request.state IN ('queued', 'running', 'submitted'))
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_locations l
                       WHERE l.cache_key = j.cache_key)
                  GROUP BY j.cache_key
                  ORDER BY MIN(j.updated_at_ms), j.cache_key LIMIT $2";

/// Production retention statement, shared by both backends and plan tests.
#[doc(hidden)]
pub const FRAGMENT_PRUNE_TERMINAL_JOBS: &str = "DELETE FROM cluster_fragment_index_jobs
              WHERE (cache_key, target_node_id) IN (
                SELECT terminal_job.cache_key, terminal_job.target_node_id
                  FROM cluster_fragment_index_jobs terminal_job
                 WHERE (terminal_job.state IN ('ready', 'cancelled') OR (
                   terminal_job.state = 'failed' AND (
                     NOT EXISTS (SELECT 1 FROM files current_file
                       WHERE current_file.id = terminal_job.file_id
                         AND current_file.size = terminal_job.source_size
                         AND current_file.mtime = terminal_job.source_mtime)
                     OR EXISTS (SELECT 1 FROM analysis_requests request
                       WHERE request.result_cache_key = terminal_job.cache_key
                         AND request.target_node_id = terminal_job.target_node_id
                         AND request.force_rebuild = 1))))
                   AND terminal_job.updated_at_ms < $1
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                     WHERE artifact.cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads head
                     WHERE head.generation_cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                     WHERE location.cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs active_job
                     WHERE active_job.cache_key = terminal_job.cache_key
                       AND (active_job.state IN ('queued', 'running')
                         OR active_job.updated_at_ms >= $1))
                   AND NOT EXISTS (SELECT 1 FROM analysis_requests active_request
                     WHERE active_request.result_cache_key = terminal_job.cache_key
                       AND active_request.state IN ('queued', 'running', 'submitted'))
                 ORDER BY terminal_job.updated_at_ms, terminal_job.cache_key,
                          terminal_job.target_node_id
                 LIMIT $2
              )";

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    #[test]
    fn cache_key_is_domain_separated_and_exact() {
        let a =
            cluster_fragment_index_key(1, 100, 10, &digest('a'), &digest('b')).expect("valid key");
        let b =
            cluster_fragment_index_key(1, 100, 10, &digest('b'), &digest('a')).expect("valid key");
        let other_file = cluster_fragment_index_key(2, 100, 10, &digest('a'), &digest('b'))
            .expect("other file key");
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(a, other_file);
        assert!(cluster_fragment_index_key(1, 100, 10, "not-a-digest", &digest('b')).is_none());
    }

    #[test]
    fn generation_keys_are_immutable_successors_of_one_logical_key() {
        let source = digest('a');
        let pipeline = digest('b');
        let logical =
            cluster_fragment_index_key(1, 100, 10, &source, &pipeline).expect("logical key");
        let first =
            cluster_fragment_index_generation_key(1, 100, 10, &source, &pipeline, "generation-1")
                .expect("first generation");
        let second =
            cluster_fragment_index_generation_key(1, 100, 10, &source, &pipeline, "generation-2")
                .expect("second generation");
        assert_ne!(logical, first);
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn analysis_retry_settings_have_safe_defaults_and_hard_maximums() {
        assert_eq!(
            bounded_analysis_max_attempts(None),
            DEFAULT_ANALYSIS_MAX_ATTEMPTS
        );
        assert_eq!(
            bounded_analysis_max_attempts(Some("999")),
            MAX_ANALYSIS_MAX_ATTEMPTS
        );
        assert_eq!(
            bounded_analysis_lease_secs(Some("0")),
            15,
            "a lease shorter than the renewal floor is refused"
        );
        assert_eq!(
            bounded_analysis_lease_secs(Some("99999")),
            MAX_ANALYSIS_LEASE_SECS
        );
        assert_eq!(
            bounded_analysis_backoff_base_secs(Some("99999")),
            MAX_ANALYSIS_BACKOFF_BASE_SECS
        );
        assert_eq!(
            bounded_analysis_backoff_max_secs(Some("99999")),
            MAX_ANALYSIS_BACKOFF_MAX_SECS
        );
    }

    #[test]
    fn subtitle_window_setting_has_the_documented_default_and_bounds() {
        assert_eq!(
            bounded_subtitle_window_seconds(None),
            DEFAULT_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("not-a-number")),
            DEFAULT_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("0")),
            MIN_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("99999")),
            MAX_SUBTITLE_WINDOW_SECS
        );
    }

    #[test]
    fn analysis_backoff_is_stable_exponential_and_capped() {
        let first = analysis_backoff_ms("request-a", 1, 5_000, 300_000);
        assert_eq!(first, analysis_backoff_ms("request-a", 1, 5_000, 300_000));
        let second = analysis_backoff_ms("request-a", 2, 5_000, 300_000);
        assert!(second > first);
        assert!(analysis_backoff_ms("request-b", 1, 5_000, 300_000) != first);
        assert_eq!(
            analysis_backoff_ms("request-a", 60, 5_000, 300_000),
            300_000
        );
    }

    #[test]
    fn blob_round_trips_and_rejects_identity_aliases() {
        let mut index = FragmentIndex::new(
            90_000,
            vec![IndexRow {
                dts: 12,
                duration: 3_003,
                bytes: 45,
                video_bytes: 31,
                class: CutClass::CleanIdr,
            }],
            digest('c'),
            SourceIdentity::new(123, 456, "argv"),
        );
        index.parameter_sets_constant = true;
        index.promotion.dolby_vision = Some(
            crate::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1)
                .expect("converted record"),
        );
        let source = digest('a');
        let pipeline = digest('b');
        let blob = encode_cluster_fragment_index_blob(&index, &source, &pipeline).expect("encode");
        let decoded =
            decode_cluster_fragment_index_blob(&blob, &source, &pipeline).expect("decode");
        assert_eq!(decoded.rows, index.rows);
        assert_eq!(decoded.init_sha256, index.init_sha256);
        assert_eq!(decoded.promotion, index.promotion);
        assert_eq!(
            decoded.parameter_sets_constant,
            index.parameter_sets_constant
        );
        assert_eq!(decoded.source, SourceIdentity::new(0, 0, pipeline.clone()));
        assert!(decode_cluster_fragment_index_blob(&blob, &digest('d'), &pipeline).is_err());
        assert!(decode_cluster_fragment_index_blob(&blob, &source, &digest('d')).is_err());
        assert!(
            decode_cluster_fragment_index_blob(&blob[..blob.len() - 1], &source, &pipeline)
                .is_err(),
            "a truncated artifact must not supply substitute fragment metadata"
        );
    }

    #[test]
    fn hevc_portable_proof_is_content_bound_and_node_independent() {
        let source = digest('a');
        let pipeline = digest('b');
        let mut index = FragmentIndex::new(
            90_000,
            vec![IndexRow {
                dts: 0,
                duration: 90000,
                bytes: 100,
                video_bytes: 90,
                class: CutClass::CleanIdr,
            }],
            digest('c'),
            SourceIdentity::new(100, 1, "argv"),
        );
        index.promotion.hevc_configuration = Some(crate::hevc_configuration::Proof {
            revision: crate::hevc_configuration::REVISION,
            source_node_id: "one".into(),
            source_object_version: "object-one".into(),
            source_sha256: Some(source.clone()),
            stream_index: 0,
            slices: 24,
            refusal: None,
        });
        let first = encode_cluster_fragment_index_blob(&index, &source, &pipeline)
            .expect("HEVC regression fixture");
        let proof = index
            .promotion
            .hevc_configuration
            .as_mut()
            .expect("HEVC regression fixture");
        proof.source_node_id = "two".into();
        proof.source_object_version = "object-two".into();
        assert_eq!(
            first,
            encode_cluster_fragment_index_blob(&index, &source, &pipeline)
                .expect("HEVC regression fixture")
        );
        let mut decoded = decode_cluster_fragment_index_blob(&first, &source, &pipeline)
            .expect("HEVC regression fixture");
        let proof = decoded
            .promotion
            .hevc_configuration
            .as_mut()
            .expect("HEVC regression fixture");
        assert!(!proof.permits_on_node("object-two", "two"));
        assert!(proof.bind_equivalent_source(&source, "two", "object-two"));
        assert!(proof.permits_on_node("object-two", "two"));
        index
            .promotion
            .hevc_configuration
            .as_mut()
            .expect("HEVC regression fixture")
            .source_sha256 = None;
        assert!(encode_cluster_fragment_index_blob(&index, &source, &pipeline).is_err());
    }

    #[test]
    fn blob_bytes_ignore_file_specific_legacy_identity() {
        let row = IndexRow {
            dts: 12,
            duration: 3_003,
            bytes: 45,
            video_bytes: 31,
            class: CutClass::CleanIdr,
        };
        let first = FragmentIndex::new(
            90_000,
            vec![row.clone()],
            digest('c'),
            SourceIdentity::new(123, 456, "argv-a"),
        );
        let second = FragmentIndex::new(
            90_000,
            vec![row],
            digest('c'),
            SourceIdentity::new(123, 999, "argv-b"),
        );
        let source = digest('a');
        let pipeline = digest('b');
        assert_eq!(
            encode_cluster_fragment_index_blob(&first, &source, &pipeline).expect("first"),
            encode_cluster_fragment_index_blob(&second, &source, &pipeline).expect("second")
        );
    }

    fn codec_index() -> FragmentIndex {
        FragmentIndex::new(
            90_000,
            vec![IndexRow {
                dts: 12,
                duration: 3_003,
                bytes: 45,
                video_bytes: 31,
                class: CutClass::CleanIdr,
            }],
            digest('c'),
            SourceIdentity::new(123, 456, "argv"),
        )
    }

    fn codec_engine() -> FragmentIndexEngineProvenance {
        FragmentIndexEngineProvenance {
            engine_sha256: digest('d'),
            pipeline_sha256: digest('b'),
        }
    }

    #[test]
    fn v2_blob_remains_byte_compatible_with_legacy_fixture() {
        let blob = encode_cluster_fragment_index_blob(&codec_index(), &digest('a'), &digest('b'))
            .expect("legacy encode");
        // Fixed PLRXIDX2/segplan-3 fixture, independently packed with a
        // 24-byte row and the original ordered header. V3 never rewrites it.
        assert_eq!(
            cluster_fragment_index_blob_sha256(&blob),
            "50696f0867150955850aabaa4d1cd1305d84a3d571ce8d9da9f0c051f93d9d6c"
        );
        let decoded = decode_cluster_fragment_index_blob_with_provenance(
            &blob,
            &FragmentIndexBlobExpectations {
                source_sha256: &digest('a'),
                legacy_pipeline_sha256: Some(&digest('b')),
                output_identity: None,
            },
        )
        .expect("explicit legacy reader");
        assert_eq!(decoded.index.source, SourceIdentity::new(0, 0, digest('b')));
        assert_eq!(
            decoded.provenance,
            FragmentIndexProvenance::LegacyV2 {
                pipeline_sha256: digest('b'),
            }
        );
    }

    #[test]
    fn fitted_clock_and_proofs_roundtrip_both_blob_formats_and_bind_output_identity() {
        use crate::segplan::{refit_proof::FittedFragmentProof, DvRefitOrigin};
        let source = digest('a');
        let semantic = digest('9');
        let engine = codec_engine();
        let mut index = codec_index();
        let mut second = index.rows[0].clone();
        second.dts += second.duration;
        second.class = CutClass::Dirty;
        index.rows.push(second);
        index.dv_refit_origin = Some(DvRefitOrigin {
            first_dts: -83,
            first_pts: 0,
            time_base_num: 1,
            time_base_den: 1000,
        });
        let proof = |digit| {
            serde_json::from_value::<FittedFragmentProof>(serde_json::json!(digest(digit)))
                .expect("typed SHA256 proof")
        };
        index.dv_refit_proofs = Some(vec![proof('e'), proof('f')]);
        let output = cluster_fragment_index_output_identity(&index, &source, &semantic)
            .expect("fitted output identity");

        let legacy = encode_cluster_fragment_index_blob(&index, &source, &engine.pipeline_sha256)
            .expect("v2 preserves optional fitted metadata");
        let current = encode_cluster_fragment_index_blob_v3(&index, &source, &semantic, &engine)
            .expect("v3 preserves optional fitted metadata");
        let decoded = [
            decode_cluster_fragment_index_blob(&legacy, &source, &engine.pipeline_sha256)
                .expect("v2 fitted roundtrip"),
            decode_cluster_fragment_index_blob_v3(&current, &source, &output)
                .expect("v3 fitted roundtrip")
                .index,
        ];
        for restored in decoded {
            assert_eq!(restored.rows, index.rows);
            assert_eq!(restored.dv_refit_origin, index.dv_refit_origin);
            assert_eq!(restored.dv_refit_proofs, index.dv_refit_proofs);
            assert_eq!(
                cluster_fragment_index_output_identity(&restored, &source, &semantic)
                    .expect("restored fitted identity"),
                output
            );
        }

        let mut changed = index.clone();
        changed.dv_refit_proofs.as_mut().expect("fitted proofs")[0] = proof('8');
        assert_ne!(
            cluster_fragment_index_output_identity(&changed, &source, &semantic)
                .expect("changed proof identity"),
            output
        );
        let changed_blob =
            encode_cluster_fragment_index_blob_v3(&changed, &source, &semantic, &engine)
                .expect("changed fitted metadata");
        assert!(
            decode_cluster_fragment_index_blob_v3(&changed_blob, &source, &output).is_err(),
            "the prior output identity cannot authorize different fitted proofs"
        );
    }

    #[test]
    fn v3_round_trips_full_width_rows_without_legacy_identity_aliasing() {
        let source = digest('a');
        let mut index = codec_index();
        index.rows[0].duration = u64::from(u32::MAX) + 123;
        let output = cluster_fragment_index_output_identity(&index, &source, &digest('9'))
            .expect("identity");
        let engine = codec_engine();
        let blob = encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &engine)
            .expect("v3 encode");
        let decoded = decode_cluster_fragment_index_blob_with_provenance(
            &blob,
            &FragmentIndexBlobExpectations {
                source_sha256: &source,
                legacy_pipeline_sha256: Some(&engine.pipeline_sha256),
                output_identity: Some(&output),
            },
        )
        .expect("dual reader");
        assert_eq!(decoded.index.rows, index.rows);
        assert_eq!(decoded.index.source, SourceIdentity::new(0, 0, ""));
        assert_eq!(
            decoded.provenance,
            FragmentIndexProvenance::OutputV3 {
                identity: output,
                engine: engine.clone(),
            }
        );
        assert!(
            decode_cluster_fragment_index_blob(&blob, &source, &engine.pipeline_sha256).is_err()
        );
        assert!(
            decode_cluster_fragment_index_blob_with_provenance(
                &blob,
                &FragmentIndexBlobExpectations {
                    source_sha256: &source,
                    legacy_pipeline_sha256: Some(&engine.pipeline_sha256),
                    output_identity: None,
                },
            )
            .is_err(),
            "legacy pipeline identity cannot authorize v3 adoption"
        );
        let legacy = encode_cluster_fragment_index_blob(&index, &source, &engine.pipeline_sha256)
            .expect("legacy encoding retains legacy semantics");
        assert_eq!(
            decode_cluster_fragment_index_blob(&legacy, &source, &engine.pipeline_sha256)
                .expect("legacy decoding")
                .rows[0]
                .duration,
            u64::from(u32::MAX),
        );
    }

    #[test]
    fn v3_planning_identity_covers_every_row_field_and_init_contract() {
        let source = digest('a');
        let index = codec_index();
        let baseline = cluster_fragment_index_output_identity(&index, &source, &digest('9'))
            .expect("baseline");
        let mut changed: Vec<FragmentIndex> = vec![index.clone(); 12];
        changed[0].rows[0].dts += 1;
        changed[1].rows[0].duration += 1;
        changed[2].rows[0].bytes += 1;
        changed[3].rows[0].video_bytes += 1;
        changed[4].rows[0].class = CutClass::CleanCra;
        changed[5].timescale += 1;
        changed[6].init_sha256 = digest('e');
        changed[7].parameter_sets_constant = false;
        changed[8].promotion.parameter_sets = vec![vec![32, 1]];
        changed[9].promotion.dvh1_parameter_sets_malformed = true;
        changed[10].promotion.indexed_hdr10_metadata = true;
        changed[11].promotion.dolby_vision = Some(
            crate::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("DV record"),
        );
        for (field, index) in changed.iter().enumerate() {
            let output = cluster_fragment_index_output_identity(index, &source, &digest('9'))
                .expect("changed");
            if field == 6 {
                assert_eq!(
                    output, baseline,
                    "raw muxer init changes are provenance only"
                );
            } else {
                assert_ne!(output, baseline, "planning field {field} must participate");
            }
        }
        assert_ne!(
            cluster_fragment_index_output_identity(&index, &source, &digest('8'))
                .expect("semantic change"),
            baseline,
            "semantic decoder/timing init changes must rekey"
        );
        assert!(
            cluster_fragment_index_output_identity(&index, &source, "missing semantic digest")
                .is_err()
        );
        let mut reordered = index.clone();
        reordered.rows.push(IndexRow {
            dts: 99,
            ..index.rows[0].clone()
        });
        let ordered = cluster_fragment_index_output_identity(&reordered, &source, &digest('9'))
            .expect("ordered");
        reordered.rows.reverse();
        assert_ne!(
            cluster_fragment_index_output_identity(&reordered, &source, &digest('9'))
                .expect("reordered"),
            ordered
        );
        let mut local = index;
        local.source = SourceIdentity::new(999, 888, "another-worker");
        assert_eq!(
            cluster_fragment_index_output_identity(&local, &source, &digest('9')).expect("local"),
            baseline
        );
    }

    #[test]
    fn v3_raw_init_changes_preserve_semantic_identity_and_raw_provenance() {
        let source = digest('a');
        let semantic = digest('9');
        let mut index = codec_index();
        let identity =
            cluster_fragment_index_output_identity(&index, &source, &semantic).expect("identity");
        let original =
            encode_cluster_fragment_index_blob_v3(&index, &source, &semantic, &codec_engine())
                .expect("original");
        index.init_sha256 = digest('e');
        let changed =
            encode_cluster_fragment_index_blob_v3(&index, &source, &semantic, &codec_engine())
                .expect("changed raw init");
        assert_ne!(
            cluster_fragment_index_blob_sha256(&original),
            cluster_fragment_index_blob_sha256(&changed)
        );
        let decoded = decode_cluster_fragment_index_blob_v3(&changed, &source, &identity)
            .expect("same semantic output");
        assert_eq!(decoded.index.init_sha256, index.init_sha256);
        assert_eq!(
            cluster_fragment_index_output_identity(&decoded.index, &source, &semantic)
                .expect("decoded identity"),
            identity
        );
    }

    #[test]
    fn v3_key_is_engine_independent_and_contract_source_catalog_bound() {
        let index = codec_index();
        let source = digest('a');
        let output = cluster_fragment_index_output_identity(&index, &source, &digest('9'))
            .expect("identity");
        let key = cluster_fragment_index_v3_key("catalog-one", 1, &source, &output).expect("key");
        let first =
            encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &codec_engine())
                .expect("first");
        let mut other_engine = codec_engine();
        other_engine.engine_sha256 = digest('f');
        other_engine.pipeline_sha256 = digest('e');
        let second =
            encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &other_engine)
                .expect("second");
        assert_ne!(first, second, "provenance remains in immutable blob bytes");
        let decoded = decode_cluster_fragment_index_blob_v3(&second, &source, &output)
            .expect("another engine can satisfy the same planning contract");
        assert!(
            matches!(decoded.provenance, FragmentIndexProvenance::OutputV3 { engine, .. } if engine == other_engine)
        );
        assert_eq!(
            cluster_fragment_index_v3_key("catalog-one", 1, &source.to_ascii_uppercase(), &output),
            Some(key.clone())
        );
        assert_ne!(
            cluster_fragment_index_v3_key("catalog-two", 1, &source, &output),
            Some(key.clone())
        );
        assert_ne!(
            cluster_fragment_index_v3_key("catalog-one", 2, &source, &output),
            Some(key.clone())
        );
        assert_ne!(
            cluster_fragment_index_v3_key("catalog-one", 1, &digest('e'), &output),
            Some(key.clone())
        );
        let mut successor = output.clone();
        successor.contract_version += 1;
        assert_ne!(
            cluster_fragment_index_v3_key("catalog-one", 1, &source, &successor),
            Some(key)
        );
        assert!(decode_cluster_fragment_index_blob_v3(&first, &source, &successor).is_err());
        assert!(cluster_fragment_index_v3_key("", 1, &source, &output).is_none());
    }

    #[test]
    fn v3_rejects_corrupt_rows_padding_unknown_fields_and_wrong_identities() {
        let index = codec_index();
        let source = digest('a');
        let output = cluster_fragment_index_output_identity(&index, &source, &digest('9'))
            .expect("identity");
        let blob =
            encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &codec_engine())
                .expect("blob");
        let row = blob.len() - V3_ROW_BYTES;
        for offset in [0, 8, 16, 20, 24, 25] {
            let mut corrupt = blob.clone();
            corrupt[row + offset] ^= 1;
            assert!(
                decode_cluster_fragment_index_blob_v3(&corrupt, &source, &output).is_err(),
                "row corruption at {offset}"
            );
        }
        assert!(
            decode_cluster_fragment_index_blob_v3(&blob[..blob.len() - 1], &source, &output)
                .is_err()
        );
        assert!(decode_cluster_fragment_index_blob_v3(&blob, &digest('f'), &output).is_err());
        let mut wrong_table = output.clone();
        wrong_table.keyframe_table_sha256 = digest('f');
        assert!(decode_cluster_fragment_index_blob_v3(&blob, &source, &wrong_table).is_err());
        let header_len = u32::from_be_bytes(blob[10..14].try_into().expect("length")) as usize;
        let mut header: serde_json::Value =
            serde_json::from_slice(&blob[14..14 + header_len]).expect("header");
        for semantic in [None, Some("invalid".to_owned()), Some(digest('8'))] {
            let mut changed = header.clone();
            if let Some(semantic) = semantic {
                changed["init_contract_sha256"] = serde_json::json!(semantic);
            } else {
                changed
                    .as_object_mut()
                    .expect("object")
                    .remove("init_contract_sha256");
            }
            let encoded = serde_json::to_vec(&changed).expect("changed header");
            let mut corrupt = blob[..10].to_vec();
            corrupt.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
            corrupt.extend_from_slice(&encoded);
            corrupt.extend_from_slice(&blob[14 + header_len..]);
            assert!(
                decode_cluster_fragment_index_blob_v3(&corrupt, &source, &output).is_err(),
                "missing, invalid or changed semantic init cannot inherit expected identity"
            );
        }
        header["promotion"]["unknown_future_rule"] = serde_json::json!(true);
        let header = serde_json::to_vec(&header).expect("modified header");
        let mut unknown = blob[..10].to_vec();
        unknown.extend_from_slice(&(header.len() as u32).to_be_bytes());
        unknown.extend_from_slice(&header);
        unknown.extend_from_slice(&blob[14 + header_len..]);
        assert!(decode_cluster_fragment_index_blob_v3(&unknown, &source, &output).is_err());
    }

    #[test]
    fn v3_portable_hevc_proof_never_adopts_local_custody() {
        let mut index = codec_index();
        let source = digest('a');
        index.promotion.hevc_configuration = Some(crate::hevc_configuration::Proof {
            revision: crate::hevc_configuration::REVISION,
            source_node_id: "one".into(),
            source_object_version: "held-one".into(),
            source_sha256: Some(source.clone()),
            stream_index: 0,
            slices: 24,
            refusal: None,
        });
        let output = cluster_fragment_index_output_identity(&index, &source, &digest('9'))
            .expect("identity");
        let first =
            encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &codec_engine())
                .expect("first");
        let proof = index.promotion.hevc_configuration.as_mut().expect("proof");
        proof.source_node_id = "two".into();
        proof.source_object_version = "held-two".into();
        assert_eq!(
            first,
            encode_cluster_fragment_index_blob_v3(&index, &source, &digest('9'), &codec_engine())
                .expect("portable")
        );
        let decoded =
            decode_cluster_fragment_index_blob_v3(&first, &source, &output).expect("decoded");
        assert!(!decoded
            .index
            .promotion
            .hevc_configuration
            .expect("proof")
            .permits_on_node("held-two", "two"));
        index
            .promotion
            .hevc_configuration
            .as_mut()
            .expect("proof")
            .source_sha256 = None;
        assert!(encode_cluster_fragment_index_blob_v3(
            &index,
            &source,
            &digest('9'),
            &codec_engine()
        )
        .is_err());
    }
}
