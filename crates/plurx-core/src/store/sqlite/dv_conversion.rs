//! SQLite permanent Dolby Vision conversion ledger.

use async_trait::async_trait;
use rusqlite::types::Type;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::dv_conversion::eligibility_reason;
use crate::store::{
    DvConversion, DvConversionCandidate, DvConversionProgress, DvConversionState,
    DvConversionStore, QueueDvConversionOutcome,
};

const CONVERSION_COLS: &str = "file_id, state, el_type, original_path, bytes_before,
    bytes_after, error, queued_at_ms, finished_at_ms";

fn invalid_state(column: usize, state: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        format!("invalid Dolby Vision conversion state `{state}`").into(),
    )
}

fn conversion_from_row(row: &Row<'_>) -> rusqlite::Result<DvConversion> {
    let state: String = row.get(1)?;
    Ok(DvConversion {
        file_id: row.get(0)?,
        state: DvConversionState::parse(&state).ok_or_else(|| invalid_state(1, state))?,
        el_type: row.get(2)?,
        original_path: row.get(3)?,
        bytes_before: row.get(4)?,
        bytes_after: row.get(5)?,
        error: row.get(6)?,
        queued_at_ms: row.get(7)?,
        finished_at_ms: row.get(8)?,
    })
}

fn read_conversion(
    conn: &rusqlite::Connection,
    file_id: i64,
) -> rusqlite::Result<Option<DvConversion>> {
    conn.query_row(
        &format!("SELECT {CONVERSION_COLS} FROM dv_conversions WHERE file_id = ?1"),
        [file_id],
        conversion_from_row,
    )
    .optional()
}

#[async_trait]
impl DvConversionStore for SqliteStore {
    async fn dv_conversion(&self, file_id: i64) -> Result<Option<DvConversion>, StoreError> {
        self.with_conn(move |conn| Ok(read_conversion(conn, file_id)?))
            .await
    }

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError> {
        self.with_conn(move |conn| {
            let facts = conn
                .query_row(
                    "SELECT dv_profile, dv_bl_compat_id, dv_el_present, dv_rpu_present
                       FROM files WHERE id = ?1",
                    [file_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<i64>>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<i64>>(2)?.map(|value| value != 0),
                            row.get::<_, Option<i64>>(3)?.map(|value| value != 0),
                        ))
                    },
                )
                .optional()?;
            let Some((profile, bl_compat_id, el_present, rpu_present)) = facts else {
                return Ok(QueueDvConversionOutcome::FileMissing);
            };
            if let Some(reason) = eligibility_reason(profile, bl_compat_id, el_present, rpu_present)
            {
                return Ok(QueueDvConversionOutcome::Ineligible(reason));
            }

            if let Some(existing) = read_conversion(conn, file_id)? {
                match existing.state {
                    DvConversionState::Committed => {
                        return Ok(QueueDvConversionOutcome::AlreadyCommitted(existing));
                    }
                    DvConversionState::Queued
                    | DvConversionState::Running
                    | DvConversionState::Verified => {
                        return Ok(QueueDvConversionOutcome::AlreadyActive(existing));
                    }
                    DvConversionState::Failed => {}
                }
            }

            conn.execute(
                "INSERT INTO dv_conversions
                   (file_id, state, queued_at_ms)
                 VALUES (?1, 'queued', ?2)
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE dv_conversions.state = 'failed'",
                params![file_id, queued_at_ms],
            )?;
            let queued =
                read_conversion(conn, file_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            Ok(QueueDvConversionOutcome::Queued(queued))
        })
        .await
    }

    async fn queue_library_dv_conversions(
        &self,
        library_id: i64,
        queued_at_ms: i64,
        retry_failed: bool,
    ) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 SELECT f.id, 'queued', ?2
                   FROM files f JOIN items i ON i.id = f.item_id
                  WHERE i.library_id = ?1
                    AND f.dv_profile = 7
                    AND f.dv_bl_compat_id IN (1, 6)
                    AND f.dv_el_present = 1
                    AND f.dv_rpu_present = 1
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE ?3 AND dv_conversions.state = 'failed'",
                params![library_id, queued_at_ms, retry_failed],
            )?;
            Ok(changed as u64)
        })
        .await
    }

    async fn dv_conversion_candidates(
        &self,
        after_file_id: i64,
        limit: i64,
    ) -> Result<Vec<DvConversionCandidate>, StoreError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT d.file_id, i.library_id, d.state
                   FROM dv_conversions d
                   JOIN files f ON f.id = d.file_id
                   JOIN items i ON i.id = f.item_id
                  WHERE d.file_id > ?1
                    AND d.state IN ('verified', 'running', 'queued')
                  ORDER BY d.file_id
                  LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_file_id, limit], |row| {
                let state: String = row.get(2)?;
                Ok(DvConversionCandidate {
                    file_id: row.get(0)?,
                    library_id: row.get(1)?,
                    state: DvConversionState::parse(&state)
                        .ok_or_else(|| invalid_state(2, state))?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError> {
        self.with_read(move |conn| {
            Ok(conn.query_row(
                "SELECT
                   COALESCE(SUM(CASE WHEN f.dv_profile = 7
                                          AND f.dv_bl_compat_id IN (1, 6)
                                          AND f.dv_el_present = 1
                                          AND f.dv_rpu_present = 1
                                     THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'queued' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'running' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'verified' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'committed' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'failed' THEN 1 ELSE 0 END), 0)
                 FROM files f
                 JOIN items i ON i.id = f.item_id
                 LEFT JOIN dv_conversions d ON d.file_id = f.id
                 WHERE ?1 IS NULL OR i.library_id = ?1",
                [library_id],
                |row| {
                    Ok(DvConversionProgress {
                        eligible: row.get(0)?,
                        queued: row.get(1)?,
                        running: row.get(2)?,
                        verified: row.get(3)?,
                        committed: row.get(4)?,
                        failed: row.get(5)?,
                    })
                },
            )?)
        })
        .await
    }

    async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'running', bytes_before = ?2, bytes_after = NULL,
                        error = NULL, finished_at_ms = NULL
                  WHERE file_id = ?1 AND state IN ('queued', 'running')",
                params![file_id, bytes_before],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_verified(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
    ) -> Result<bool, StoreError> {
        if !matches!(el_type, None | Some("mel" | "fel")) {
            return Err(StoreError::Task(
                "Dolby Vision enhancement-layer type must be mel, fel, or unknown".to_owned(),
            ));
        }
        let el_type = el_type.map(str::to_owned);
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'verified', el_type = ?2, bytes_after = ?3, error = NULL
                  WHERE file_id = ?1 AND state = 'running'",
                params![file_id, el_type, bytes_after],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_committed(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let original_path = original_path.map(str::to_owned);
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'committed', original_path = ?2, bytes_after = ?3,
                        error = NULL, finished_at_ms = ?4
                  WHERE file_id = ?1 AND state = 'verified'",
                params![file_id, original_path, bytes_after, finished_at_ms],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let error = error.chars().take(4096).collect::<String>();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'failed', error = ?2, finished_at_ms = ?3
                  WHERE file_id = ?1 AND state != 'committed'",
                params![file_id, error, finished_at_ms],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::{
        DolbyVisionFacts, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
    };
    use crate::store::{LibraryStore, MediaStore};

    async fn p7_file(store: &SqliteStore, name: &str, compat_id: i64) -> (i64, i64) {
        let library = store
            .create_library(&NewLibrary {
                name: name.to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from(format!("/{name}"))],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: name.to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = store
            .upsert_file(
                item,
                &format!("/{name}/{name}.mkv"),
                80_000,
                7,
                &ProbeResult {
                    dolby_vision: DolbyVisionFacts {
                        profile: Some(7),
                        level: Some(6),
                        bl_compat_id: Some(compat_id),
                        el_present: Some(true),
                        rpu_present: Some(true),
                    },
                    raw_json: Some("{}".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        (library.id, file)
    }

    #[tokio::test]
    async fn ledger_enforces_eligibility_transitions_and_committed_terminal_state() {
        let store = SqliteStore::open_in_memory().expect("store");
        let (library_id, file_id) = p7_file(&store, "HDR10", 6).await;
        let (_, hlg_file_id) = p7_file(&store, "HLG", 4).await;

        assert!(matches!(
            store
                .queue_dv_conversion(hlg_file_id, 10)
                .await
                .expect("reject HLG"),
            QueueDvConversionOutcome::Ineligible(_)
        ));
        assert!(matches!(
            store.queue_dv_conversion(file_id, 11).await.expect("queue"),
            QueueDvConversionOutcome::Queued(_)
        ));
        assert!(store
            .mark_dv_conversion_running(file_id, 80_000)
            .await
            .expect("running"));
        assert!(store
            .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
            .await
            .expect("verified"));
        assert!(store
            .mark_dv_conversion_committed(file_id, Some("/HDR10/HDR10.mkv.p7.orig"), 60_000, 12,)
            .await
            .expect("committed"));

        let row = store
            .dv_conversion(file_id)
            .await
            .expect("row")
            .expect("conversion");
        assert_eq!(row.state, DvConversionState::Committed);
        assert_eq!(row.el_type.as_deref(), Some("fel"));
        assert_eq!(row.bytes_before, Some(80_000));
        assert_eq!(row.bytes_after, Some(60_000));
        assert!(matches!(
            store
                .queue_dv_conversion(file_id, 13)
                .await
                .expect("terminal queue"),
            QueueDvConversionOutcome::AlreadyCommitted(_)
        ));
        assert!(!store
            .mark_dv_conversion_failed(file_id, "late stale worker", 14)
            .await
            .expect("terminal failure refused"));

        let progress = store
            .dv_conversion_progress(Some(library_id))
            .await
            .expect("progress");
        assert_eq!(progress.eligible, 1);
        assert_eq!(progress.committed, 1);
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn sqlite_v38_fixture_migrates_the_conversion_ledger_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v38.db");
        let connection = rusqlite::Connection::open(&path).expect("fixture");
        SqliteStore::apply_migrations_for_test(
            &connection,
            crate::store::SQLITE_SCHEMA_VERSION - 1,
        )
        .expect("v38 schema");
        drop(connection);

        let _store = SqliteStore::open(&path).expect("migrate v38");
        let connection = rusqlite::Connection::open(&path).expect("inspect");
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name = 'dv_conversions'",
                [],
                |row| row.get(0),
            )
            .expect("table count");
        assert_eq!(tables, 1);
    }
}
