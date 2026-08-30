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
//! packed form is 24 bytes a row — under 100 KB for the same film — and it
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

/// The two columns plan §2.2's ruling added, applied by migration rather than
/// written into the create above.
///
/// The create is what a v27 database already got and the migration list is
/// append-only, so editing it would leave a fresh install and an upgraded one
/// with different tables. Both backends reach this shape the same way instead.
///
/// `parameter_sets_constant` defaults to **0**, not 1. An upgraded row was
/// built by an indexer that never compared anything, and presenting a title on
/// a promise nobody made is the failure this whole mechanism exists to prevent.
/// A zero forces a rebuild, which the version bump would have forced anyway.
pub(crate) const FRAGMENT_INDEXES_PROMOTION_COLUMNS: &str = "
ALTER TABLE fragment_indexes ADD COLUMN promotion TEXT NOT NULL DEFAULT '';
ALTER TABLE fragment_indexes ADD COLUMN parameter_sets_constant INTEGER NOT NULL DEFAULT 0;";

/// Re-key the table on `(file_id, argv_fingerprint)` — PLAYBACK-CAPS-V2-PLAN
/// §4.7's M1 migration.
///
/// One file has as many indexable byte streams as there are copy pipelines a
/// real client can ask for, and a Dolby Vision title has at least two: the
/// preserved-DV envelope a Safari or Apple TV session requests, and the
/// stripped one everything else gets. `file_id INTEGER PRIMARY KEY` let only
/// one of them exist, and the background pass indexes the stripped identity —
/// so every preserved-DV session landed on `vod_index_pending` and the
/// temporary live-HLS recovery path.
///
/// A rebuild rather than an `ALTER TABLE`, because SQLite cannot change a
/// primary key in place. Rows carry over verbatim: an existing index is still
/// a true statement about the pipeline its fingerprint names, and dropping
/// them would re-run a whole library's indexing for nothing.
///
/// Its own constant, applied by both backends, rather than an edit to
/// [`FRAGMENT_INDEXES_SCHEMA`]: the migration lists are append-only, so
/// editing the create would leave a fresh install and an upgraded one on
/// different paths to the same shape. A fresh database creates the old table
/// and rebuilds it while it is still empty, which costs nothing.
pub(crate) const FRAGMENT_INDEXES_IDENTITY_KEY: &str = "
CREATE TABLE fragment_indexes_identity_keyed (
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    segplan_version   INTEGER NOT NULL,
    timescale         INTEGER NOT NULL,
    init_sha256       TEXT NOT NULL,
    fragments         INTEGER NOT NULL,
    rows_packed       BLOB NOT NULL,
    built_at_ms       INTEGER NOT NULL,
    promotion         TEXT NOT NULL DEFAULT '',
    parameter_sets_constant INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (file_id, argv_fingerprint)
) STRICT;
INSERT INTO fragment_indexes_identity_keyed (
    file_id, source_size, source_mtime, argv_fingerprint, segplan_version,
    timescale, init_sha256, fragments, rows_packed, built_at_ms, promotion,
    parameter_sets_constant
) SELECT
    file_id, source_size, source_mtime, argv_fingerprint, segplan_version,
    timescale, init_sha256, fragments, rows_packed, built_at_ms, promotion,
    parameter_sets_constant
  FROM fragment_indexes;
DROP TABLE fragment_indexes;
ALTER TABLE fragment_indexes_identity_keyed RENAME TO fragment_indexes;";

/// How many pipeline identities one file may hold an index for at once.
///
/// The set a file can legitimately be asked for at any one moment is small and
/// bounded by code — at most three, once the Profile 7 conversion adds its own
/// (PLAYBACK-CAPS-V2-PLAN §4.7). This ceiling is not that bound restated; it is
/// the backstop for identities that used to *replace* each other and now
/// accumulate. `copy_video_args` reads three inputs beyond the session's own
/// choice — the `dovi_rpu` probe, parameter-set promotion, and the file's
/// `hdr_format` label — and each is a node or scan fact that can flip without
/// the file's bytes changing. Every flip strands the fingerprints from before
/// it, and nothing else would ever collect them. Enumerating those inputs
/// gives six argv strings for one Dolby Vision file, so twelve is the double
/// headroom, not six.
///
/// The row being written is exempt: a cap that could delete its own insert
/// would report a successful build of a row that is not there, and the next
/// pass would rebuild and delete it again — an unbounded loop over a 60 GB
/// remux with no error anywhere to show for it. Everything else is evicted
/// oldest-built first, which is a deliberate second-best: `built_at_ms` is
/// stamped by [`put`] and never refreshed by a read, so a long-lived index
/// nothing has had to rebuild is the *oldest* row, not the newest. At twelve
/// the eviction should never fire at all; if it ever does, it means the
/// fingerprint churn above is real and worth a look rather than a smarter
/// policy here.
const MAX_IDENTITIES_PER_FILE: i64 = 12;

/// Bytes per packed row: dts u64, duration u32, wire bytes u32, video bytes
/// u32, class u8, 3 pad.
///
/// Padding rather than a 21-byte record because a fixed four-aligned stride
/// makes `rows_packed.len() / ROW_BYTES` an exact row count, which is the
/// cheapest possible integrity check on a blob that came off a disk.
const ROW_BYTES: usize = 24;

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
        out.extend_from_slice(&row.video_bytes.to_le_bytes());
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
        let video_bytes = u32::from_le_bytes(chunk[16..20].try_into().expect("4 bytes"));
        let class = class_from_code(chunk[20]).ok_or_else(|| {
            StoreError::Migration(format!(
                "a stored fragment index has cut class {}",
                chunk[20]
            ))
        })?;
        rows.push(IndexRow {
            dts,
            duration: u64::from(duration),
            bytes,
            video_bytes,
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

/// Store one index, keyed by the file **and** the pipeline it describes.
///
/// Two housekeeping steps ride along, both of which the old `file_id`-only key
/// got for free by overwriting:
///
/// 1. Rows describing different source bytes are dropped. A re-encoded or
///    replaced file keeps its fingerprints (they name the pipeline, not the
///    content) but changes size and mtime, so without this every edit to a
///    file would strand its whole identity set.
/// 2. The identity set is capped at [`MAX_IDENTITIES_PER_FILE`], oldest built
///    first, so a fingerprint that no code path asks for any more cannot
///    accumulate.
pub(crate) fn put(
    conn: &Connection,
    file_id: i64,
    index: &FragmentIndex,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM fragment_indexes
          WHERE file_id = ?1 AND (source_size <> ?2 OR source_mtime <> ?3)",
        params![file_id, index.source.size as i64, index.source.mtime_ms],
    )?;
    conn.execute(
        "INSERT INTO fragment_indexes (
             file_id, source_size, source_mtime, argv_fingerprint,
             segplan_version, timescale, init_sha256, fragments, rows_packed,
             built_at_ms, promotion, parameter_sets_constant
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(file_id, argv_fingerprint) DO UPDATE SET
             source_size = excluded.source_size,
             source_mtime = excluded.source_mtime,
             argv_fingerprint = excluded.argv_fingerprint,
             segplan_version = excluded.segplan_version,
             timescale = excluded.timescale,
             init_sha256 = excluded.init_sha256,
             fragments = excluded.fragments,
             rows_packed = excluded.rows_packed,
             built_at_ms = excluded.built_at_ms,
             promotion = excluded.promotion,
             parameter_sets_constant = excluded.parameter_sets_constant",
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
            serde_json::to_string(&index.promotion)
                .map_err(|error| StoreError::Migration(error.to_string()))?,
            i64::from(index.parameter_sets_constant),
        ],
    )?;
    conn.execute(
        "DELETE FROM fragment_indexes
          WHERE file_id = ?1
            AND argv_fingerprint <> ?2
            AND argv_fingerprint NOT IN (
                SELECT argv_fingerprint FROM fragment_indexes
                 WHERE file_id = ?1 AND argv_fingerprint <> ?2
                 ORDER BY built_at_ms DESC, argv_fingerprint ASC
                 LIMIT ?3
            )",
        params![
            file_id,
            index.source.argv_fingerprint,
            MAX_IDENTITIES_PER_FILE - 1
        ],
    )?;
    Ok(())
}

/// Read an index back, but only if it still describes this source.
///
/// Invalidation is by mismatch, never by deletion — the same discipline the
/// transcode cache keeps. A file that changes, or a pipeline that changes,
/// simply stops matching, so nothing has to notice and nothing can fail to.
///
/// The fingerprint selects the row and the size/mtime check still runs in
/// Rust. Since M1 a file holds one row per pipeline, so addressing the row by
/// fingerprint is the difference between "this pipeline has no index" and
/// "some other pipeline's index is in the way" — but the remaining checks stay
/// where they were, because a stale row for the *right* pipeline is still the
/// case that has to answer `None`.
pub(crate) fn get(
    conn: &Connection,
    file_id: i64,
    identity: &SourceIdentity,
) -> Result<Option<FragmentIndex>, StoreError> {
    let row = conn
        .query_row(
            "SELECT source_size, source_mtime, argv_fingerprint, segplan_version,
                    timescale, init_sha256, rows_packed, promotion,
                    parameter_sets_constant
               FROM fragment_indexes
              WHERE file_id = ?1 AND argv_fingerprint = ?2",
            params![file_id, identity.argv_fingerprint],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        size,
        mtime,
        fingerprint,
        version,
        timescale,
        init_sha256,
        packed,
        promotion,
        constant,
    )) = row
    else {
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
    let mut index = FragmentIndex::new(
        u32::try_from(timescale).unwrap_or(1),
        rows,
        init_sha256,
        stored,
    );
    // A promotion blob that will not parse is not a reason to serve an index
    // whose promotion inputs are unknown -- that is precisely the state that
    // makes the served init unreproducible. Rebuild instead.
    index.promotion = match serde_json::from_str(&promotion) {
        Ok(inputs) => inputs,
        Err(_) => return Ok(None),
    };
    index.parameter_sets_constant = constant != 0;
    Ok(Some(index))
}

/// File ids this node holds an index or a plan for, lowest first.
///
/// Both tables in one answer because the sweep that consumes it forgets from
/// both, and asking twice would sweep two different bounded windows.
pub(crate) fn vod_row_file_ids(conn: &Connection, limit: i64) -> Result<Vec<i64>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT file_id FROM (
             SELECT file_id FROM fragment_indexes
             UNION
             SELECT file_id FROM rendition_plans
         ) ORDER BY file_id LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit.max(0)], |row| row.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
                    video_bytes: 103_836,
                    class: CutClass::CleanIdr,
                },
                IndexRow {
                    dts: 28_016,
                    duration: 28_032,
                    bytes: 110_038,
                    video_bytes: 109_422,
                    class: CutClass::Dirty,
                },
                IndexRow {
                    dts: 56_048,
                    duration: 28_032,
                    bytes: 103_547,
                    video_bytes: 102_931,
                    class: CutClass::CleanCra,
                },
            ],
            "abc123",
            SourceIdentity::new(4_096, 1_700_000_000_000, "fingerprint"),
        )
    }

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sidecar");
        // Create then migrate, exactly as both real backends do -- the create
        // constant is frozen at its v27 shape on purpose.
        conn.execute_batch(FRAGMENT_INDEXES_SCHEMA).expect("schema");
        conn.execute_batch(FRAGMENT_INDEXES_PROMOTION_COLUMNS)
            .expect("promotion columns");
        conn.execute_batch(FRAGMENT_INDEXES_IDENTITY_KEY)
            .expect("identity key");
        conn
    }

    fn index_with(fingerprint: &str, size: u64, mtime_ms: i64) -> FragmentIndex {
        let mut built = index();
        built.source = SourceIdentity::new(size, mtime_ms, fingerprint);
        built
    }

    fn fingerprints(conn: &Connection, file_id: i64) -> Vec<String> {
        let mut statement = conn
            .prepare(
                "SELECT argv_fingerprint FROM fragment_indexes
                  WHERE file_id = ?1 ORDER BY argv_fingerprint",
            )
            .expect("prepare");
        let rows = statement
            .query_map(params![file_id], |row| row.get::<_, String>(0))
            .expect("query");
        rows.collect::<rusqlite::Result<Vec<_>>>().expect("collect")
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
    fn one_file_holds_an_index_per_pipeline() {
        let conn = conn();
        let stripped = index_with("stripped", 4_096, 1_700_000_000_000);
        let mut preserved = index_with("preserved", 4_096, 1_700_000_000_000);
        preserved.rows.truncate(2);

        put(&conn, 7, &stripped, 1).expect("put stripped");
        put(&conn, 7, &preserved, 2).expect("put preserved");

        assert_eq!(fingerprints(&conn, 7), vec!["preserved", "stripped"]);
        assert_eq!(
            get(&conn, 7, &stripped.source)
                .expect("get stripped")
                .expect("stripped present")
                .rows
                .len(),
            3,
            "the Dolby Vision strip and the preserved envelope are different \
             byte streams; writing one must not answer for the other"
        );
        assert_eq!(
            get(&conn, 7, &preserved.source)
                .expect("get preserved")
                .expect("preserved present")
                .rows
                .len(),
            2
        );
    }

    #[test]
    fn a_replaced_file_takes_its_whole_identity_set_with_it() {
        let conn = conn();
        put(&conn, 7, &index_with("stripped", 4_096, 1_700_000_000_000), 1).expect("put");
        put(&conn, 7, &index_with("preserved", 4_096, 1_700_000_000_000), 2).expect("put");

        // A re-encode changes the bytes but not the pipelines, so the
        // fingerprints survive and only the source identity moves. Without the
        // prune the old rows would sit there forever, matching nothing.
        put(&conn, 7, &index_with("stripped", 9_000, 1_700_000_500_000), 3).expect("put");

        assert_eq!(fingerprints(&conn, 7), vec!["stripped"]);
        assert_eq!(
            get(
                &conn,
                7,
                &SourceIdentity::new(4_096, 1_700_000_000_000, "preserved")
            )
            .expect("get"),
            None
        );
    }

    #[test]
    fn the_identity_set_cannot_grow_without_bound() {
        let conn = conn();
        let wanted = MAX_IDENTITIES_PER_FILE as usize;
        for step in 0..(wanted + 3) {
            let fingerprint = format!("pipeline-{step:02}");
            put(
                &conn,
                7,
                &index_with(&fingerprint, 4_096, 1_700_000_000_000),
                1_000 + step as i64,
            )
            .expect("put");
        }
        let held = fingerprints(&conn, 7);
        assert_eq!(held.len(), wanted);
        assert_eq!(
            held.first().map(String::as_str),
            Some("pipeline-03"),
            "eviction is oldest-built first"
        );
    }

    #[test]
    fn the_cap_never_evicts_the_row_it_was_called_for() {
        // The failure this pins: a `put` that reports success for a row the
        // cap deleted on its way out. The next pass finds nothing, rebuilds
        // the same identity -- a whole ffmpeg pass over a 60 GB remux -- and
        // deletes it again, forever, with no error anywhere. Reachable with a
        // clock that steps backwards, which `built_at_ms` is taken from.
        let conn = conn();
        let full = MAX_IDENTITIES_PER_FILE as usize;
        for step in 0..full {
            put(
                &conn,
                7,
                &index_with(&format!("incumbent-{step:02}"), 4_096, 1_700_000_000_000),
                5_000,
            )
            .expect("put");
        }
        let newcomer = index_with("newcomer", 4_096, 1_700_000_000_000);
        put(&conn, 7, &newcomer, 1).expect("put with a clock that went backwards");

        assert!(
            get(&conn, 7, &newcomer.source)
                .expect("get")
                .is_some(),
            "the row a caller was told was stored must be there to read"
        );
        assert_eq!(fingerprints(&conn, 7).len(), full);
    }

    #[test]
    fn forgetting_takes_every_pipeline_for_the_file() {
        let conn = conn();
        put(&conn, 7, &index_with("stripped", 4_096, 1_700_000_000_000), 1).expect("put");
        put(&conn, 7, &index_with("preserved", 4_096, 1_700_000_000_000), 2).expect("put");
        put(&conn, 8, &index_with("stripped", 4_096, 1_700_000_000_000), 3).expect("put other");

        assert!(forget(&conn, 7).expect("forget"));
        assert!(fingerprints(&conn, 7).is_empty());
        assert_eq!(fingerprints(&conn, 8), vec!["stripped"]);
    }

    #[test]
    fn a_file_with_several_pipelines_is_swept_once() {
        let conn = conn();
        conn.execute_batch(crate::store::renditionplan::RENDITION_PLANS_SCHEMA)
            .expect("plans schema");
        put(&conn, 7, &index_with("stripped", 4_096, 1_700_000_000_000), 1).expect("put");
        put(&conn, 7, &index_with("preserved", 4_096, 1_700_000_000_000), 2).expect("put");

        assert_eq!(
            vod_row_file_ids(&conn, 512).expect("ids"),
            vec![7],
            "the sweep window counts files, not pipelines: a Dolby Vision \
             library must not halve the number of files each tick examines"
        );
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
