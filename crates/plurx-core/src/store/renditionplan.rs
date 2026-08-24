//! Node-local storage for a rendition's **normative** segment plan.
//!
//! Companion to [`crate::store::fragindex`] (the measurements a plan is
//! computed from) — this is where the plan itself is kept, and it is kept for
//! a reason the index is not.
//!
//! An index is a description of a file: recomputing it from the same file with
//! the same pipeline gives the same rows, so losing one costs only the time to
//! rebuild it. A plan is not a description. It is a **decision**, and ledger
//! D10 makes it normative — segment boundaries are chosen once and a
//! materializing producer cuts at them for the life of the file. The playlist
//! a client fetched names those boundaries by index and duration, and it holds
//! that playlist across a restart of this daemon.
//!
//! So re-deriving the plan is not a cheaper way of getting the same answer; it
//! is a way of getting a *different* one. [`crate::fmp4::CutPolicy`] is built
//! from tuning constants — the six-second floor, the 48 MB byte ceiling, the
//! fifteen-second ceiling — and any release may change one. Nothing in
//! [`crate::segplan::SEGPLAN_VERSION`] or [`crate::segplan::SourceIdentity`]
//! covers that: the file is unchanged, the video pipeline is unchanged, the
//! index still matches, and the plan comes out cut somewhere else. Every
//! segment a client then requests by the old index names a different part of
//! the film. Persisting the plan is what makes "computed once" true rather
//! than aspirational.
//!
//! Node-local for the same reason as the index and for the same reason
//! [`crate::store::telemetry`]'s rows are: the segments a plan describes are
//! bytes on one machine's disk, and replicating the plan through Raft would
//! let one node's answer govern another node's media.
//!
//! Rows are packed rather than stored as JSON, exactly as an index's are: a
//! two-hour film is around 1,200 plan entries, and the packed form round-trips
//! integers exactly where a text encoding of a float would not.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::StoreError;
use crate::segplan::{
    PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SourceIdentity, SEGPLAN_VERSION,
};

pub(crate) const RENDITION_PLANS_SCHEMA: &str = "
CREATE TABLE rendition_plans (
    rendition_key     TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    segplan_version   INTEGER NOT NULL,
    timescale         INTEGER NOT NULL,
    target_duration   INTEGER NOT NULL,
    entries           INTEGER NOT NULL,
    entries_packed    BLOB NOT NULL,
    built_at_ms       INTEGER NOT NULL
) STRICT;
CREATE INDEX rendition_plans_by_file ON rendition_plans(file_id);";

/// Bytes per packed entry: start u64, duration u64, est_bytes u64, fragments
/// u32, kind u8, cut u8, 2 pad.
///
/// `index` is not stored. It is the entry's position in the list by
/// definition, and a stored copy is a second place for it to be wrong.
const ENTRY_BYTES: usize = 32;

fn pack(entries: &[PlanEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * ENTRY_BYTES);
    for entry in entries {
        out.extend_from_slice(&entry.start_ticks.to_le_bytes());
        out.extend_from_slice(&entry.duration_ticks.to_le_bytes());
        out.extend_from_slice(&entry.est_bytes.to_le_bytes());
        out.extend_from_slice(&entry.fragments.to_le_bytes());
        out.push(kind_code(entry.kind));
        out.push(cut_code(entry.cut));
        out.extend_from_slice(&[0, 0]);
    }
    out
}

fn unpack(blob: &[u8]) -> Result<Vec<PlanEntry>, StoreError> {
    if !blob.len().is_multiple_of(ENTRY_BYTES) {
        return Err(StoreError::Migration(format!(
            "a stored segment plan is {} bytes, not a whole number of \
             {ENTRY_BYTES}-byte entries",
            blob.len()
        )));
    }
    let mut entries = Vec::with_capacity(blob.len() / ENTRY_BYTES);
    for (position, chunk) in blob.chunks_exact(ENTRY_BYTES).enumerate() {
        let start_ticks = u64::from_le_bytes(chunk[0..8].try_into().expect("8 bytes"));
        let duration_ticks = u64::from_le_bytes(chunk[8..16].try_into().expect("8 bytes"));
        let est_bytes = u64::from_le_bytes(chunk[16..24].try_into().expect("8 bytes"));
        let fragments = u32::from_le_bytes(chunk[24..28].try_into().expect("4 bytes"));
        let kind = kind_from_code(chunk[28]).ok_or_else(|| {
            StoreError::Migration(format!(
                "a stored segment plan has entry kind {}",
                chunk[28]
            ))
        })?;
        let cut = cut_from_code(chunk[29]).ok_or_else(|| {
            StoreError::Migration(format!(
                "a stored segment plan has cut reason {}",
                chunk[29]
            ))
        })?;
        entries.push(PlanEntry {
            index: u32::try_from(position).unwrap_or(u32::MAX),
            kind,
            start_ticks,
            duration_ticks,
            est_bytes,
            cut,
            fragments,
        });
    }
    Ok(entries)
}

fn kind_code(kind: PlanEntryKind) -> u8 {
    match kind {
        PlanEntryKind::Video => 1,
        PlanEntryKind::AudioTail => 2,
    }
}

fn kind_from_code(code: u8) -> Option<PlanEntryKind> {
    match code {
        1 => Some(PlanEntryKind::Video),
        2 => Some(PlanEntryKind::AudioTail),
        _ => None,
    }
}

fn cut_code(cut: PlanCut) -> u8 {
    match cut {
        PlanCut::Clean => 1,
        PlanCut::ByteCeiling => 2,
        PlanCut::TimeCeiling => 3,
        PlanCut::EndOfStream => 4,
    }
}

fn cut_from_code(code: u8) -> Option<PlanCut> {
    match code {
        1 => Some(PlanCut::Clean),
        2 => Some(PlanCut::ByteCeiling),
        3 => Some(PlanCut::TimeCeiling),
        4 => Some(PlanCut::EndOfStream),
        _ => None,
    }
}

/// Store a plan under a rendition key, if one is not already stored.
///
/// **Deliberately not an upsert.** Every other node-local table here takes the
/// newest answer, because every other one holds a measurement. This holds a
/// decision that a client may already have fetched a playlist for, so the
/// first plan written under a key is the plan, and a second attempt is a
/// no-op rather than a silent re-cut. A rendition whose plan genuinely must
/// change gets a new key — that is what a key is for.
///
/// Returns whether this call is the one that stored it.
///
/// An empty plan is refused rather than stored. `get` treats an empty entry
/// list as a miss -- a playlist with no segments is not a rendition -- so
/// storing one strands the key forever: every read misses, and every later
/// write is declined because the row exists.
pub(crate) fn put_if_absent(
    conn: &Connection,
    rendition_key: &str,
    file_id: i64,
    plan: &SegmentPlan,
    source: &SourceIdentity,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if plan.entries.is_empty() {
        return Err(StoreError::Migration(format!(
            "refusing to store an empty segment plan under {rendition_key}: a \
             stored empty plan reads back as a miss and cannot be replaced"
        )));
    }
    let affected = conn.execute(
        "INSERT INTO rendition_plans (
             rendition_key, file_id, source_size, source_mtime,
             argv_fingerprint, segplan_version, timescale, target_duration,
             entries, entries_packed, built_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(rendition_key) DO NOTHING",
        params![
            rendition_key,
            file_id,
            source.size as i64,
            source.mtime_ms,
            source.argv_fingerprint,
            i64::from(plan.version),
            i64::from(plan.timescale),
            i64::from(plan.target_duration),
            plan.entries.len() as i64,
            pack(&plan.entries),
            now_ms,
        ],
    )?;
    Ok(affected > 0)
}

/// Read a plan back, but only if it still describes this source.
///
/// Invalidation is by mismatch, never by deletion, the same discipline the
/// index keeps: a changed file or a changed video pipeline simply stops
/// matching, so nothing has to notice and nothing can fail to.
///
/// A version mismatch is also a miss. A plan written by a binary that packed
/// entries differently is not readable as this one's, and answering with a
/// misread plan would put a client's segment requests somewhere arbitrary.
pub(crate) fn get(
    conn: &Connection,
    rendition_key: &str,
    identity: &SourceIdentity,
) -> Result<Option<SegmentPlan>, StoreError> {
    let row = conn
        .query_row(
            "SELECT source_size, source_mtime, argv_fingerprint, segplan_version,
                    timescale, target_duration, entries, entries_packed
               FROM rendition_plans
              WHERE rendition_key = ?1",
            params![rendition_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((size, mtime, fingerprint, version, timescale, target, declared_entries, packed)) =
        row
    else {
        return Ok(None);
    };
    if version != i64::from(SEGPLAN_VERSION) {
        return Ok(None);
    }
    if !SourceIdentity::new(size.max(0) as u64, mtime, fingerprint).matches(identity) {
        return Ok(None);
    }
    let entries = unpack(&packed)?;
    // The count column earns its place here. `unpack` only rejects a blob that
    // is not a whole number of entries, so a blob truncated on an entry
    // boundary -- 1,200 stored, 1,100 readable -- comes back as a shorter
    // plan, and a shorter plan renders a VOD playlist with an ENDLIST for a
    // film that ends early. Nothing downstream could tell.
    if entries.len() as i64 != declared_entries {
        return Err(StoreError::Migration(format!(
            "a stored segment plan declares {declared_entries} entries and \
             carries {}",
            entries.len()
        )));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(SegmentPlan {
        version: SEGPLAN_VERSION,
        timescale: u32::try_from(timescale).unwrap_or(1).max(1),
        entries,
        target_duration: u32::try_from(target).unwrap_or(0),
    }))
}

/// Drop every plan for a file. The caller that deletes a file's media owns
/// this; nothing here expires a plan on its own, because a plan outliving its
/// media costs a row and a plan dying under a live playlist costs a session.
pub(crate) fn forget_file(conn: &Connection, file_id: i64) -> Result<usize, StoreError> {
    let affected = conn.execute(
        "DELETE FROM rendition_plans WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> SourceIdentity {
        SourceIdentity::new(4_096, 1_700_000_000_000, "fingerprint")
    }

    fn entry(
        index: u32,
        start: u64,
        duration: u64,
        kind: PlanEntryKind,
        cut: PlanCut,
    ) -> PlanEntry {
        PlanEntry {
            index,
            kind,
            start_ticks: start,
            duration_ticks: duration,
            est_bytes: 48_000_000 + u64::from(index),
            cut,
            fragments: if kind == PlanEntryKind::Video { 4 } else { 0 },
        }
    }

    fn plan() -> SegmentPlan {
        SegmentPlan {
            version: SEGPLAN_VERSION,
            timescale: 16_000,
            entries: vec![
                entry(0, 0, 112_128, PlanEntryKind::Video, PlanCut::Clean),
                entry(
                    1,
                    112_128,
                    112_064,
                    PlanEntryKind::Video,
                    PlanCut::ByteCeiling,
                ),
                entry(
                    2,
                    224_192,
                    96_000,
                    PlanEntryKind::Video,
                    PlanCut::EndOfStream,
                ),
                entry(
                    3,
                    320_192,
                    48_000,
                    PlanEntryKind::AudioTail,
                    PlanCut::EndOfStream,
                ),
            ],
            target_duration: 15,
        }
    }

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory");
        conn.execute_batch(RENDITION_PLANS_SCHEMA).expect("schema");
        conn
    }

    #[test]
    fn a_plan_round_trips_entry_for_entry() {
        let conn = conn();
        assert!(put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("put"));
        let read = get(&conn, "rk", &identity())
            .expect("get")
            .expect("a plan is stored");
        assert_eq!(read, plan(), "a stored plan must come back identical");
    }

    #[test]
    fn a_second_plan_under_one_key_never_replaces_the_first() {
        // The boundaries are already in a playlist somebody fetched. A tuning
        // constant that moved between releases must not be able to re-cut a
        // rendition under the key that names it.
        let conn = conn();
        assert!(put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("first"));

        let mut recut = plan();
        recut.entries[0].duration_ticks = 999;
        recut.entries[1].start_ticks = 999;
        assert!(
            !put_if_absent(&conn, "rk", 7, &recut, &identity(), 2_000).expect("second"),
            "a second plan under one key is refused, not applied"
        );

        let read = get(&conn, "rk", &identity()).expect("get").expect("stored");
        assert_eq!(read, plan(), "the first plan is still the plan");
    }

    #[test]
    fn a_changed_source_stops_matching_rather_than_answering_wrongly() {
        let conn = conn();
        put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("put");

        let repacked = SourceIdentity::new(4_096, 1_700_000_000_000, "a different pipeline");
        assert!(get(&conn, "rk", &repacked).expect("get").is_none());

        let edited = SourceIdentity::new(8_192, 1_700_000_000_000, "fingerprint");
        assert!(get(&conn, "rk", &edited).expect("get").is_none());

        let touched = SourceIdentity::new(4_096, 1_800_000_000_000, "fingerprint");
        assert!(get(&conn, "rk", &touched).expect("get").is_none());
    }

    #[test]
    fn a_plan_from_another_version_is_a_miss_not_a_misread() {
        let conn = conn();
        put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("put");
        conn.execute(
            "UPDATE rendition_plans SET segplan_version = ?1",
            params![i64::from(SEGPLAN_VERSION) + 1],
        )
        .expect("bump");
        assert!(get(&conn, "rk", &identity()).expect("get").is_none());
    }

    #[test]
    fn an_empty_plan_is_refused_rather_than_stranding_the_key() {
        // `get` treats an empty entry list as a miss, so a stored empty plan
        // is a key that reads as absent forever and refuses every replacement.
        let conn = conn();
        let empty = SegmentPlan {
            version: SEGPLAN_VERSION,
            timescale: 16_000,
            entries: Vec::new(),
            target_duration: 0,
        };
        assert!(put_if_absent(&conn, "rk", 7, &empty, &identity(), 1_000).is_err());
        // And the key is still free for a real plan.
        assert!(put_if_absent(&conn, "rk", 7, &plan(), &identity(), 2_000).expect("put"));
        assert!(get(&conn, "rk", &identity()).expect("get").is_some());
    }

    #[test]
    fn a_plan_truncated_on_an_entry_boundary_is_caught_by_the_count() {
        // The dangerous truncation: a whole number of entries, so the blob's
        // length tells you nothing. A short plan renders a VOD playlist with
        // an ENDLIST for a film that ends early, and nothing downstream could
        // tell.
        let conn = conn();
        put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("put");
        conn.execute(
            "UPDATE rendition_plans SET entries_packed = ?1",
            params![pack(&plan().entries[..2])],
        )
        .expect("truncate on a boundary");
        let error = get(&conn, "rk", &identity()).expect_err("a short plan is refused");
        assert!(error.to_string().contains("declares 4"), "{error}");
    }

    #[test]
    fn a_truncated_blob_is_an_error_rather_than_a_short_plan() {
        // A plan with entries missing off the end is a playlist that stops
        // partway through a film, which is worse than refusing to serve it.
        let conn = conn();
        put_if_absent(&conn, "rk", 7, &plan(), &identity(), 1_000).expect("put");
        conn.execute(
            "UPDATE rendition_plans SET entries_packed = ?1",
            params![vec![0u8; ENTRY_BYTES * 2 + 7]],
        )
        .expect("truncate");
        assert!(get(&conn, "rk", &identity()).is_err());
    }

    #[test]
    fn forgetting_a_file_drops_every_rendition_of_it() {
        let conn = conn();
        put_if_absent(&conn, "rk-1080", 7, &plan(), &identity(), 1_000).expect("put");
        put_if_absent(&conn, "rk-720", 7, &plan(), &identity(), 1_000).expect("put");
        put_if_absent(&conn, "rk-other", 8, &plan(), &identity(), 1_000).expect("put");

        assert_eq!(forget_file(&conn, 7).expect("forget"), 2);
        assert!(get(&conn, "rk-1080", &identity()).expect("get").is_none());
        assert!(get(&conn, "rk-other", &identity()).expect("get").is_some());
    }
}
