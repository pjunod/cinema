//! Deterministic import of a prepared legacy SQLite backup into fresh Hiqlite.
//!
//! The coordinator owns the crash boundary: this method may leave a partial
//! `hiqlite.incoming` cluster after an error, and the next attempt removes that
//! directory before starting again. The importer therefore refuses a target
//! with any application rows instead of trying to resume or merge state.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use hiqlite::macros::params;
use hiqlite::{Param, Row};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

use super::hiqlite::{database_error, HiqliteAuthStore};
use super::{keys, MediaStore, SettingsStore, SQLITE_SCHEMA_VERSION};
use crate::cluster::migration::HIQLITE_WAL_USABLE_PAYLOAD_BYTES;
use crate::error::StoreError;
use crate::secrets::SealedSecret;

const MINIMUM_IMPORT_SCHEMA_VERSION: i64 = 14;
/// Serialized bytes one import transaction accumulates to before it is
/// submitted. This — not a row count — is what keeps a transaction inside the
/// WAL, because `files.probe_json` holds whole ffprobe documents and adjacent
/// rows vary by orders of magnitude.
///
/// This ceiling stays at the measured 512 KiB instead of scaling with WAL
/// headroom. Every submission goes through the store's three-second
/// `STORE_TIMEOUT`: a three-voter import of 200 KiB probe rows timed out near
/// 1 MiB on a loaded host and completed near 512 KiB. Raising the WAL protects
/// an indivisible large row; it is not permission to make routine replication
/// eight times larger.
const IMPORT_REPLICATION_CEILING_BYTES: usize = 512 * 1024;
const IMPORT_TXN_BUDGET_BYTES: usize = {
    let wal_fraction = HIQLITE_WAL_USABLE_PAYLOAD_BYTES / 4;
    if wal_fraction < IMPORT_REPLICATION_CEILING_BYTES {
        wal_fraction
    } else {
        IMPORT_REPLICATION_CEILING_BYTES
    }
};
/// Bytes charged to a transaction before any row, covering the Raft entry
/// header and `QueryWrite` framing wrapped around the statements.
const IMPORT_TXN_ENVELOPE_BYTES: usize = 256;
/// Held back from every transaction against encoding drift: [`estimated_row_bytes`]
/// is an upper bound on the `bincode::config::legacy()` encoding `hiqlite-wal`
/// 0.14 writes, but a later version could widen a discriminant, add a field, or
/// change config, and the cost of being wrong is a panicked node rather than a
/// slower import.
const WAL_ENCODING_RESERVE_BYTES: usize = 64 * 1024;
/// A single row estimated above this is refused, because no transaction
/// carrying it fits the WAL: submitting it panics the writer thread and
/// restarts the node into `SelectedBackend::SqliteRecovery` — HTTP-healthy
/// while serving unreplicated SQLite.
///
/// Deliberately just under what the WAL can hold rather than a fraction of it.
/// A row this large has no smaller transaction to travel in, so every byte
/// shaved off here refuses an import the WAL would have accepted.
const IMPORT_MAX_ROW_BYTES: usize =
    HIQLITE_WAL_USABLE_PAYLOAD_BYTES - WAL_ENCODING_RESERVE_BYTES - IMPORT_TXN_ENVELOPE_BYTES;
/// Rows per source read page, and the secondary ceiling on one import `txn`.
///
/// [`IMPORT_TXN_BUDGET_BYTES`] is the bound that keeps a transaction inside the
/// WAL; this row count bounds how much of the source is resident at once.
/// It stays small because a page is read whole before the byte budget can split
/// it: 64 rows of a real library serialized to 3,137,236 bytes, so a large page
/// of those costs memory whether or not the builder then splits them.
const IMPORT_CHUNK_ROWS: i64 = 16;
/// Rows per read-only parity page. Independent of [`IMPORT_CHUNK_ROWS`]: these
/// pages are consistent reads that never enter a Raft transaction, so the WAL
/// payload cap does not apply to them.
const PARITY_PAGE_ROWS: i64 = 64;

/// Ordered evidence that one durable SQLite table exactly matches Hiqlite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteImportTableDigest {
    pub table: String,
    pub row_count: u64,
    pub sha256: String,
}

/// Evidence emitted only after every imported table and derived index passes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteImportReport {
    pub source_schema_version: i64,
    pub backup_sha256: String,
    pub imported_rows: u64,
    pub search_rows: u64,
    pub tables: Vec<SqliteImportTableDigest>,
}

#[derive(Clone, Copy)]
struct TablePlan {
    name: &'static str,
    columns: &'static [&'static str],
    order_by: &'static str,
    minimum_schema: i64,
    import_filter: Option<ImportFilter>,
    /// Columns holding a sealed credential, which import refuses to carry in
    /// the clear.
    ///
    /// Declared per table rather than checked by name at the one call site so
    /// that a future table with a credential column cannot quietly skip the
    /// gate: adding a `TablePlan` does not compile until this is answered.
    /// Empty for every table whose columns are ordinary catalogue data.
    sealed_columns: &'static [&'static str],
    parent_first: bool,
}

#[derive(Clone, Copy)]
enum ImportFilter {
    ExcludeInstanceId,
}

#[derive(Debug, PartialEq, Eq)]
struct OrderedRowsDigest {
    row_count: u64,
    sha256: String,
}

struct OrderedRowsHasher {
    hasher: Sha256,
    row_count: u64,
}

impl OrderedRowsHasher {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"[");
        Self {
            hasher,
            row_count: 0,
        }
    }

    fn push(&mut self, row: &str) -> Result<(), StoreError> {
        if self.row_count > 0 {
            self.hasher.update(b",");
        }
        let encoded = serde_json::to_vec(row)
            .map_err(|error| import_error(format!("serializing table parity row: {error}")))?;
        self.hasher.update(encoded);
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or_else(|| import_error("table parity row count overflow"))?;
        Ok(())
    }

    fn finish(mut self) -> OrderedRowsDigest {
        self.hasher.update(b"]");
        OrderedRowsDigest {
            row_count: self.row_count,
            sha256: hex::encode(self.hasher.finalize()),
        }
    }
}

struct SourceMetadata {
    backup_sha256: String,
    schema_version: i64,
    instance_id: String,
    instance_updated_at: i64,
}

enum SourceChunk {
    Offset(i64),
    ItemIds(Vec<i64>),
}

enum SourceRequest {
    ItemIdentityWatermark {
        reply: oneshot::Sender<Result<Option<i64>, StoreError>>,
    },
    Count {
        table: TablePlan,
        for_import: bool,
        reply: oneshot::Sender<Result<i64, StoreError>>,
    },
    ParentFirstItemIds {
        reply: oneshot::Sender<Result<Vec<i64>, StoreError>>,
    },
    ImportChunk {
        table: TablePlan,
        schema_version: i64,
        chunk: SourceChunk,
        reply: oneshot::Sender<Result<Vec<hiqlite::Params>, StoreError>>,
    },
    Digest {
        table: TablePlan,
        schema_version: i64,
        reply: oneshot::Sender<Result<OrderedRowsDigest, StoreError>>,
    },
    UnsealedCredentialRows {
        table: TablePlan,
        reply: oneshot::Sender<Result<u64, StoreError>>,
    },
    #[cfg(test)]
    Pause {
        duration: std::time::Duration,
        reply: oneshot::Sender<()>,
    },
}

/// One blocking worker owns the immutable SQLite connection for the import.
///
/// Reopening the backup for every [`IMPORT_CHUNK_ROWS`] chunk would avoid
/// blocking Tokio but turn a large catalogue into thousands of connection
/// setups. This actor
/// keeps one read-only connection and sends row data back in bounded chunks.
/// The one deliberate O(items) reply is the parent-first id ordering used to
/// avoid rebuilding and sorting the full item tree for every chunk.
struct SourceReader {
    requests: mpsc::Sender<SourceRequest>,
}

impl SourceReader {
    async fn open(
        backup_path: &Path,
        expected_sha256: &str,
        expected_schema_version: i64,
    ) -> Result<(Self, SourceMetadata), StoreError> {
        let backup_path = backup_path.to_owned();
        let expected_sha256 = expected_sha256.to_owned();
        let (requests, mut receiver) = mpsc::channel::<SourceRequest>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let opened =
                open_validated_source(&backup_path, &expected_sha256, expected_schema_version);
            let (source, metadata) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if ready_tx.send(Ok(metadata)).is_err() {
                return;
            }

            while let Some(request) = receiver.blocking_recv() {
                match request {
                    SourceRequest::ItemIdentityWatermark { reply } => {
                        let _ = reply.send(source_item_identity_watermark(&source));
                    }
                    SourceRequest::Count {
                        table,
                        for_import,
                        reply,
                    } => {
                        let _ = reply.send(source_count(&source, table, for_import));
                    }
                    SourceRequest::ParentFirstItemIds { reply } => {
                        let _ = reply.send(parent_first_item_ids(&source));
                    }
                    SourceRequest::ImportChunk {
                        table,
                        schema_version,
                        chunk,
                        reply,
                    } => {
                        let result = match chunk {
                            SourceChunk::Offset(offset) => {
                                let sql = import_select_sql(table, schema_version);
                                read_import_chunk(&source, table, &sql, offset)
                            }
                            SourceChunk::ItemIds(ids) => {
                                read_item_import_chunk(&source, schema_version, table, &ids)
                            }
                        };
                        let _ = reply.send(result);
                    }
                    SourceRequest::Digest {
                        table,
                        schema_version,
                        reply,
                    } => {
                        let _ = reply.send(source_digest(&source, schema_version, table));
                    }
                    SourceRequest::UnsealedCredentialRows { table, reply } => {
                        let _ = reply.send(unsealed_credential_rows(&source, table));
                    }
                    #[cfg(test)]
                    SourceRequest::Pause { duration, reply } => {
                        std::thread::sleep(duration);
                        let _ = reply.send(());
                    }
                }
            }
        });

        let metadata = ready_rx.await.map_err(|_| {
            import_error("SQLite source worker stopped before reporting initialization")
        })??;
        Ok((Self { requests }, metadata))
    }

    async fn item_identity_watermark(&self) -> Result<Option<i64>, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::ItemIdentityWatermark { reply })
            .await?;
        receive_source(response).await
    }
    async fn count(&self, table: TablePlan, for_import: bool) -> Result<i64, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::Count {
            table,
            for_import,
            reply,
        })
        .await?;
        receive_source(response).await
    }

    async fn parent_first_item_ids(&self) -> Result<Vec<i64>, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::ParentFirstItemIds { reply })
            .await?;
        receive_source(response).await
    }

    async fn import_chunk(
        &self,
        table: TablePlan,
        schema_version: i64,
        chunk: SourceChunk,
    ) -> Result<Vec<hiqlite::Params>, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::ImportChunk {
            table,
            schema_version,
            chunk,
            reply,
        })
        .await?;
        receive_source(response).await
    }

    async fn digest(
        &self,
        table: TablePlan,
        schema_version: i64,
    ) -> Result<OrderedRowsDigest, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::Digest {
            table,
            schema_version,
            reply,
        })
        .await?;
        receive_source(response).await
    }

    async fn unsealed_credential_rows(&self, table: TablePlan) -> Result<u64, StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::UnsealedCredentialRows { table, reply })
            .await?;
        receive_source(response).await
    }

    async fn send(&self, request: SourceRequest) -> Result<(), StoreError> {
        self.requests
            .send(request)
            .await
            .map_err(|_| import_error("SQLite source worker stopped during import"))
    }

    #[cfg(test)]
    async fn pause(&self, duration: std::time::Duration) -> Result<(), StoreError> {
        let (reply, response) = oneshot::channel();
        self.send(SourceRequest::Pause { duration, reply }).await?;
        response
            .await
            .map_err(|_| import_error("SQLite source worker stopped during validation pause"))
    }
}

fn source_item_identity_watermark(source: &Connection) -> Result<Option<i64>, StoreError> {
    let present = source.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='item_identity_watermark'",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    if present == 0 {
        return Ok(None);
    }
    let value=source.query_row("SELECT high_water FROM item_identity_watermark WHERE singleton=1 AND importing=0 AND high_water>=coalesce((SELECT max(id) FROM items),0)",[],|r|r.get::<_,i64>(0)).map_err(|_|import_error("invalid or interrupted source item identity allocator"))?;
    Ok(Some(value))
}

async fn receive_source<T>(
    response: oneshot::Receiver<Result<T, StoreError>>,
) -> Result<T, StoreError> {
    response
        .await
        .map_err(|_| import_error("SQLite source worker stopped during import"))?
}

const TABLES: &[TablePlan] = &[
    TablePlan {
        name: "jellyfin_entity_ids",
        columns: &[
            "wire_id",
            "entity_kind",
            "native_id",
            "incarnation",
            "retired",
        ],
        order_by: "wire_id",
        minimum_schema: 98,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_jobs",
        columns: &[
            "id",
            "kind",
            "payload_version",
            "payload_json",
            "dedupe_key",
            "priority",
            "state",
            "target_node_id",
            "owner_node_id",
            "owner_boot_id",
            "claim_id",
            "fence",
            "revision",
            "lease_expires_ms",
            "failure_policy",
            "failed_attempts",
            "attempt_limit",
            "retry_deadline_ms",
            "attempt_errors",
            "index_diagnostic_json",
            "yield_count",
            "abandoned_count",
            "not_before_ms",
            "checkpoint_json",
            "result_ref",
            "last_error_code",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_migration",
        columns: &["singleton", "format_version", "source_count"],
        order_by: "singleton",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_legacy",
        columns: &[
            "legacy_key",
            "kind",
            "snapshot_json",
            "state",
            "job_id",
            "outcome",
            "updated_at_ms",
        ],
        order_by: "legacy_key",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_fragment_targets",
        columns: &["cache_key", "target_node_id", "job_id"],
        order_by: "cache_key, target_node_id",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_waiters",
        columns: &[
            "request_scope",
            "request_id",
            "request_digest",
            "job_id",
            "consumer_kind",
            "consumer_ref",
            "priority",
            "state",
            "target_node_id",
            "deadline_ms",
            "receipt_expires_ms",
            "retain_identity",
            "result_ref",
            "failed_attempts",
            "attempt_limit",
            "not_before_ms",
            "retry_deadline_ms",
            "participation_fence",
            "attempt_errors",
            "last_error_code",
            "index_diagnostic_json",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "request_scope, request_id",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_attempts",
        columns: &[
            "job_id",
            "fence",
            "claim_id",
            "owner_node_id",
            "owner_boot_id",
            "started_at_ms",
            "resolve_until_ms",
            "finished_at_ms",
            "outcome",
            "error_code",
        ],
        order_by: "job_id, fence",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_reservations",
        columns: &["resource_key", "slot", "job_id", "fence", "expires_at_ms"],
        order_by: "resource_key, slot",
        minimum_schema: super::background_jobs::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "settings",
        columns: &["key", "value", "updated_at"],
        order_by: "key",
        minimum_schema: 1,
        import_filter: Some(ImportFilter::ExcludeInstanceId),
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "users",
        columns: &["id", "username", "password_hash", "is_admin", "created_at"],
        order_by: "id",
        minimum_schema: 2,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "tokens",
        columns: &[
            "token_hash",
            "user_id",
            "device",
            "created_at",
            "last_seen_at",
        ],
        order_by: "token_hash",
        minimum_schema: 2,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "jellyfin_login_tokens",
        columns: &["token_hash", "user_id", "device_digest", "client_family"],
        order_by: "token_hash",
        minimum_schema: 99,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    // Playback handoff codes and the tokens they redeemed into; a handoff
    // token lives only in its own table, never in `tokens`.
    TablePlan {
        name: "playback_handoffs",
        columns: &[
            "id",
            "code_hash",
            "user_id",
            "file_id",
            "source_token_hash",
            "start_ms",
            "audio",
            "subtitle",
            "origin",
            "created_at",
            "expires_at",
            "redeemed_at",
        ],
        order_by: "id",
        minimum_schema: 115,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "playback_handoffs_tokens",
        columns: &[
            "token_hash",
            "handoff_id",
            "user_id",
            "file_id",
            "source_token_hash",
            "device",
            "created_at",
            "expires_at",
        ],
        order_by: "token_hash",
        minimum_schema: 115,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "jellyfin_plays",
        columns: &[
            "play_id",
            "user_id",
            "token_digest",
            "device_digest",
            "client_family",
            "playback_id",
            "item_id",
            "file_id",
            "item_wire_id",
            "file_wire_id",
            "payload",
            "state",
            "expires_at_ms",
            "manual_revision",
            "native_incarnation_id",
            "direct_grant_id",
        ],
        order_by: "play_id",
        minimum_schema: 100,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "api_keys",
        columns: &[
            "id",
            "name",
            "key_hash",
            "scopes",
            "created_at",
            "last_used_at",
            "disabled",
        ],
        order_by: "id",
        minimum_schema: 8,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "libraries",
        columns: &[
            "id",
            "name",
            "kind",
            "paths",
            "anime",
            "created_at",
            "scan_interval_mins",
            "refresh_interval_mins",
            "last_scan_at",
            "last_refresh_at",
        ],
        order_by: "id",
        minimum_schema: 7,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "items",
        columns: &[
            "id",
            "library_id",
            "kind",
            "parent_id",
            "title",
            "sort_title",
            "year",
            "overview",
            "tmdb_id",
            "imdb_id",
            "season_number",
            "episode_number",
            "air_date",
            "runtime_ms",
            "poster_path",
            "backdrop_path",
            "added_at",
            "updated_at",
            "recorded_at",
            "tags",
            "nfo_seeded_at",
            "metadata_at",
            "artwork_attempted_at",
            "artwork_error",
            "genres",
            "author",
            "book_work_id",
            "book_edition_id",
            "book_metadata_source",
        ],
        order_by: "id",
        minimum_schema: 13,
        import_filter: None,
        sealed_columns: &[],
        parent_first: true,
    },
    TablePlan {
        name: "files",
        columns: &[
            "id",
            "item_id",
            "path",
            "size",
            "mtime",
            "duration_ms",
            "container",
            "video_codec",
            "video_profile",
            "width",
            "height",
            "bit_depth",
            "hdr",
            "bitrate",
            "audio_streams",
            "subtitle_streams",
            "probe_json",
            "scanned_at",
            "hdr_format",
            "audio_offset_ms",
            // The Dolby Vision configuration record, added with the columns
            // themselves: a legacy database that already ran the backfill
            // carries real values here, and dropping them on import would
            // silently re-open the backfill's whole window.
            "dv_profile",
            "dv_level",
            "dv_bl_compat_id",
            "dv_el_present",
            "dv_rpu_present",
            "video_codec_tag",
            "field_order",
            "max_cll",
            "max_fall",
            "mastering_max_luminance",
            "luminance_source",
            "downloaded_subtitles",
        ],
        order_by: "id",
        minimum_schema: 5,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "dv_conversions",
        columns: &[
            "file_id",
            "state",
            "el_type",
            "original_path",
            "bytes_before",
            "bytes_after",
            "error",
            "queued_at_ms",
            "finished_at_ms",
            "recovery_guard_id",
            "requested_manually",
        ],
        order_by: "file_id",
        minimum_schema: 43,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "dv_recovery_guards",
        columns: &[
            "guard_id",
            "file_id",
            "library_id",
            "source_path",
            "recovery_path",
            "state",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "guard_id",
        minimum_schema: 44,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "watch_state",
        columns: &[
            "user_id",
            "item_id",
            "position_ms",
            "duration_ms",
            "watched",
            "updated_at",
            "manual_revision",
            "manual_origin",
        ],
        order_by: "user_id, item_id",
        minimum_schema: 2,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "reading_state",
        columns: &[
            "user_id",
            "item_id",
            "file_id",
            "file_size",
            "file_mtime",
            "locator_json",
            "progression_millis",
            "completed",
            "updated_at",
        ],
        order_by: "user_id, item_id, file_id",
        minimum_schema: 20,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "trakt_auth",
        columns: &[
            "user_id",
            "access_token",
            "refresh_token",
            "expires_at",
            "trakt_username",
            "connected_at",
            "last_sync_at",
            "last_activities",
        ],
        order_by: "user_id",
        minimum_schema: 5,
        import_filter: None,
        sealed_columns: &["access_token", "refresh_token"],
        parent_first: false,
    },
    TablePlan {
        name: "watched_outbox",
        columns: &[
            "id",
            "payload",
            "attempts",
            "last_error",
            "status",
            "next_at",
            "created_at",
            "updated_at",
            "claim_until",
        ],
        order_by: "id",
        minimum_schema: 10,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_session_requests",
        columns: &[
            "user_id",
            "request_id",
            "request_fingerprint",
            "playback_id",
            "state",
            "claim_expires_at_ms",
            "incarnation_id",
            "owner_node_id",
            "response_json",
            "updated_at_ms",
        ],
        order_by: "user_id, request_id",
        minimum_schema: 25,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_session_recipes",
        columns: &[
            "user_id",
            "request_id",
            "incarnation_id",
            "recipe_json",
            "created_at_ms",
        ],
        order_by: "user_id, request_id",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_playback_pointers",
        columns: &[
            "user_id",
            "playback_id",
            "current_incarnation_id",
            "updated_at_ms",
            // Carried rather than recomputed. The fence triggers read a null
            // here on a playback that has an ask as a writer from before the
            // column, so a restore that dropped this would turn every imported
            // pointer into one the next write cannot replace. Sources older
            // than v49 have no such column and are handled by
            // `value_projection`, not by leaving it out here.
            "desired_revision",
        ],
        order_by: "user_id, playback_id",
        minimum_schema: 25,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_session_preparations",
        columns: &[
            "user_id",
            "playback_id",
            "staged_incarnation_id",
            "expected_predecessor_incarnation_id",
            "deadline_ms",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "user_id, playback_id",
        // A brand-new table, so `minimum_schema` is the whole story: a source
        // older than v42 simply has no rows to carry and needs no arm in
        // `value_projection`.
        minimum_schema: 42,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_playback_desired",
        columns: &[
            "user_id",
            "playback_id",
            "revision",
            "digest",
            "canonical_form",
            "updated_at_ms",
        ],
        order_by: "user_id, playback_id",
        // A brand-new table, so `minimum_schema` is the whole story: a source
        // older than this has no rows to carry and needs no arm in
        // `value_projection`. Carried on import rather than dropped, because
        // what a viewer asked for is the one fact about a playback that a
        // rebuilt cluster cannot re-derive from anything else it holds.
        //
        // 48 is the SQLite migration that creates the table. This field is
        // compared against the *source's* `PRAGMA user_version`, and it said
        // 28 — the replicated AUTH version, which is a different number line
        // entirely. A source between v28 and v47 would have been sent to
        // count and read a table it does not have.
        // `every_import_plan_names_the_migration_that_creates_its_table`
        // now makes that class of mistake fail at compile-test time rather
        // than during someone's restore.
        minimum_schema: 48,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_sessions",
        columns: &[
            "incarnation_id",
            "session_id",
            "user_id",
            "playback_id",
            "request_fingerprint",
            "owner_node_id",
            "owner_epoch",
            "lease_expires_at_ms",
            "state",
            "terminal_reason",
            "publication_ready_at_ms",
            "recipe_json",
            "response_json",
            "produced_playable_through_ms",
            "fetched_through_ms",
            "media_origin_ms",
            "media_sequence",
            "discontinuity_sequence",
            "updated_at_ms",
            "drain_deadline_ms",
            "recovery_epoch",
        ],
        order_by: "incarnation_id",
        minimum_schema: 25,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "candidate_recovery",
        columns: &[
            "scope",
            "recipe",
            "cause",
            "event_id",
            "user_id",
            "playback_id",
            "recovery_epoch",
            "created_ms",
            "quality_step",
        ],
        order_by: "scope, recipe, cause",
        minimum_schema: super::candidate_recovery::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_session_terminal_acks",
        columns: &[
            "incarnation_id",
            "session_id",
            "owner_node_id",
            "owner_epoch",
            "client_instance_id",
            "sequence",
            "request_fingerprint",
            "response_json",
            "expires_at_ms",
            "updated_at_ms",
        ],
        order_by: "session_id",
        minimum_schema: 32,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    // The three continuous-quality tables travel with `media_sessions`: an
    // imported active parent without its ledger would lose the reserved
    // intervals that keep scheduled media from eviction, and without its
    // cancellation receipts and preparation owners a cancelled quality intent
    // could be restaged or committed by the rebuilt cluster. All three are
    // brand-new tables, so `minimum_schema` is the whole story.
    TablePlan {
        name: "quality_cancellation_receipts",
        columns: &[
            "receipt_key",
            "generation",
            "session_id",
            "owner_node_id",
            "owner_epoch",
            "client_instance_id",
            "lifetime_id",
            "recipe_revision",
            "accepted_sequence",
            "state",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "receipt_key",
        minimum_schema: super::sqlite::QUALITY_CANCELLATION_SQLITE_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "quality_preparation_owners",
        columns: &["staged_incarnation_id", "cancellation_key"],
        order_by: "staged_incarnation_id",
        minimum_schema: super::sqlite::QUALITY_CANCELLATION_SQLITE_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "continuous_quality_ledgers",
        columns: &[
            "generation",
            "owner_node_id",
            "owner_epoch",
            "revision",
            "attachment_id",
            "ledger_json",
            "updated_at_ms",
        ],
        order_by: "generation",
        minimum_schema: super::sqlite::QUALITY_LEDGER_SQLITE_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "pretranscode_jobs",
        columns: &[
            "id",
            "dedupe_key",
            "file_id",
            "source_size",
            "source_mtime",
            "target_height",
            "policy_generation",
            "requirements_json",
            "reason",
            "priority",
            "state",
            "owner_node_id",
            "staging_node_id",
            "fence",
            "lease_expires_ms",
            "attempts",
            "not_before_ms",
            "last_error_code",
            "recipe_hash",
            "storage_id",
            "relative_dir",
            "manifest_digest",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 24,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cluster_fragment_index_sources",
        columns: &[
            "node_id",
            "file_id",
            "object_version",
            "source_size",
            "source_mtime",
            "source_sha256",
            "observed_at_ms",
        ],
        order_by: "node_id, file_id",
        minimum_schema: 30,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cluster_fragment_index_jobs",
        columns: &[
            "cache_key",
            "file_id",
            "source_size",
            "source_mtime",
            "source_sha256",
            "pipeline_sha256",
            "priority",
            "trigger",
            "target_node_id",
            "state",
            "owner_node_id",
            "fence",
            "lease_expires_ms",
            "attempts",
            "not_before_ms",
            "last_error_code",
            "attempt_errors",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "cache_key, target_node_id",
        minimum_schema: 30,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cluster_fragment_index_artifacts",
        columns: &[
            "cache_key",
            "file_id",
            "source_size",
            "source_mtime",
            "source_sha256",
            "pipeline_sha256",
            "blob_sha256",
            "bytes",
            "built_by_node_id",
            "built_at_ms",
        ],
        order_by: "cache_key",
        minimum_schema: 30,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cluster_fragment_index_locations",
        columns: &[
            "cache_key",
            "node_id",
            "bytes",
            "verified_at_ms",
            "last_seen_at_ms",
        ],
        order_by: "cache_key, node_id",
        minimum_schema: 30,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "analysis_requests",
        columns: &[
            "request_id",
            "file_id",
            "source_size",
            "source_mtime",
            "component",
            "pipeline_version",
            "requested_generation",
            "expected_predecessor_generation",
            "priority",
            "trigger",
            "force_rebuild",
            "target_node_id",
            "state",
            "owner_node_id",
            "fence",
            "lease_expires_ms",
            "attempts",
            "not_before_ms",
            "result_cache_key",
            "last_error_code",
            "cancel_requested",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "request_id",
        minimum_schema: 31,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "analysis_attempts",
        columns: &[
            "request_id",
            "attempt",
            "claim_node_id",
            "claim_epoch",
            "claim_expires_at_ms",
            "phase",
            "started_at_ms",
            "phase_updated_at_ms",
            "terminal_code",
        ],
        order_by: "request_id, claim_epoch",
        minimum_schema: 41,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cluster_fragment_index_heads",
        columns: &[
            "logical_cache_key",
            "generation_cache_key",
            "request_id",
            "updated_at_ms",
        ],
        order_by: "logical_cache_key",
        minimum_schema: 41,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "analysis_lifecycle_counters",
        columns: &["event", "reason", "count"],
        order_by: "event, reason",
        minimum_schema: 41,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "timeline_annotation_sets",
        columns: &[
            "file_id",
            "source_size",
            "source_mtime",
            "argv_fingerprint",
            "generation_id",
            "version",
            "annotations_json",
            "updated_at_ms",
            "publication_priority",
        ],
        order_by: "file_id",
        minimum_schema: 39,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "timeline_manual_overrides",
        columns: &[
            "file_id",
            "kind",
            "source_size",
            "source_mtime",
            "argv_fingerprint",
            "start_ticks",
            "end_ticks",
            "timescale",
            "start_ms",
            "end_ms",
            "revision",
            "generation_id",
            "updated_at_ms",
        ],
        order_by: "file_id, kind",
        minimum_schema: 40,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "transcode_cache_recipes",
        columns: &["recipe_hash", "file_id", "recipe_version", "created_at"],
        order_by: "recipe_hash",
        minimum_schema: 11,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "transcode_cache_locations",
        columns: &[
            "recipe_hash",
            "node_id",
            "storage_class",
            "relative_dir",
            "bytes",
            "complete",
            "manifest_digest",
            "scrub_object_index",
            "last_used_at",
            "last_seen_at",
            "storage_id",
            "generation_id",
            "publication_generation",
        ],
        order_by: "recipe_hash, node_id, storage_class",
        minimum_schema: 11,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_transcode_artifacts",
        columns: &[
            "recipe_hash",
            "manifest_digest",
            "file_id",
            "source_size",
            "source_mtime",
            "recipe_version",
            "built_by_node_id",
            "built_at_ms",
            "producer_payload",
        ],
        order_by: "recipe_hash, manifest_digest",
        minimum_schema: super::background_jobs_transcode::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cache_storage_members",
        columns: &[
            "storage_id",
            "node_id",
            "storage_class",
            "verified_at_ms",
            "verification_state",
        ],
        order_by: "storage_id, node_id",
        minimum_schema: 26,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "cache_consumer_pins",
        columns: &[
            "storage_id",
            "recipe_hash",
            "generation_id",
            "consumer_kind",
            "consumer_id",
            "consumer_epoch",
            "expires_at_ms",
        ],
        order_by: "storage_id, recipe_hash, generation_id, consumer_kind, consumer_id",
        minimum_schema: 26,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "offline_packages",
        columns: &[
            "id",
            "request_id",
            "user_id",
            "file_id",
            "node_id",
            "source_path",
            "source_size",
            "source_mtime",
            "recipe_hash",
            "claim_generation",
            "decoder_recovery_state",
            "alternate_recipe_hash",
            "effective_rate_control",
            "target_height",
            "output_width",
            "output_height",
            "audio_index",
            "audio_offset_ms",
            "subtitle_index",
            "subtitle_language",
            "subtitle_mode",
            "state",
            "phase",
            "progress_millis",
            "estimated_bytes",
            "reserved_bytes",
            "actual_bytes",
            "duration_ms",
            "error_code",
            "error_message",
            "created_at",
            "updated_at",
            "last_access_at",
            "expires_at",
            "audio_recipe",
        ],
        order_by: "id",
        minimum_schema: 14,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "offline_package_leases",
        columns: &[
            "token_hash",
            "package_id",
            "created_at",
            "last_access_at",
            "expires_at",
        ],
        order_by: "token_hash",
        minimum_schema: 14,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "file_grants",
        columns: &[
            "id",
            "token_hash",
            "file_id",
            "user_id",
            "source_token_hash",
            "purpose",
            "created_at",
            "expires_at",
            "revoked_at",
        ],
        order_by: "id",
        minimum_schema: 68,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_roots",
        columns: &["library_id", "fingerprint"],
        order_by: "library_id",
        minimum_schema: 15,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "scan_reconcile_guards",
        columns: &["library_id"],
        order_by: "library_id",
        minimum_schema: 15,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "scan_reconcile_items",
        columns: &["library_id", "item_id"],
        order_by: "library_id, item_id",
        minimum_schema: 15,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channels",
        columns: &[
            "id",
            "owner_user_id",
            "name",
            "description",
            "visibility",
            "enabled",
            "definition_revision",
            "recipe_json",
            "seed",
            "active_generation_id",
            "active_epoch_ms",
            "pending_generation_id",
            "pending_epoch_ms",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_generations",
        columns: &[
            "id",
            "channel_id",
            "definition_revision",
            "algorithm_version",
            "content_digest",
            "state",
            "entry_count",
            "loop_duration_ms",
            "build_claim_id",
            "build_claim_expires_ms",
            "created_at_ms",
        ],
        order_by: "id",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_entries",
        columns: &[
            "generation_id",
            "ordinal",
            "item_id",
            "file_id",
            "file_size",
            "file_mtime",
            "duration_ms",
            "cumulative_start_ms",
            "show_id",
        ],
        order_by: "generation_id, ordinal",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_favourites",
        columns: &["user_id", "channel_id", "created_at_ms"],
        order_by: "user_id, channel_id",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_requests",
        columns: &[
            "user_id",
            "request_id",
            "operation_hash",
            "channel_id",
            "result_revision",
            "created_at_ms",
            "expires_at_ms",
        ],
        order_by: "user_id, request_id",
        minimum_schema: 55,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "media_classifications",
        columns: &[
            "item_id",
            "source_json",
            "payload",
            "overrides",
            "terms",
            "revision",
        ],
        order_by: "item_id",
        minimum_schema: 61,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_subject_jobs",
        columns: &[
            "id",
            "owner_user_id",
            "channel_id",
            "channel_revision",
            "identity",
            "state",
            "payload",
            "claim_id",
            "claim_expires_ms",
            "available_ms",
            "updated_ms",
            "expires_ms",
        ],
        order_by: "id",
        minimum_schema: 58,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_channel_subject_decisions",
        columns: &[
            "owner_user_id",
            "subject_digest",
            "item_id",
            "metadata_digest",
            "classifier_profile",
            "payload",
            "last_used_ms",
        ],
        order_by: "owner_user_id, subject_digest, item_id, metadata_digest, classifier_profile",
        minimum_schema: 58,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "dvr_rules",
        columns: &[
            "id",
            "owner_user_id",
            "priority",
            "name",
            "match_mode",
            "match_value",
            "channel_id",
            "new_only",
            "keep_mode",
            "keep_value",
            "pad_start_s",
            "pad_end_s",
            "enabled",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 57,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "live_tv_resource_records",
        columns: &["id", "kind", "user_id", "live", "expires_at_ms", "body"],
        order_by: "id",
        minimum_schema: 83,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "dvr_recordings",
        columns: &[
            "id",
            "origin",
            "rule_id",
            "requested_by_user_id",
            "channel_id",
            "guide_number",
            "channel_name",
            "airing_start",
            "airing_end",
            "capture_start",
            "capture_end",
            "title",
            "episode_title",
            "episode",
            "synopsis",
            "image_url",
            "original_air_date",
            "series_id",
            "programme_id",
            "state",
            "state_reason",
            "attempt",
            "gap_s",
            "late_start_s",
            "tuner_owner_node_id",
            "path",
            "bytes",
            "last_progress_ms",
            "stop_requested_at_ms",
            "stop_requested_by_user_id",
            "item_id",
            "file_id",
            "started_at_ms",
            "finished_at_ms",
            "stopped_by_user_id",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 57,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "dvr_reminders",
        columns: &[
            "id",
            "user_id",
            "channel_id",
            "guide_number",
            "airing_start",
            "airing_end",
            "title",
            "lead_s",
            "state",
            "fired_at_ms",
            "acked_at_ms",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 57,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_job_domain_leases",
        columns: &[
            "resource",
            "domain_fence",
            "job_id",
            "job_fence",
            "node_id",
            "boot_id",
            "claim_id",
        ],
        order_by: "resource",
        minimum_schema: super::background_jobs_domain::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_provider_budgets",
        columns: &["provider", "next_dispatch_ms", "interval_ms"],
        order_by: "provider",
        minimum_schema: super::background_jobs_provider::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_storage_domains",
        columns: &["library_id", "root_path", "domain_id"],
        order_by: "library_id, root_path",
        minimum_schema: super::background_jobs_resources::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_artifact_repairs",
        columns: &[
            "id",
            "original_key",
            "target_node_id",
            "location_generation",
            "artifact_key",
            "producer_payload",
            "phase",
            "job_id",
            "created_at_ms",
            "updated_at_ms",
            "expires_ms",
        ],
        order_by: "id",
        minimum_schema: super::background_jobs_integrity::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_embeddings",
        columns: &[
            "item_id",
            "model_digest",
            "content_digest",
            "artifact_json",
            "built_by_node_id",
            "built_at_ms",
        ],
        order_by: "item_id, model_digest",
        minimum_schema: super::background_jobs_embeddings::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_predictions",
        columns: &[
            "request_id",
            "file_id",
            "request_json",
            "expires_ms",
            "state",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "request_id",
        minimum_schema: super::background_jobs_predictions::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_artwork_locations",
        columns: &[
            "artifact_key",
            "node_id",
            "spec_json",
            "blob_sha256",
            "bytes",
            "built_by_node_id",
            "built_at_ms",
            "verified_at_ms",
        ],
        order_by: "artifact_key, node_id",
        minimum_schema: super::background_jobs_artwork::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "background_library_requests",
        columns: &[
            "request_id",
            "library_id",
            "job_id",
            "input_json",
            "result_json",
            "completed_claim_id",
            "completed_at_ms",
        ],
        order_by: "request_id",
        minimum_schema: super::background_jobs_library::SQLITE_INTRODUCED_SCHEMA,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "job_leases",
        columns: &[
            "resource",
            "owner_node_id",
            "fence",
            "revision",
            "expires_at_ms",
            "updated_at_ms",
        ],
        order_by: "resource",
        minimum_schema: 23,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_identity",
        columns: &["singleton", "server_id", "catalogue_epoch", "created_at_ms"],
        order_by: "singleton",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_invitations",
        columns: &[
            "id",
            "token_hash",
            "library_ids_json",
            "created_at_ms",
            "expires_at_ms",
            "state",
            "claim_id",
            "claim_digest",
        ],
        order_by: "id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_exports",
        columns: &[
            "id",
            "invitation_id",
            "recipient_server_id",
            "recipient_name",
            "token_hash",
            "scope_generation",
            "credential_generation",
            "catalogue_generation",
            "mutation_generation",
            "state",
            "pending_expires_at_ms",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_export_libraries",
        columns: &["grant_id", "library_id"],
        order_by: "grant_id, library_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_imports",
        columns: &[
            "id",
            "source_server_id",
            "catalogue_epoch",
            "source_name",
            "claim_id",
            "remote_grant_id",
            "credential_envelope",
            "claim_envelope",
            "endpoints_json",
            "assignment_generation",
            "lifecycle_generation",
            "endpoint_generation",
            "observed_scope_generation",
            "observed_credential_generation",
            "observed_catalogue_generation",
            "observed_endpoint_revision",
            "state",
            "created_at_ms",
            "updated_at_ms",
        ],
        order_by: "id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &["credential_envelope", "claim_envelope"],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_viewers",
        columns: &["user_id", "viewer_id"],
        order_by: "user_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_assignments",
        columns: &["import_id", "remote_library_id", "user_id", "enabled"],
        order_by: "import_id, remote_library_id, user_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_watch",
        columns: &[
            "source_server_id",
            "catalogue_epoch",
            "remote_library_id",
            "remote_item_id",
            "user_id",
            "position_ms",
            "duration_ms",
            "watched",
            "sequence",
            "updated_at_ms",
        ],
        order_by: "source_server_id, catalogue_epoch, remote_item_id, user_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_rotations",
        columns: &[
            "grant_id",
            "request_id",
            "old_hash",
            "new_hash",
            "created_at_ms",
            "expires_at_ms",
        ],
        order_by: "grant_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_import_rotations",
        columns: &[
            "import_id",
            "request_id",
            "credential_envelope",
            "created_at_ms",
            "expires_at_ms",
        ],
        order_by: "import_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &["credential_envelope"],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_catalogue_revisions",
        columns: &["library_id", "order_revision"],
        order_by: "library_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_endpoint_manifest",
        columns: &["singleton", "endpoints_json", "revision"],
        order_by: "singleton",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_relay_upstream",
        columns: &[
            "incarnation_id",
            "import_id",
            "lifecycle_generation",
            "assignment_generation",
            "remote_library_id",
            "remote_item_id",
            "remote_file_id",
            "remote_revision",
            "source_request_id",
            "source_session_id",
            "source_incarnation_id",
            "endpoint_revision",
            "capability_envelope",
            "source_position_ms",
            "dispatch_envelope",
        ],
        order_by: "incarnation_id",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &["capability_envelope", "dispatch_envelope"],
        parent_first: false,
    },
    TablePlan {
        name: "sharing_delivery_grants",
        columns: &[
            "token_hash",
            "incarnation_id",
            "source_token_hash",
            "state",
            "deadline_ms",
        ],
        order_by: "token_hash",
        minimum_schema: 104,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    // Preserve unresolved physical debts as diagnostic custody. Imported rows
    // never confer a driver handle or a closure receipt in this process boot.
    TablePlan {
        name: "sharing_ingress_custody",
        columns: &[
            "principal_kind",
            "incarnation_id",
            "owner_identity",
            "custody_json",
            "revision",
        ],
        order_by: "principal_kind, incarnation_id",
        minimum_schema: 105,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    }, // A person's watchlist and favorites, and the provider scores cached per
    // item, follow the catalogue they reference.
    TablePlan {
        name: "library_annotations_lists",
        columns: &["user_id", "list", "item_id", "added_at_ms"],
        order_by: "user_id, list, item_id",
        minimum_schema: 113,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_annotations_ratings",
        columns: &["item_id", "source", "value", "scale", "votes"],
        order_by: "item_id, source",
        minimum_schema: 113,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
    TablePlan {
        name: "library_annotations_rating_fetches",
        columns: &["item_id", "fetched_at_ms", "omdb_consulted"],
        order_by: "item_id",
        minimum_schema: 113,
        import_filter: None,
        sealed_columns: &[],
        parent_first: false,
    },
];

/// Groups import rows into transactions bounded by serialized bytes.
///
/// Both chunk producers — the offset paging path and the parent-first item-id
/// path — feed rows through one of these, so the WAL bound is enforced in one
/// place rather than once per producer. The builder is deliberately free of any
/// client: it decides *what* to submit, and the caller submits it, which is
/// what lets the boundary cases be tested without a running voter.
///
/// Rows are emitted in the order they are pushed, so the parent-first ordering
/// the items table depends on survives being split across transactions.
struct ImportTransactionBuilder {
    table: TablePlan,
    /// Estimated bytes each statement adds beyond its parameters: the INSERT
    /// text is repeated verbatim once per row inside the Raft entry.
    statement_bytes: usize,
    pending: Vec<hiqlite::Params>,
    pending_bytes: usize,
    /// 1-based position of the next row in the table's import order, used as a
    /// safe identifier when a row has no integer primary key to name.
    next_ordinal: u64,
}

impl ImportTransactionBuilder {
    fn new(table: TablePlan, insert_sql: &str) -> Self {
        Self {
            table,
            statement_bytes: insert_sql.len() + PER_STATEMENT_FRAMING_BYTES,
            pending: Vec::new(),
            pending_bytes: IMPORT_TXN_ENVELOPE_BYTES,
            next_ordinal: 1,
        }
    }

    /// Adds one row, returning the transaction that must be submitted first
    /// when this row would push the pending one past its bounds.
    ///
    /// Refuses a row that no transaction could carry instead of handing it to
    /// `txn`, where the WAL writer would panic the process.
    fn push(&mut self, row: hiqlite::Params) -> Result<Option<Vec<hiqlite::Params>>, StoreError> {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| import_error("import row ordinal overflow"))?;
        let row_bytes = self.statement_bytes + estimated_row_bytes(&row);
        if row_bytes > IMPORT_MAX_ROW_BYTES {
            return Err(import_error(format!(
                "table {} {} serializes to about {row_bytes} bytes, above the \
                 {IMPORT_MAX_ROW_BYTES}-byte single-row import limit derived from the \
                 {HIQLITE_WAL_USABLE_PAYLOAD_BYTES}-byte production WAL payload capacity; \
                 import refuses this backup instead of crashing the node mid-import. \
                 The source database is unchanged",
                self.table.name,
                self.row_identifier(&row, ordinal),
            )));
        }

        let full = self.pending.len() >= IMPORT_CHUNK_ROWS as usize
            || self.pending_bytes + row_bytes > IMPORT_TXN_BUDGET_BYTES;
        let ready = if full { self.take() } else { None };
        self.pending.push(row);
        self.pending_bytes += row_bytes;
        Ok(ready)
    }

    /// Returns the last partially filled transaction, if any.
    fn finish(&mut self) -> Option<Vec<hiqlite::Params>> {
        self.take()
    }

    fn take(&mut self) -> Option<Vec<hiqlite::Params>> {
        if self.pending.is_empty() {
            return None;
        }
        self.pending_bytes = IMPORT_TXN_ENVELOPE_BYTES;
        Some(std::mem::take(&mut self.pending))
    }

    /// Names a row without quoting any imported value.
    ///
    /// Media paths, titles, and credential hashes are all first columns
    /// somewhere in [`TABLES`], and none of them may reach a log line or an
    /// operator-facing error. An integer primary key is safe and is what an
    /// operator can actually look up; everything else falls back to the row's
    /// position in the table's deterministic import order.
    fn row_identifier(&self, row: &[Param], ordinal: u64) -> String {
        match (self.table.columns.first(), row.first()) {
            (Some(column), Some(Param::Integer(value))) => format!("row {column}={value}"),
            _ => format!("row {ordinal} of the {} import order", self.table.order_by),
        }
    }
}

/// Bytes a statement costs inside a transaction beyond its SQL text and its
/// parameters: the length prefix on `Query::sql` and the statement's slot in the
/// transaction vector. The parameter vector's own header is charged by
/// [`estimated_row_bytes`] instead, so it is not counted here.
const PER_STATEMENT_FRAMING_BYTES: usize = 32;

/// Conservative upper bound on the bytes one row adds to a Raft entry.
///
/// `hiqlite-wal` 0.14 encodes log entries with `bincode::config::legacy()` —
/// fixed-width integers, chosen there for speed — not with `standard()`:
/// `log_store_impl.rs` serializes every appended entry with that config, and
/// those are the bytes the writer thread measures before it panics. The
/// `standard()` calls elsewhere in the crate serialize metadata and snapshots,
/// which never carry an import transaction.
///
/// Rather than depend on one config, every param is charged the widest form it
/// takes under either. A mis-estimate then makes transactions smaller than
/// necessary, never larger than the WAL.
fn estimated_row_bytes(row: &[Param]) -> usize {
    PARAM_VEC_HEADER_BYTES
        + row
            .iter()
            .map(|param| PER_PARAM_FRAMING_BYTES + param_payload_bytes(param))
            .sum::<usize>()
}

/// Length prefix written ahead of the row's parameter vector: a fixed 8 bytes
/// under `legacy()`, never more than a 9-byte varint under `standard()`.
const PARAM_VEC_HEADER_BYTES: usize = 9;

/// Enum discriminant plus the one integer field the widest `Param` variant puts
/// after it. Under `legacy()` that is a fixed 4 plus a fixed 8; under
/// `standard()` neither a varint discriminant nor a varint length exceeds those.
const PER_PARAM_FRAMING_BYTES: usize = 12;

fn param_payload_bytes(param: &Param) -> usize {
    match param {
        Param::Null => 0,
        // Both land inside the framing allowance's integer field: a fixed-width
        // `i64` and an `f64` bit pattern are 8 bytes each.
        Param::Integer(_) => 0,
        Param::Real(_) => 0,
        Param::Text(value) => value.len(),
        Param::Blob(value) => value.len(),
        // Import never builds statement-output references, but both carry a
        // second integer past the framing allowance. Charge it rather than
        // leave a future caller silently under-counted.
        Param::StmtOutputIndexed(..) => 8,
        Param::StmtOutputNamed(_, name) => 8 + name.len(),
    }
}

impl HiqliteAuthStore {
    /// Import one immutable, content-addressed SQLite backup into a fresh
    /// Hiqlite target and prove exact row parity before returning evidence.
    ///
    /// Schema v14 is the oldest supported clustering source. v14 lacks the
    /// scan-reconciliation tables and the outbox claim deadline; those facts
    /// are imported as empty tables and a zero deadline respectively. Media
    /// sessions before v34 lack a terminal cause, and those before v35 lack a
    /// replacement-publication fence, so import projects `NULL` and zero for
    /// those fields. v17's playback events are node-local telemetry and
    /// intentionally absent from this mapping.
    pub async fn import_sqlite_backup(
        &self,
        backup_path: &Path,
        expected_sha256: &str,
        expected_schema_version: i64,
    ) -> Result<SqliteImportReport, StoreError> {
        self.import_sqlite_backup_inner(
            backup_path,
            expected_sha256,
            expected_schema_version,
            false,
        )
        .await
    }

    /// Validation-only fault seam proving the per-table parity comparison is
    /// an enforced gate rather than report decoration.
    #[doc(hidden)]
    pub async fn validation_import_sqlite_backup_with_parity_fault(
        &self,
        backup_path: &Path,
        expected_sha256: &str,
        expected_schema_version: i64,
    ) -> Result<SqliteImportReport, StoreError> {
        self.import_sqlite_backup_inner(backup_path, expected_sha256, expected_schema_version, true)
            .await
    }

    async fn import_sqlite_backup_inner(
        &self,
        backup_path: &Path,
        expected_sha256: &str,
        expected_schema_version: i64,
        inject_parity_fault: bool,
    ) -> Result<SqliteImportReport, StoreError> {
        let (source, metadata) =
            SourceReader::open(backup_path, expected_sha256, expected_schema_version).await?;
        let schema_version = metadata.schema_version;
        self.refuse_unsealed_source_credentials(&source, schema_version)
            .await?;
        self.verify_empty_import_target().await?;
        let identity_allocator = self
            .target_count(
                "sqlite_master",
                Some("type='table' AND name='item_identity_watermark'".into()),
            )
            .await?
            == 1;
        let source_watermark = source.item_identity_watermark().await?;
        if source_watermark.is_some() && !identity_allocator {
            return Err(import_error(
                "source item identity allocator unavailable on import target",
            ));
        }
        if identity_allocator {
            let changed = self.client().execute("UPDATE item_identity_watermark SET importing=1 WHERE singleton=1 AND importing=0 AND high_water=0",params!()).await?;
            if changed != 1 {
                return Err(import_error(
                    "item identity import target is not fresh; discard incoming target",
                ));
            }
        }
        // A fresh queue schema has an empty seal marker. The backup supplies
        // its own marker; older backups are sealed only after parity is proved.
        self.client()
            .execute(
                "DELETE FROM background_job_migration WHERE source_count = 0",
                params!(),
            )
            .await?;
        self.import_instance_setting(&metadata).await?;

        for table in TABLES {
            self.import_table(&source, schema_version, *table).await?;
        }

        // This CAS counter is derived coordination state, not application
        // data. Rebuild it above every imported attempt epoch before serving.
        self.client()
            .execute(
                "UPDATE live_tv_resource_revision SET revision=MAX(revision,
            COALESCE((SELECT MAX(CAST(json_extract(body,'$.value.epoch') AS INTEGER))
                FROM live_tv_resource_records),0)),nonce='' WHERE singleton=1",
                params!(),
            )
            .await?;

        let search_rows = self.rebuild_search_index().await?;
        let item_count = self.target_count("items", None).await?;
        if search_rows != item_count {
            return Err(import_error(format!(
                "rebuilt items_fts has {search_rows} rows, expected {item_count}; discard the incoming target"
            )));
        }
        self.verify_target_foreign_keys().await?;
        if inject_parity_fault {
            let changed = self
                .client()
                .execute(
                    "UPDATE settings SET value = $1 WHERE key = $2",
                    params!("corrupted-after-import", "migration.fixture"),
                )
                .await?;
            if changed != 1 {
                return Err(import_error(format!(
                    "validation parity fault changed {changed} rows"
                )));
            }
        }

        let mut tables = Vec::with_capacity(TABLES.len());
        let mut imported_rows = 0_u64;
        for table in TABLES {
            let source_digest = source.digest(*table, schema_version).await?;
            let target_digest = self.target_digest(*table).await?;
            if source_digest != target_digest {
                return Err(import_error(format!(
                    "table {} failed SQLite-to-Hiqlite parity (source rows {}, target rows {}); discard the incoming target",
                    table.name,
                    source_digest.row_count,
                    target_digest.row_count
                )));
            }
            let row_count = source_digest.row_count;
            imported_rows = imported_rows
                .checked_add(row_count)
                .ok_or_else(|| import_error("imported row count overflow"))?;
            tables.push(SqliteImportTableDigest {
                table: table.name.to_owned(),
                row_count,
                sha256: source_digest.sha256,
            });
        }

        if schema_version < super::background_jobs::SQLITE_INTRODUCED_SCHEMA {
            // Triggers were installed at bootstrap; now capture the imported
            // legacy rows in the same finite schema-defined snapshot.
            super::hiqlite_background_jobs::seal_legacy(self.client()).await?;
        }

        if schema_version < 76 {
            // Parity describes the imported snapshot. Only after proving it do
            // we revoke pre-common-queue owners; their spent attempts survive.
            self.client()
                .execute(super::background_jobs_subtitle::RESET_LEGACY, params!())
                .await?;
        }

        if identity_allocator {
            self.client().execute("UPDATE item_identity_watermark SET high_water=max(high_water,$1) WHERE singleton=1 AND importing=1",params!(source_watermark.unwrap_or(0))).await?;
            self.client().execute("INSERT INTO sharing_catalogue_revisions(library_id,order_revision) SELECT id,1 FROM libraries WHERE true ON CONFLICT(library_id) DO NOTHING",params!()).await?;
            self.client().execute("UPDATE item_identity_watermark SET importing=0 WHERE singleton=1 AND importing=1",params!()).await?;
        }
        Ok(SqliteImportReport {
            source_schema_version: schema_version,
            backup_sha256: metadata.backup_sha256,
            imported_rows,
            search_rows,
            tables,
        })
    }

    /// Refuse a backup carrying a credential this build would replicate in the
    /// clear, before a single row is submitted to Raft.
    ///
    /// The check is up front rather than per chunk because Raft is the thing
    /// being protected. Every durable backend write already refuses an unsealed
    /// credential, but import does not go through those writers — it copies
    /// source columns straight into the target — so without this the one path
    /// that bypasses the write-time gate is also the one that fans the value
    /// out to every voter. Discarding a half-imported target recovers the
    /// database; it does not recover a bearer token from committed log entries
    /// on three machines.
    ///
    /// The remedy named in the error is real: `open_store` seals cleartext rows
    /// on boot, so starting this build on the SQLite install and taking a fresh
    /// backup produces a source this accepts.
    async fn refuse_unsealed_source_credentials(
        &self,
        source: &SourceReader,
        schema_version: i64,
    ) -> Result<(), StoreError> {
        for table in TABLES {
            if table.sealed_columns.is_empty() || schema_version < table.minimum_schema {
                continue;
            }
            let unsealed = source.unsealed_credential_rows(*table).await?;
            if unsealed != 0 {
                return Err(import_error(format!(
                    "SQLite backup has {unsealed} {} row(s) whose credential columns are not \
                     sealed envelopes; importing them would replicate a usable credential to \
                     every voter. Start this build on the SQLite install so it seals those rows, \
                     then take a fresh backup",
                    table.name
                )));
            }
        }
        Ok(())
    }

    async fn verify_empty_import_target(&self) -> Result<(), StoreError> {
        // Device credentials are never imported: restore cannot resurrect
        // pairings. A nonempty target is refused before any import write.
        // authority: refuse import before writes if the target retains live device credentials.
        let remote = self.client().query_consistent_map::<CountRow,_>("SELECT (SELECT count(*) FROM remote_receivers)+(SELECT count(*) FROM remote_grants)+(SELECT count(*) FROM invitation_phones)+(SELECT count(*) FROM invitation_consents)+(SELECT count(*) FROM invitation_events) AS count",params!()).await?;
        if !matches!(remote.as_slice(),[r] if r.count==0) {
            return Err(StoreError::Migration(
                "import target has Cinema remote credentials".into(),
            ));
        }

        for table in TABLES {
            let filter = import_filter_sql(*table);
            let rows = self
                .target_count(
                    table.name,
                    if table.name == "background_job_migration" {
                        Some("source_count > 0".to_owned())
                    } else {
                        filter
                    },
                )
                .await?;
            if rows != 0 {
                return Err(import_error(format!(
                    "Hiqlite import target is not fresh: table {} already has {rows} application row(s)",
                    table.name
                )));
            }
        }
        let guards = self.target_count("offline_lease_guards", None).await?;
        if guards != 0 {
            return Err(import_error(format!(
                "Hiqlite import target is not fresh: table offline_lease_guards already has {guards} row(s)"
            )));
        }
        Ok(())
    }

    async fn import_instance_setting(&self, source: &SourceMetadata) -> Result<(), StoreError> {
        let target_id = self.instance_id().await?;
        if source.instance_id != target_id {
            return Err(StoreError::Identity(format!(
                "SQLite instance.id {} does not match incoming cluster instance.id {target_id}",
                source.instance_id
            )));
        }
        let changed = self
            .client()
            .execute(
                "UPDATE settings SET updated_at = $1 WHERE key = $2 AND value = $3",
                params!(
                    source.instance_updated_at,
                    keys::INSTANCE_ID,
                    source.instance_id.clone()
                ),
            )
            .await?;
        if changed != 1 {
            return Err(import_error(format!(
                "updating imported instance.id timestamp changed {changed} rows"
            )));
        }
        Ok(())
    }

    async fn import_table(
        &self,
        source: &SourceReader,
        schema_version: i64,
        table: TablePlan,
    ) -> Result<(), StoreError> {
        if schema_version < table.minimum_schema {
            return Ok(());
        }
        let expected = source.count(table, true).await?;
        let insert_sql = insert_sql(table);
        let mut transactions = ImportTransactionBuilder::new(table, &insert_sql);
        if table.parent_first {
            let ids = source.parent_first_item_ids().await?;
            let selected =
                i64::try_from(ids.len()).map_err(|error| import_error(error.to_string()))?;
            if selected != expected {
                let unreachable = expected.saturating_sub(selected);
                return Err(import_error(format!(
                    "items parent graph contains {unreachable} row(s) unreachable from a root; a parent_id cycle is possible"
                )));
            }
            for ids in ids.chunks(IMPORT_CHUNK_ROWS as usize) {
                let chunk = source
                    .import_chunk(table, schema_version, SourceChunk::ItemIds(ids.to_vec()))
                    .await?;
                for row in chunk {
                    if let Some(ready) = transactions.push(row)? {
                        self.insert_import_chunk(table, &insert_sql, ready).await?;
                    }
                }
            }
            if let Some(ready) = transactions.finish() {
                self.insert_import_chunk(table, &insert_sql, ready).await?;
            }
            return Ok(());
        }

        let mut offset = 0_i64;
        while offset < expected {
            let chunk = source
                .import_chunk(table, schema_version, SourceChunk::Offset(offset))
                .await?;
            if chunk.is_empty() {
                break;
            }
            let chunk_len =
                i64::try_from(chunk.len()).map_err(|error| import_error(error.to_string()))?;
            for row in chunk {
                if let Some(ready) = transactions.push(row)? {
                    self.insert_import_chunk(table, &insert_sql, ready).await?;
                }
            }
            offset += chunk_len;
        }
        if let Some(ready) = transactions.finish() {
            self.insert_import_chunk(table, &insert_sql, ready).await?;
        }
        if offset != expected {
            return Err(import_error(format!(
                "table {} import selected {offset} of {expected} source rows",
                table.name
            )));
        }
        Ok(())
    }

    async fn insert_import_chunk(
        &self,
        table: TablePlan,
        insert_sql: &str,
        chunk: Vec<hiqlite::Params>,
    ) -> Result<(), StoreError> {
        // Last gate before the transaction becomes a Raft entry.
        // `ImportTransactionBuilder` already keeps every chunk it emits well
        // inside this, so reaching it means a caller bypassed the builder —
        // still an error to return rather than a WAL writer panic to survive.
        let estimated = IMPORT_TXN_ENVELOPE_BYTES
            + chunk
                .iter()
                .map(|row| {
                    insert_sql.len() + PER_STATEMENT_FRAMING_BYTES + estimated_row_bytes(row)
                })
                .sum::<usize>();
        if estimated > HIQLITE_WAL_USABLE_PAYLOAD_BYTES {
            return Err(import_error(format!(
                "table {} import transaction of {} row(s) serializes to about {estimated} bytes, \
                 above the {HIQLITE_WAL_USABLE_PAYLOAD_BYTES}-byte production WAL payload capacity",
                table.name,
                chunk.len(),
            )));
        }
        let results = self
            .client()
            .txn(chunk.into_iter().map(|row| (insert_sql.to_owned(), row)))
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if results.iter().any(|changed| *changed != 1) {
            return Err(import_error(format!(
                "table {} import did not insert exactly one row per statement",
                table.name
            )));
        }
        Ok(())
    }

    async fn target_count(
        &self,
        table: &'static str,
        filter: Option<String>,
    ) -> Result<u64, StoreError> {
        let sql = match filter {
            Some(filter) => format!("SELECT COUNT(*) AS count FROM {table} WHERE {filter}"),
            None => format!("SELECT COUNT(*) AS count FROM {table}"),
        };
        let rows = self
            .client()
            .query_consistent_map::<CountRow, _>(sql, params!())
            .await?;
        one_count(rows, table)
    }

    async fn verify_target_foreign_keys(&self) -> Result<(), StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM pragma_foreign_key_check",
                params!(),
            )
            .await?;
        let dangling = one_count(rows, "pragma_foreign_key_check")?;
        if dangling != 0 {
            return Err(import_error(format!(
                "Hiqlite import left {dangling} dangling foreign key reference(s); discard the incoming target"
            )));
        }
        Ok(())
    }

    async fn target_digest(&self, table: TablePlan) -> Result<OrderedRowsDigest, StoreError> {
        let mut cursor = Vec::new();
        let mut digest = OrderedRowsHasher::new();
        loop {
            let (sql, query_params) = target_parity_page(table, &cursor)?;
            let page = self
                .client()
                .query_consistent_map::<ParityPageRow, _>(sql, query_params)
                .await?;
            if page.is_empty() {
                break;
            }
            if page.len() > PARITY_PAGE_ROWS as usize {
                return Err(import_error(format!(
                    "table {} parity page returned {} rows above the {PARITY_PAGE_ROWS}-row bound",
                    table.name,
                    page.len()
                )));
            }
            for row in &page {
                digest.push(&row.value)?;
            }
            let next_cursor = parity_cursor_params(table, &page[page.len() - 1].cursor)?;
            if next_cursor == cursor {
                return Err(import_error(format!(
                    "table {} parity cursor did not advance",
                    table.name
                )));
            }
            cursor = next_cursor;
            if page.len() < PARITY_PAGE_ROWS as usize {
                break;
            }
        }
        Ok(digest.finish())
    }
}

fn target_parity_page(
    table: TablePlan,
    cursor: &[Param],
) -> Result<(String, hiqlite::Params), StoreError> {
    let projection = json_projection(table, SQLITE_SCHEMA_VERSION, false);
    let keys = parity_key_columns(table);
    let cursor_projection = keys.join(", ");
    let mut query_params = cursor.to_vec();
    let filter = if cursor.is_empty() {
        String::new()
    } else {
        if cursor.len() != keys.len() {
            return Err(import_error(format!(
                "table {} parity cursor has {} values for {} ordering columns",
                table.name,
                cursor.len(),
                keys.len()
            )));
        }
        let placeholders = (1..=cursor.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" WHERE ({cursor_projection}) > ({placeholders})")
    };
    query_params.push(Param::Integer(PARITY_PAGE_ROWS));
    let limit = query_params.len();
    Ok((
        format!(
            "SELECT json_array({projection}) AS value, \
             json_array({cursor_projection}) AS cursor \
             FROM {}{filter} ORDER BY {} LIMIT ${limit}",
            table.name, table.order_by
        ),
        query_params,
    ))
}

fn parity_key_columns(table: TablePlan) -> Vec<&'static str> {
    table.order_by.split(", ").collect()
}

fn parity_cursor_params(table: TablePlan, cursor: &str) -> Result<Vec<Param>, StoreError> {
    let values = serde_json::from_str::<Vec<serde_json::Value>>(cursor).map_err(|error| {
        import_error(format!(
            "decoding table {} parity cursor {cursor}: {error}",
            table.name
        ))
    })?;
    let keys = parity_key_columns(table);
    if values.len() != keys.len() {
        return Err(import_error(format!(
            "table {} parity cursor has {} values for {} ordering columns",
            table.name,
            values.len(),
            keys.len()
        )));
    }
    values
        .into_iter()
        .map(|value| match value {
            serde_json::Value::Number(value) => value
                .as_i64()
                .map(Param::Integer)
                .ok_or_else(|| import_error("parity cursor integer is outside i64")),
            serde_json::Value::String(value) => Ok(Param::Text(value)),
            other => Err(import_error(format!(
                "table {} parity cursor contains unsupported key {other}",
                table.name
            ))),
        })
        .collect()
}

fn open_source(path: &Path) -> Result<Connection, StoreError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| import_error(format!("opening SQLite backup {}: {error}", path.display())))
}

fn open_validated_source(
    path: &Path,
    expected_sha256: &str,
    expected_schema_version: i64,
) -> Result<(Connection, SourceMetadata), StoreError> {
    let actual_sha256 = sha256_file(path)?;
    if actual_sha256 != expected_sha256 {
        return Err(import_error(format!(
            "SQLite backup checksum changed: expected {expected_sha256}, got {actual_sha256}"
        )));
    }
    let source = open_source(path)?;
    let schema_version = source_schema_version(&source)?;
    if schema_version != expected_schema_version {
        return Err(import_error(format!(
            "SQLite backup schema changed: expected v{expected_schema_version}, got v{schema_version}"
        )));
    }
    if !(MINIMUM_IMPORT_SCHEMA_VERSION..=SQLITE_SCHEMA_VERSION).contains(&schema_version) {
        return Err(import_error(format!(
            "clustering import supports SQLite schemas v{MINIMUM_IMPORT_SCHEMA_VERSION}..=v{SQLITE_SCHEMA_VERSION}, got v{schema_version}"
        )));
    }
    verify_source(&source)?;
    verify_source_dv_recovery_guards(&source, schema_version)?;
    let (instance_id, instance_updated_at) = source
        .query_row(
            "SELECT value, updated_at FROM settings WHERE key = ?1",
            [keys::INSTANCE_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| import_error(format!("reading source instance.id: {error}")))?;
    Ok((
        source,
        SourceMetadata {
            backup_sha256: actual_sha256,
            schema_version,
            instance_id,
            instance_updated_at,
        },
    ))
}

fn source_schema_version(source: &Connection) -> Result<i64, StoreError> {
    source
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| import_error(format!("reading SQLite backup schema: {error}")))
}

fn verify_source(source: &Connection) -> Result<(), StoreError> {
    let integrity: String = source
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| import_error(format!("checking SQLite backup: {error}")))?;
    if integrity != "ok" {
        return Err(import_error(format!(
            "SQLite backup failed quick_check: {integrity}"
        )));
    }
    let dangling: i64 = source
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|error| import_error(format!("checking SQLite backup foreign keys: {error}")))?;
    if dangling != 0 {
        return Err(import_error(format!(
            "SQLite backup contains {dangling} dangling foreign key reference(s)"
        )));
    }
    Ok(())
}

/// Refuse every non-null recovery link unless the immutable backup contains
/// its exact guard identity. A committed deleted-original claim additionally
/// requires that exact guard to be active. Pre-v44 sources have no guard
/// ledger, so such a claim cannot be made truthful by import and must be
/// repaired on the source install first.
fn verify_source_dv_recovery_guards(
    source: &Connection,
    schema_version: i64,
) -> Result<(), StoreError> {
    if schema_version < 43 {
        return Ok(());
    }
    let invalid: i64 = if schema_version < 44 {
        source.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM dv_conversions
                  WHERE state = 'committed' AND original_path IS NULL
                  LIMIT 1
             )",
            [],
            |row| row.get(0),
        )
    } else {
        source.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM dv_conversions d
                   JOIN files f ON f.id = d.file_id
                   JOIN items i ON i.id = f.item_id
              LEFT JOIN dv_recovery_guards g
                     ON g.guard_id = d.recovery_guard_id
                    AND g.file_id = d.file_id
                    AND g.library_id = i.library_id
                    AND g.source_path = f.path
                  WHERE (d.recovery_guard_id IS NOT NULL AND g.guard_id IS NULL)
                     OR (d.state = 'committed' AND d.original_path IS NULL
                         AND (d.recovery_guard_id IS NULL OR g.guard_id IS NULL
                              OR g.state != 'active'))
                  LIMIT 1
             )",
            [],
            |row| row.get(0),
        )
    }
    .map_err(|error| {
        import_error(format!(
            "checking source Dolby Vision recovery guards: {error}"
        ))
    })?;
    if invalid != 0 {
        return Err(import_error(
            "SQLite backup contains a Dolby Vision conversion without its exact recovery guard linkage, or a committed deleted-original claim without its exact active recovery guard; repair the recovery link or retain the original on the source install before clustering",
        ));
    }
    Ok(())
}

fn source_count(
    source: &Connection,
    table: TablePlan,
    for_import: bool,
) -> Result<i64, StoreError> {
    let filter = for_import.then(|| import_filter_sql(table)).flatten();
    let sql = match filter {
        Some(filter) => format!("SELECT COUNT(*) FROM {} WHERE {filter}", table.name),
        None => format!("SELECT COUNT(*) FROM {}", table.name),
    };
    source
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| import_error(format!("counting source table {}: {error}", table.name)))
}

/// Count the source rows whose sealed columns are not envelopes this build can
/// parse.
///
/// The test is [`SealedSecret::is_wrapped`], not the laxer
/// [`looks_wrapped`](SealedSecret::looks_wrapped) the SQLite upgrade pass uses.
/// The upgrade pass is deciding whether re-sealing would destroy a credential,
/// so a damaged envelope belongs on its refusal path. Import is deciding what
/// may enter a replicated log that never forgets, and a damaged envelope is a
/// credential nobody can open — copying it into every voter fixes nothing and
/// makes the source install's problem permanent.
///
/// Only the count leaves this function. The values are exactly what must not
/// reach an error string or a log line.
fn unsealed_credential_rows(source: &Connection, table: TablePlan) -> Result<u64, StoreError> {
    if table.sealed_columns.is_empty() {
        return Ok(0);
    }
    let projection = table.sealed_columns.join(", ");
    let mut statement = source
        .prepare(&format!("SELECT {projection} FROM {}", table.name))
        .map_err(|error| {
            import_error(format!(
                "reading sealed columns of source table {}: {error}",
                table.name
            ))
        })?;
    let mut rows = statement.query([]).map_err(|error| {
        import_error(format!(
            "reading sealed columns of source table {}: {error}",
            table.name
        ))
    })?;

    let mut unsealed = 0_u64;
    while let Some(row) = rows.next().map_err(|error| {
        import_error(format!(
            "reading sealed columns of source table {}: {error}",
            table.name
        ))
    })? {
        // A NULL or non-text value in a credential column is not an envelope
        // either, so it fails closed with everything else rather than reading
        // as "nothing to check here".
        let sealed = (0..table.sealed_columns.len()).all(|index| {
            row.get::<_, Option<String>>(index).is_ok_and(|value| {
                value.map_or(
                    matches!(
                        table.sealed_columns[index],
                        "claim_envelope" | "capability_envelope" | "dispatch_envelope"
                    ),
                    |value| {
                        // The not-dispatched marker is not a credential.
                        (table.sealed_columns[index] == "dispatch_envelope" && value == "none")
                            || SealedSecret::from_stored(value).is_wrapped()
                    },
                )
            })
        });
        if !sealed {
            unsealed = unsealed
                .checked_add(1)
                .ok_or_else(|| import_error("unsealed credential row count overflow"))?;
        }
    }
    Ok(unsealed)
}

fn import_select_sql(table: TablePlan, schema_version: i64) -> String {
    let projection = value_projection(table, schema_version, false);
    let filter = import_filter_sql(table)
        .map(|filter| format!(" WHERE {filter}"))
        .unwrap_or_default();
    let order_by = source_order_by(table, schema_version);
    format!(
        "SELECT {projection} FROM {}{filter} ORDER BY {order_by} LIMIT ?1 OFFSET ?2",
        table.name
    )
}

fn source_order_by(table: TablePlan, schema_version: i64) -> &'static str {
    if table.name == "cluster_fragment_index_jobs" && schema_version < 41 {
        "cache_key"
    } else {
        table.order_by
    }
}

fn import_filter_sql(table: TablePlan) -> Option<String> {
    table.import_filter.map(|ImportFilter::ExcludeInstanceId| {
        format!("key <> '{}'", keys::INSTANCE_ID.replace('\'', "''"))
    })
}

fn parent_first_item_ids(source: &Connection) -> Result<Vec<i64>, StoreError> {
    let mut statement = source
        .prepare(
            "WITH RECURSIVE import_items(id, depth) AS (
                 SELECT id, 0 FROM items WHERE parent_id IS NULL
                 UNION ALL
                 SELECT child.id, parent.depth + 1 FROM items AS child
                 JOIN import_items AS parent ON child.parent_id = parent.id
             )
             SELECT id FROM import_items ORDER BY depth, id",
        )
        .map_err(|error| import_error(format!("ordering source items by parent: {error}")))?;
    let mapped = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|error| import_error(format!("ordering source items by parent: {error}")))?;
    mapped
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| import_error(format!("ordering source items by parent: {error}")))
}

fn read_item_import_chunk(
    source: &Connection,
    schema_version: i64,
    table: TablePlan,
    ids: &[i64],
) -> Result<Vec<hiqlite::Params>, StoreError> {
    let projection = value_projection(table, schema_version, true);
    let sql = format!("SELECT {projection} FROM items AS source WHERE source.id = ?1");
    let mut statement = source
        .prepare(&sql)
        .map_err(|error| import_error(format!("preparing source items: {error}")))?;
    ids.iter()
        .map(|id| {
            statement
                .query_row([id], |row| {
                    (0..table.columns.len())
                        .map(|index| value_param(row.get_ref(index)?))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(|error| import_error(format!("reading source item {id}: {error}")))
        })
        .collect()
}

fn insert_sql(table: TablePlan) -> String {
    let columns = table.columns.join(", ");
    let placeholders = (1..=table.columns.len())
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({columns}) VALUES ({placeholders})",
        table.name
    )
}

fn read_import_chunk(
    source: &Connection,
    table: TablePlan,
    sql: &str,
    offset: i64,
) -> Result<Vec<hiqlite::Params>, StoreError> {
    let mut statement = source
        .prepare(sql)
        .map_err(|error| import_error(format!("preparing source table {}: {error}", table.name)))?;
    let mapped = statement
        .query_map([IMPORT_CHUNK_ROWS, offset], |row| {
            (0..table.columns.len())
                .map(|index| value_param(row.get_ref(index)?))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|error| import_error(format!("reading source table {}: {error}", table.name)))?;
    mapped
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| import_error(format!("reading source table {}: {error}", table.name)))
}

fn value_param(value: ValueRef<'_>) -> rusqlite::Result<Param> {
    Ok(match value {
        ValueRef::Null => Param::Null,
        ValueRef::Integer(value) => Param::Integer(value),
        ValueRef::Real(value) => Param::Real(value),
        ValueRef::Text(value) => {
            Param::Text(String::from_utf8(value.to_vec()).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?)
        }
        ValueRef::Blob(_) => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Blob,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "BLOB import needs a deterministic parity encoding",
                )),
            ));
        }
    })
}

fn source_digest(
    source: &Connection,
    schema_version: i64,
    table: TablePlan,
) -> Result<OrderedRowsDigest, StoreError> {
    if schema_version < table.minimum_schema {
        return Ok(OrderedRowsHasher::new().finish());
    }
    let projection = json_projection(table, schema_version, false);
    let order_by = source_order_by(table, schema_version);
    let sql = format!(
        "SELECT json_array({projection}) AS value FROM {} ORDER BY {order_by}",
        table.name
    );
    let mut statement = source
        .prepare(&sql)
        .map_err(|error| import_error(format!("hashing source table {}: {error}", table.name)))?;
    let mapped = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| import_error(format!("hashing source table {}: {error}", table.name)))?;
    let mut digest = OrderedRowsHasher::new();
    for row in mapped {
        let row = row.map_err(|error| {
            import_error(format!("hashing source table {}: {error}", table.name))
        })?;
        digest.push(&row)?;
    }
    Ok(digest.finish())
}

fn json_projection(table: TablePlan, schema_version: i64, qualify: bool) -> String {
    value_projection(table, schema_version, qualify)
}

fn value_projection(table: TablePlan, schema_version: i64, qualify: bool) -> String {
    table
        .columns
        .iter()
        .map(|column| {
            if table.name == "watch_state" && *column == "manual_origin" {
                // Origin is mutation context, never restored authority. Replaying
                // it could advance an otherwise fenced binding during import.
                "NULL".to_owned()
            } else if table.name == "watch_state" && *column == "manual_revision" && schema_version < 101 {
                "0".to_owned()
            } else if table.name == "media_playback_pointers"
                && *column == "desired_revision"
                && schema_version < 49
            {
                // A source from before the column has no ask to attribute the
                // pointer to, and null is the honest answer rather than a
                // fabricated revision. The fence lets it through for exactly
                // as long as the playback has no `media_playback_desired` row
                // — which such a source also cannot have, because that table
                // arrived at v48 and this one at v49. Inventing a revision
                // here is the "never replace a missing expected token with the
                // current revision" mistake, spelled as a restore.
                "NULL".to_owned()
            } else if table.name == "cluster_fragment_index_jobs"
                && *column == "priority"
                && schema_version < 41
            {
                "'normal'".to_owned()
            } else if table.name == "cluster_fragment_index_jobs"
                && *column == "trigger"
                && schema_version < 41
            {
                "'background'".to_owned()
            } else if table.name == "cluster_fragment_index_jobs"
                && *column == "target_node_id"
                && schema_version < 41
            {
                if schema_version < 31 {
                    return "''".to_owned();
                }
                let cache_key = if qualify { "source.cache_key" } else { "cache_key" };
                format!("COALESCE((SELECT request.target_node_id FROM analysis_requests request WHERE request.result_cache_key = {cache_key} ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), '')")
            } else if table.name == "timeline_annotation_sets"
                && *column == "publication_priority"
                && schema_version < 41
            {
                "'normal'".to_owned()
            } else if table.name == "analysis_requests"
                && *column == "pipeline_version"
                && schema_version < 41
            {
                let component = if qualify { "source.component" } else { "component" };
                format!("CASE {component} WHEN 'fragment_index' THEN 'legacy-fragment-index' ELSE 'chapter-classifier-v1' END")
            } else if table.name == "analysis_requests"
                && *column == "requested_generation"
                && schema_version < 41
            {
                if qualify { "source.request_id".to_owned() } else { "request_id".to_owned() }
            } else if table.name == "analysis_requests"
                && *column == "expected_predecessor_generation"
                && schema_version < 41
            {
                "''".to_owned()
            } else if table.name == "analysis_requests"
                && *column == "priority"
                && schema_version < 41
            {
                let force = if qualify { "source.force_rebuild" } else { "force_rebuild" };
                format!("CASE {force} WHEN 1 THEN 'forced' ELSE 'normal' END")
            } else if table.name == "analysis_requests"
                && *column == "trigger"
                && schema_version < 41
            {
                "'admin'".to_owned()
            } else if (table.name == "analysis_requests"
                && *column == "cancel_requested"
                && schema_version < 41)
                || (table.name == "watched_outbox"
                    && *column == "claim_until"
                    && schema_version < 16)
            {
                "0".to_owned()
            } else if table.name == "media_sessions"
                && *column == "terminal_reason"
                && schema_version < 34
            {
                "NULL".to_owned()
            } else if table.name == "media_sessions"
                && *column == "publication_ready_at_ms"
                && schema_version < 35
            {
                "0".to_owned()
            } else if table.name == "media_sessions"
                && *column == "recovery_epoch"
                && schema_version < 53
            {
                "''".to_owned()
            } else if table.name == "media_sessions"
                && *column == "drain_deadline_ms"
                && schema_version < 51
            {
                // A source from before the column has no draining session to
                // describe, and null is what "not draining" is spelled as
                // everywhere else. Zero would be a deadline in 1970 that every
                // reader would then have to except.
                "NULL".to_owned()
            } else if table.name == "files"
                && matches!(
                    *column,
                    "dv_profile"
                        | "dv_level"
                        | "dv_bl_compat_id"
                        | "dv_el_present"
                        | "dv_rpu_present"
                )
                && schema_version < 38
            {
                // The backup is opened read-only and never migrated, so a
                // source below v38 has no such column to select. NULL is also
                // the honest value: the destination's own backfill will fill
                // it in from the probe JSON that comes across with the row.
                "NULL".to_owned()
            } else if table.name == "files"
                && *column == "video_codec_tag"
                && schema_version < 60
            {
                // The source is opened read-only and cannot be migrated. Its
                // stored probe JSON crosses with the row and the destination's
                // bounded backfill recovers a valid tag afterward.
                "NULL".to_owned()
            } else if table.name == "background_transcode_artifacts" && *column == "producer_payload" && schema_version < 82 {
                "NULL".to_owned()
            } else if table.name == "dv_conversions" && *column == "requested_manually" && schema_version < 90 {
                "0".to_owned()
            } else if table.name == "files" && *column == "downloaded_subtitles" && schema_version < 67 {
                "'[]'".to_owned()
            } else if table.name == "files" && *column == "field_order" && schema_version < 64 {
                // The stored probe document crosses with every legacy row;
                // the destination's fenced bounded job recovers the token.
                "NULL".to_owned()
            } else if table.name == "files"
                && matches!(
                    *column,
                    "max_cll" | "max_fall" | "mastering_max_luminance" | "luminance_source"
                )
                && schema_version < 65
            {
                // The luminance columns land one step after the field-order
                // column, so v64 sources still predate them; the stored probe
                // JSON crosses with the row and the bounded job classifies it.
                "NULL".to_owned()
            } else if table.name == "cluster_fragment_index_jobs"
                && *column == "attempt_errors"
                && schema_version < 46
            {
                // A source below v46 kept no per-attempt history, and an empty
                // one is the honest value: the column is NOT NULL DEFAULT ''.
                "''".to_owned()
            } else if (table.name == "dv_conversions"
                && *column == "recovery_guard_id"
                && schema_version < 44)
                || (table.name == "items"
                    && matches!(
                        *column,
                        "author" | "book_work_id" | "book_edition_id" | "book_metadata_source"
                    )
                    && schema_version < 21)
                || (table.name == "offline_packages"
                    && *column == "audio_recipe"
                    && schema_version < super::sqlite::OFFLINE_AUDIO_RECIPE_SCHEMA)
            {
                "NULL".to_owned()
            } else if table.name == "offline_packages"
                && *column == "effective_rate_control"
                && schema_version < 18
            {
                "'vbr'".to_owned()
            } else if table.name == "offline_packages"
                && matches!(
                    *column,
                    "claim_generation" | "decoder_recovery_state" | "alternate_recipe_hash"
                )
                && schema_version < 54
            {
                match *column {
                    "claim_generation" => "0".to_owned(),
                    "decoder_recovery_state" => "'primary'".to_owned(),
                    _ => "NULL".to_owned(),
                }
            } else if table.name == "transcode_cache_locations"
                && *column == "manifest_digest"
                && schema_version < 24
            {
                "NULL".to_owned()
            } else if table.name == "transcode_cache_locations"
                && *column == "scrub_object_index"
                && schema_version < 24
            {
                "0".to_owned()
            } else if table.name == "transcode_cache_locations"
                && *column == "storage_id"
                && schema_version < 26
            {
                if qualify {
                    "'node:' || source.node_id || ':cache'".to_owned()
                } else {
                    "'node:' || node_id || ':cache'".to_owned()
                }
            } else if table.name == "transcode_cache_locations"
                && *column == "generation_id"
                && schema_version < 26
            {
                if qualify {
                    "source.relative_dir".to_owned()
                } else {
                    "relative_dir".to_owned()
                }
            } else if table.name == "transcode_cache_locations"
                && *column == "publication_generation"
                && schema_version < 54
            {
                "0".to_owned()
            } else if qualify {
                format!("source.{column}")
            } else {
                (*column).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sha256_file(path: &Path) -> Result<String, StoreError> {
    let mut file = File::open(path).map_err(|error| {
        import_error(format!("opening SQLite backup {}: {error}", path.display()))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            import_error(format!("reading SQLite backup {}: {error}", path.display()))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn import_error(message: impl Into<String>) -> StoreError {
    StoreError::Migration(message.into())
}

struct CountRow {
    count: i64,
}

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            count: row.get("count"),
        }
    }
}

fn one_count(rows: Vec<CountRow>, source: &str) -> Result<u64, StoreError> {
    let [row] = rows.as_slice() else {
        return Err(import_error(format!(
            "counting {source} returned {} rows",
            rows.len()
        )));
    };
    u64::try_from(row.count).map_err(|error| import_error(error.to_string()))
}

struct ParityPageRow {
    value: String,
    cursor: String,
}

impl From<&mut Row<'_>> for ParityPageRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            value: row.get("value"),
            cursor: row.get("cursor"),
        }
    }
}

#[cfg(test)]
mod tests;
