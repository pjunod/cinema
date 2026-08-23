//! Node-local fragment-index storage, shared by both durable backends.
//!
//! Companion to [`crate::segplan`] (what an index means) — this is where one
//! is kept. Single-node SQLite carries the schema as a migration; a hiqlite
//! voter owns the same table in its per-voter sidecar, because an index
//! describes what one machine's ffmpeg produced from one machine's copy of a
//! file and replicating it through Raft would let one node's answer govern
//! another node's bytes. That is the same rule as
//! [`crate::store::telemetry`]'s, for a different reason.
//!
//! Rows are stored packed rather than as JSON. A two-hour film with ~1.75 s
//! GOPs is around 4,100 fragments; JSON of that is a quarter of a megabyte per
//! title, and a library-sized sidecar of those is measured in gigabytes. The
//! packed form is 20 bytes a row — under 90 KB for the same film — and it
//! round-trips exactly, which a float-bearing text encoding would not.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::StoreError;
use crate::fmp4::CutClass;
use crate::segplan::{FragmentIndex, IndexRow, SourceIdentity, SEGPLAN_VERSION};

pub(crate) const FRAGMENT_INDEXES_SCHEMA: &str = "
CREATE TABLE fragment_indexes (
    file_id           INTEGER PRIMARY KEY,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    segplan_version   INTEGER NOT NULL,
    timescale         INTEGER NOT NULL,
    init_sha256       TEXT NOT NULL,
    fragments         INTEGER NOT NULL,
    rows_packed       BLOB NOT NULL,
    built_at_ms       INTEGER NOT NULL
) STRICT;";

/// Bytes per packed row: dts u64, duration u32, bytes u32, class u8, 3 pad.
///
/// Padding rather than a 17-byte record because a fixed power-of-four stride
/// makes `rows_packed.len() / ROW_BYTES` an exact row count, which is the
/// cheapest possible integrity check on a blob that came off a disk.
const ROW_BYTES: usize = 20;

fn pack(rows: &[IndexRow]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * ROW_BYTES);
    for row in rows {
        out.extend_from_slice(&row.dts.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(row.duration)
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&row.bytes.to_le_bytes());
        out.push(class_code(row.class));
        out.extend_from_slice(&[0, 0, 0]);
    }
    out
}

fn unpack(blob: &[u8]) -> Result<Vec<IndexRow>, StoreError> {
    if !blob.len().is_multiple_of(ROW_BYTES) {
        return Err(StoreError::Migration(format!(
            "a stored fragment index is {} bytes, not a whole number of \
             {ROW_BYTES}-byte rows",
            blob.len()
        )));
    }
    let mut rows = Vec::with_capacity(blob.len() / ROW_BYTES);
    for chunk in blob.chunks_exact(ROW_BYTES) {
        let dts = u64::from_le_bytes(chunk[0..8].try_into().expect("8 bytes"));
        let duration = u32::from_le_bytes(chunk[8..12].try_into().expect("4 bytes"));
        let bytes = u32::from_le_bytes(chunk[12..16].try_into().expect("4 bytes"));
        let class = class_from_code(chunk[16]).ok_or_else(|| {
            StoreError::Migration(format!(
                "a stored fragment index has cut class {}",
                chunk[16]
            ))
        })?;
        rows.push(IndexRow {
            dts,
            duration: u64::from(duration),
            bytes,
            class,
        });
    }
    Ok(rows)
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

pub(crate) fn put(
    conn: &Connection,
    file_id: i64,
    index: &FragmentIndex,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO fragment_indexes (
             file_id, source_size, source_mtime, argv_fingerprint,
             segplan_version, timescale, init_sha256, fragments, rows_packed,
             built_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(file_id) DO UPDATE SET
             source_size = excluded.source_size,
             source_mtime = excluded.source_mtime,
             argv_fingerprint = excluded.argv_fingerprint,
             segplan_version = excluded.segplan_version,
             timescale = excluded.timescale,
             init_sha256 = excluded.init_sha256,
             fragments = excluded.fragments,
             rows_packed = excluded.rows_packed,
             built_at_ms = excluded.built_at_ms",
        params![
            file_id,
            index.source.size as i64,
            index.source.mtime_ms,
            index.source.argv_fingerprint,
            i64::from(index.version),
            i64::from(index.timescale),
            index.init_sha256,
            index.rows.len() as i64,
            pack(&index.rows),
            now_ms,
        ],
    )?;
    Ok(())
}

/// Read an index back, but only if it still describes this source.
///
/// Invalidation is by mismatch, never by deletion — the same discipline the
/// transcode cache keeps. A file that changes, or a pipeline that changes,
/// simply stops matching, so nothing has to notice and nothing can fail to.
pub(crate) fn get(
    conn: &Connection,
    file_id: i64,
    identity: &SourceIdentity,
) -> Result<Option<FragmentIndex>, StoreError> {
    let row = conn
        .query_row(
            "SELECT source_size, source_mtime, argv_fingerprint, segplan_version,
                    timescale, init_sha256, rows_packed
               FROM fragment_indexes
              WHERE file_id = ?1",
            params![file_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((size, mtime, fingerprint, version, timescale, init_sha256, packed)) = row else {
        return Ok(None);
    };
    if version != i64::from(SEGPLAN_VERSION) {
        return Ok(None);
    }
    let stored = SourceIdentity::new(size.max(0) as u64, mtime, fingerprint);
    if !stored.matches(identity) {
        return Ok(None);
    }
    let rows = unpack(&packed)?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(FragmentIndex::new(
        u32::try_from(timescale).unwrap_or(1),
        rows,
        init_sha256,
        stored,
    )))
}

pub(crate) fn forget(conn: &Connection, file_id: i64) -> Result<bool, StoreError> {
    let affected = conn.execute(
        "DELETE FROM fragment_indexes WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FragmentIndex {
        FragmentIndex::new(
            16_000,
            vec![
                IndexRow {
                    dts: 0,
                    duration: 28_016,
                    bytes: 104_452,
                    class: CutClass::CleanIdr,
                },
                IndexRow {
                    dts: 28_016,
                    duration: 28_032,
                    bytes: 110_038,
                    class: CutClass::Dirty,
                },
                IndexRow {
                    dts: 56_048,
                    duration: 28_032,
                    bytes: 103_547,
                    class: CutClass::CleanCra,
                },
            ],
            "abc123",
            SourceIdentity::new(4_096, 1_700_000_000_000, "fingerprint"),
        )
    }

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sidecar");
        conn.execute_batch(FRAGMENT_INDEXES_SCHEMA).expect("schema");
        conn
    }

    #[test]
    fn an_index_round_trips_exactly() {
        let conn = conn();
        let stored = index();
        put(&conn, 7, &stored, 1).expect("put");
        let read = get(&conn, 7, &stored.source)
            .expect("get")
            .expect("present");
        assert_eq!(read, stored);
    }

    #[test]
    fn the_packed_form_is_twenty_bytes_a_row() {
        let packed = pack(&index().rows);
        assert_eq!(packed.len(), 3 * ROW_BYTES);
        assert_eq!(unpack(&packed).expect("unpack"), index().rows);
    }

    #[test]
    fn a_changed_source_stops_matching_rather_than_needing_deletion() {
        let conn = conn();
        let stored = index();
        put(&conn, 7, &stored, 1).expect("put");

        let resized = SourceIdentity::new(8_192, 1_700_000_000_000, "fingerprint");
        assert_eq!(get(&conn, 7, &resized).expect("get"), None);

        let touched = SourceIdentity::new(4_096, 1_700_000_000_001, "fingerprint");
        assert_eq!(get(&conn, 7, &touched).expect("get"), None);
    }

    #[test]
    fn a_changed_video_pipeline_stops_matching() {
        let conn = conn();
        let stored = index();
        put(&conn, 7, &stored, 1).expect("put");
        let upgraded = SourceIdentity::new(4_096, 1_700_000_000_000, "different-ffmpeg");
        assert_eq!(
            get(&conn, 7, &upgraded).expect("get"),
            None,
            "an index whose byte counts describe another pipeline is not stale, \
             it is wrong: the landing matcher compares exactly those numbers"
        );
    }

    #[test]
    fn writing_the_same_file_twice_replaces_rather_than_duplicates() {
        let conn = conn();
        let mut stored = index();
        put(&conn, 7, &stored, 1).expect("put");
        stored.rows.truncate(2);
        put(&conn, 7, &stored, 2).expect("put again");
        let read = get(&conn, 7, &stored.source)
            .expect("get")
            .expect("present");
        assert_eq!(read.rows.len(), 2);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fragment_indexes", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn forgetting_reports_whether_anything_was_there() {
        let conn = conn();
        assert!(!forget(&conn, 7).expect("forget missing"));
        put(&conn, 7, &index(), 1).expect("put");
        assert!(forget(&conn, 7).expect("forget present"));
        assert_eq!(get(&conn, 7, &index().source).expect("get"), None);
    }

    #[test]
    fn a_torn_blob_is_refused_rather_than_half_read() {
        assert!(unpack(&[0u8; ROW_BYTES + 3]).is_err());
        assert!(
            unpack(&[0u8; ROW_BYTES]).is_err(),
            "cut class 0 is not a class"
        );
    }
}
