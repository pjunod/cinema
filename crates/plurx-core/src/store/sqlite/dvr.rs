//! SQLite persistence for recording rules, scheduled airings and reminders.
//!
//! The three invariants named in [`crate::store::DvrStore`] are the reason
//! this file is shaped the way it is: an airing is inserted or reported and
//! never updated, every owner-loop write is conditional on the state and the
//! tuner generation it was planned under, and the column sets that progress,
//! a stop request and a state transition write never overlap.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::{conversion_err, SqliteStore};
use crate::dvr::{
    DvrInsertOutcome, DvrKeepMode, DvrMatchMode, DvrOrigin, DvrRecording, DvrReminder,
    DvrReminderState, DvrRule, DvrState, DvrStatePatch, DVR_RECORDINGS_LIST_PAGE,
    DVR_REMINDERS_PER_USER_MAX, DVR_RULES_MAX,
};
use crate::error::StoreError;
use crate::store::DvrStore;

const RULE_COLS: &str = "id, owner_user_id, priority, name, match_mode, match_value, \
    channel_id, new_only, keep_mode, keep_value, pad_start_s, pad_end_s, enabled, \
    created_at_ms, updated_at_ms";

const RECORDING_COLS: &str = "id, origin, rule_id, requested_by_user_id, channel_id, \
    guide_number, channel_name, airing_start, airing_end, capture_start, capture_end, title, \
    episode_title, episode, synopsis, image_url, original_air_date, series_id, programme_id, \
    state, state_reason, attempt, gap_s, late_start_s, tuner_owner_node_id, path, bytes, \
    last_progress_ms, stop_requested_at_ms, stop_requested_by_user_id, item_id, file_id, \
    started_at_ms, finished_at_ms, stopped_by_user_id, created_at_ms, updated_at_ms";

const REMINDER_COLS: &str = "id, user_id, channel_id, guide_number, airing_start, airing_end, \
    title, lead_s, state, fired_at_ms, acked_at_ms, created_at_ms, updated_at_ms";

/// The `state` clause every write that re-points a rule row carries.
///
/// A row that has reached the tuner is history: the rule that owns it may be
/// edited, deleted or re-matched, and none of that is allowed to rewrite what
/// a capture already recorded.
const PENDING_STATES: &str = "('scheduled', 'conflict', 'withdrawn', 'stale')";

/// The tuner-generation fence, as a fragment of the caller's own statement.
///
/// A settings change lands between the owner loop reading its configuration
/// and writing the row it planned from that configuration, so the comparison
/// has to happen inside the UPDATE rather than as a read before it — one
/// operation, or the window reopens. A missing settings row reads as
/// generation 0, the same answer `live_tv` config loading gives it, so an
/// unconfigured server is fenced at 0 rather than failing every write.
const GENERATION_FENCE: &str = "(?6 IS NULL OR COALESCE((SELECT CAST(value AS INTEGER) \
     FROM settings WHERE key = 'live_tv.config_generation'), 0) = ?6)";

fn rule_from_row(row: &Row<'_>) -> rusqlite::Result<DvrRule> {
    let match_mode_raw: String = row.get(4)?;
    let match_mode = DvrMatchMode::parse(&match_mode_raw)
        .ok_or_else(|| conversion_err(4, format!("unknown dvr match mode `{match_mode_raw}`")))?;
    let keep_mode_raw: String = row.get(8)?;
    let keep_mode = DvrKeepMode::parse(&keep_mode_raw)
        .ok_or_else(|| conversion_err(8, format!("unknown dvr keep mode `{keep_mode_raw}`")))?;
    Ok(DvrRule {
        id: row.get(0)?,
        owner_user_id: row.get(1)?,
        priority: row.get(2)?,
        name: row.get(3)?,
        match_mode,
        match_value: row.get(5)?,
        channel_id: row.get(6)?,
        new_only: row.get::<_, i64>(7)? != 0,
        keep_mode,
        keep_value: row.get(9)?,
        pad_start_s: row.get(10)?,
        pad_end_s: row.get(11)?,
        enabled: row.get::<_, i64>(12)? != 0,
        created_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
    })
}

fn recording_from_row(row: &Row<'_>) -> rusqlite::Result<DvrRecording> {
    let origin_raw: String = row.get(1)?;
    let origin = DvrOrigin::parse(&origin_raw)
        .ok_or_else(|| conversion_err(1, format!("unknown dvr origin `{origin_raw}`")))?;
    let state_raw: String = row.get(19)?;
    let state = DvrState::parse(&state_raw)
        .ok_or_else(|| conversion_err(19, format!("unknown dvr state `{state_raw}`")))?;
    Ok(DvrRecording {
        id: row.get(0)?,
        origin,
        rule_id: row.get(2)?,
        requested_by_user_id: row.get(3)?,
        channel_id: row.get(4)?,
        guide_number: row.get(5)?,
        channel_name: row.get(6)?,
        airing_start: row.get(7)?,
        airing_end: row.get(8)?,
        capture_start: row.get(9)?,
        capture_end: row.get(10)?,
        title: row.get(11)?,
        episode_title: row.get(12)?,
        episode: row.get(13)?,
        synopsis: row.get(14)?,
        image_url: row.get(15)?,
        original_air_date: row.get(16)?,
        series_id: row.get(17)?,
        programme_id: row.get(18)?,
        state,
        state_reason: row.get(20)?,
        attempt: row.get(21)?,
        gap_s: row.get(22)?,
        late_start_s: row.get(23)?,
        tuner_owner_node_id: row.get(24)?,
        path: row.get(25)?,
        bytes: row.get(26)?,
        last_progress_ms: row.get(27)?,
        stop_requested_at_ms: row.get(28)?,
        stop_requested_by_user_id: row.get(29)?,
        item_id: row.get(30)?,
        file_id: row.get(31)?,
        started_at_ms: row.get(32)?,
        finished_at_ms: row.get(33)?,
        stopped_by_user_id: row.get(34)?,
        created_at_ms: row.get(35)?,
        updated_at_ms: row.get(36)?,
    })
}

fn reminder_from_row(row: &Row<'_>) -> rusqlite::Result<DvrReminder> {
    let state_raw: String = row.get(8)?;
    let state = DvrReminderState::parse(&state_raw)
        .ok_or_else(|| conversion_err(8, format!("unknown dvr reminder state `{state_raw}`")))?;
    Ok(DvrReminder {
        id: row.get(0)?,
        user_id: row.get(1)?,
        channel_id: row.get(2)?,
        guide_number: row.get(3)?,
        airing_start: row.get(4)?,
        airing_end: row.get(5)?,
        title: row.get(6)?,
        lead_s: row.get(7)?,
        state,
        fired_at_ms: row.get(9)?,
        acked_at_ms: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

/// A set of states, as the JSON array `json_each` expands.
///
/// One prepared statement then serves every arity, which is what keeps these
/// queries free of a placeholder list assembled by string concatenation — the
/// one construction in a SQL builder that can be made to carry a caller's
/// value as syntax.
fn state_json(states: &[DvrState]) -> Result<String, StoreError> {
    let wire = states
        .iter()
        .map(|state| state.as_str())
        .collect::<Vec<_>>();
    serde_json::to_string(&wire).map_err(|error| StoreError::Database(error.to_string()))
}

fn load_recording(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<DvrRecording>, StoreError> {
    Ok(conn
        .query_row(
            &format!("SELECT {RECORDING_COLS} FROM dvr_recordings WHERE id = ?1"),
            [id],
            recording_from_row,
        )
        .optional()?)
}

#[async_trait]
impl DvrStore for SqliteStore {
    async fn list_dvr_rules(&self) -> Result<Vec<crate::dvr::DvrRule>, StoreError> {
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {RULE_COLS} FROM dvr_rules ORDER BY priority"
            ))?;
            let rows = statement.query_map([], rule_from_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn get_dvr_rule(&self, id: &str) -> Result<Option<crate::dvr::DvrRule>, StoreError> {
        let id = id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {RULE_COLS} FROM dvr_rules WHERE id = ?1"),
                    [id],
                    rule_from_row,
                )
                .optional()?)
        })
        .await
    }

    /// The ceiling applies to new rules only. Editing one of two hundred rules
    /// has to keep working, or an operator who reaches the limit can no longer
    /// disable the rule that is filling the disk.
    async fn put_dvr_rule(&self, rule: &crate::dvr::DvrRule) -> Result<bool, StoreError> {
        let rule = rule.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing: i64 = tx.query_row(
                "SELECT COUNT(*) FROM dvr_rules WHERE id = ?1",
                [rule.id.as_str()],
                |row| row.get(0),
            )?;
            if existing == 0 {
                let total: i64 =
                    tx.query_row("SELECT COUNT(*) FROM dvr_rules", [], |row| row.get(0))?;
                if total >= DVR_RULES_MAX {
                    return Ok(false);
                }
            }
            // `created_at_ms` is deliberately absent from the update list: when
            // a rule was first written is a fact about the past, not a field a
            // later edit gets to restate.
            tx.execute(
                &format!(
                    "INSERT INTO dvr_rules ({RULE_COLS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                     ON CONFLICT(id) DO UPDATE SET \
                        owner_user_id = excluded.owner_user_id, priority = excluded.priority, \
                        name = excluded.name, match_mode = excluded.match_mode, \
                        match_value = excluded.match_value, channel_id = excluded.channel_id, \
                        new_only = excluded.new_only, keep_mode = excluded.keep_mode, \
                        keep_value = excluded.keep_value, pad_start_s = excluded.pad_start_s, \
                        pad_end_s = excluded.pad_end_s, enabled = excluded.enabled, \
                        updated_at_ms = excluded.updated_at_ms"
                ),
                params![
                    rule.id,
                    rule.owner_user_id,
                    rule.priority,
                    rule.name,
                    rule.match_mode.as_str(),
                    rule.match_value,
                    rule.channel_id,
                    rule.new_only,
                    rule.keep_mode.as_str(),
                    rule.keep_value,
                    rule.pad_start_s,
                    rule.pad_end_s,
                    rule.enabled,
                    rule.created_at_ms,
                    rule.updated_at_ms,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn delete_dvr_rule(&self, id: &str) -> Result<bool, StoreError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute("DELETE FROM dvr_rules WHERE id = ?1", [id])?;
            Ok(changed == 1)
        })
        .await
    }

    /// Renumbered in two passes, because `priority` is unique.
    ///
    /// Walking the list and assigning 1, 2, 3 in place collides the moment two
    /// rules swap places: the first write lands on a number the second rule
    /// still holds. Parking every listed rule in a disjoint negative range
    /// first empties the whole positive range, so the second pass cannot meet
    /// a number that is still occupied, and the transaction means no reader
    /// ever sees the negative ordering.
    async fn reorder_dvr_rules(
        &self,
        ids_in_priority_order: &[String],
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let ids = ids_in_priority_order.to_vec();
        let listed =
            serde_json::to_string(&ids).map_err(|error| StoreError::Database(error.to_string()))?;
        let distinct = ids.iter().collect::<std::collections::BTreeSet<_>>().len();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM dvr_rules", [], |row| row.get(0))?;
            let named: i64 = tx.query_row(
                "SELECT COUNT(*) FROM dvr_rules WHERE id IN (SELECT value FROM json_each(?1))",
                [listed.as_str()],
                |row| row.get(0),
            )?;
            // A partial order is not an order: a list that omits a rule, names
            // one twice or names one that is gone would leave the scheduler
            // breaking ties by row order again.
            if distinct != ids.len() || total != ids.len() as i64 || named != ids.len() as i64 {
                return Ok(false);
            }
            for (index, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE dvr_rules SET priority = ?1 WHERE id = ?2",
                    params![-1 - index as i64, id],
                )?;
            }
            for (index, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE dvr_rules SET priority = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    params![index as i64 + 1, now_ms, id],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    /// Reads the airing and inserts it in one transaction, and writes nothing
    /// at all when the row is already there.
    ///
    /// Expansion runs every tick against the same guide, so an upsert here
    /// would re-arm a recording the viewer cancelled fifteen seconds ago, and
    /// an `ON CONFLICT DO UPDATE` would do it while reporting success. The
    /// unique index on `(channel_id, airing_start)` remains the backstop: a
    /// racing insert fails loudly rather than producing a second row for one
    /// broadcast.
    async fn insert_dvr_airing_if_absent(
        &self,
        row: &crate::dvr::DvrRecording,
    ) -> Result<crate::dvr::DvrInsertOutcome, StoreError> {
        let row = row.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT state FROM dvr_recordings WHERE channel_id = ?1 AND airing_start = ?2",
                    params![row.channel_id, row.airing_start],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(state_raw) = existing {
                let state = DvrState::parse(&state_raw).ok_or_else(|| {
                    StoreError::Database(format!("unknown dvr state `{state_raw}`"))
                })?;
                return Ok(DvrInsertOutcome::Exists(state));
            }
            tx.execute(
                &format!(
                    "INSERT INTO dvr_recordings ({RECORDING_COLS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                      ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, \
                      ?31, ?32, ?33, ?34, ?35, ?36, ?37)"
                ),
                params![
                    row.id,
                    row.origin.as_str(),
                    row.rule_id,
                    row.requested_by_user_id,
                    row.channel_id,
                    row.guide_number,
                    row.channel_name,
                    row.airing_start,
                    row.airing_end,
                    row.capture_start,
                    row.capture_end,
                    row.title,
                    row.episode_title,
                    row.episode,
                    row.synopsis,
                    row.image_url,
                    row.original_air_date,
                    row.series_id,
                    row.programme_id,
                    row.state.as_str(),
                    row.state_reason,
                    row.attempt,
                    row.gap_s,
                    row.late_start_s,
                    row.tuner_owner_node_id,
                    row.path,
                    row.bytes,
                    row.last_progress_ms,
                    row.stop_requested_at_ms,
                    row.stop_requested_by_user_id,
                    row.item_id,
                    row.file_id,
                    row.started_at_ms,
                    row.finished_at_ms,
                    row.stopped_by_user_id,
                    row.created_at_ms,
                    row.updated_at_ms,
                ],
            )?;
            tx.commit()?;
            Ok(DvrInsertOutcome::Inserted)
        })
        .await
    }

    async fn get_dvr_recording(
        &self,
        id: &str,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError> {
        let id = id.to_owned();
        self.with_read(move |conn| load_recording(conn, &id)).await
    }

    async fn list_dvr_recordings(
        &self,
        filter: &crate::dvr::DvrRecordingFilter,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<crate::dvr::DvrRecording>, StoreError> {
        let after_id = after_id.unwrap_or_default().to_owned();
        let limit = limit.clamp(1, DVR_RECORDINGS_LIST_PAGE);
        let states = state_json(&filter.states)?;
        // An explicit list is honoured as written — naming `deleted` is how a
        // caller asks for history — and `include_deleted` only decides what an
        // empty list means.
        let state_clause = if filter.states.is_empty() {
            if filter.include_deleted {
                "1 = 1"
            } else {
                "state <> 'deleted'"
            }
        } else {
            "state IN (SELECT value FROM json_each(?2))"
        };
        let before = filter.before_capture_start;
        self.with_read(move |conn| {
            // `id` is the cursor and the order, so a page boundary cannot be
            // moved by a row the owner loop rewrites between two requests.
            let sql = format!(
                "SELECT {RECORDING_COLS} FROM dvr_recordings \
                 WHERE id > ?1 AND {state_clause} AND (?3 IS NULL OR capture_start < ?3) \
                 ORDER BY id LIMIT ?4"
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement
                .query_map(params![after_id, states, before, limit], recording_from_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn list_dvr_recordings_in(
        &self,
        states: &[crate::dvr::DvrState],
    ) -> Result<Vec<crate::dvr::DvrRecording>, StoreError> {
        let states = state_json(states)?;
        self.with_read(move |conn| {
            // Capture order, because the owner loop's tick walks the schedule
            // forwards and allocates tuners to whatever starts first.
            let mut statement = conn.prepare(&format!(
                "SELECT {RECORDING_COLS} FROM dvr_recordings \
                 WHERE state IN (SELECT value FROM json_each(?1)) \
                 ORDER BY capture_start, id"
            ))?;
            let rows = statement.query_map([states], recording_from_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// One statement, so that the state the caller expected and the generation
    /// it planned under are both still true at the instant of the write.
    ///
    /// Each patch names the columns it means to touch and no others. Nothing
    /// here writes `stop_requested_at_ms`, `stop_requested_by_user_id`,
    /// `bytes` or `last_progress_ms` — `Finished` sets `bytes` because a
    /// closed capture's byte count is its final answer rather than a progress
    /// sample — so a transition landing between a progress write and a
    /// viewer's Stop cannot erase either.
    async fn transition_dvr_recording(
        &self,
        transition: &crate::dvr::DvrTransition<'_>,
    ) -> Result<bool, StoreError> {
        let crate::dvr::DvrTransition {
            id,
            from,
            to,
            reason,
            patch,
            fence_generation,
            now_ms,
        } = transition;
        let (to, fence_generation, now_ms) = (*to, *fence_generation, *now_ms);
        let id = id.to_string();
        let from = state_json(from)?;
        let reason = reason.map(str::to_owned);
        let patch = patch.clone();
        self.with_conn(move |conn| {
            let head =
                "UPDATE dvr_recordings SET state = ?1, state_reason = ?2, updated_at_ms = ?3";
            let tail = format!(
                " WHERE id = ?4 AND state IN (SELECT value FROM json_each(?5)) \
                   AND {GENERATION_FENCE}"
            );
            let to = to.as_str();
            let changed = match &patch {
                DvrStatePatch::None => conn.execute(
                    &format!("{head}{tail}"),
                    params![to, reason, now_ms, id, from, fence_generation],
                )?,
                DvrStatePatch::Started {
                    attempt,
                    tuner_owner_node_id,
                    started_at_ms,
                    late_start_s,
                    path,
                } => conn.execute(
                    &format!(
                        "{head}, attempt = ?7, tuner_owner_node_id = ?8, started_at_ms = ?9, \
                         late_start_s = ?10, path = ?11{tail}"
                    ),
                    params![
                        to,
                        reason,
                        now_ms,
                        id,
                        from,
                        fence_generation,
                        attempt,
                        tuner_owner_node_id,
                        started_at_ms,
                        late_start_s,
                        path,
                    ],
                )?,
                DvrStatePatch::Reattempt { attempt, gap_s } => conn.execute(
                    &format!("{head}, attempt = ?7, gap_s = ?8{tail}"),
                    params![
                        to,
                        reason,
                        now_ms,
                        id,
                        from,
                        fence_generation,
                        attempt,
                        gap_s
                    ],
                )?,
                DvrStatePatch::Finished {
                    finished_at_ms,
                    bytes,
                    gap_s,
                    path,
                    stopped_by_user_id,
                } => conn.execute(
                    &format!(
                        // `COALESCE` rather than a plain assignment: a finish
                        // carrying no path means "keep the one the capture
                        // opened", not "forget where the file is". Only a
                        // concatenation that produced a new final file
                        // restates it.
                        "{head}, finished_at_ms = ?7, bytes = ?8, gap_s = ?9, \
                         path = COALESCE(?10, path), \
                         stopped_by_user_id = ?11{tail}"
                    ),
                    params![
                        to,
                        reason,
                        now_ms,
                        id,
                        from,
                        fence_generation,
                        finished_at_ms,
                        bytes,
                        gap_s,
                        path,
                        stopped_by_user_id,
                    ],
                )?,
                DvrStatePatch::Rule { rule_id } => conn.execute(
                    &format!("{head}, rule_id = ?7{tail}"),
                    params![to, reason, now_ms, id, from, fence_generation, rule_id],
                )?,
            };
            Ok(changed == 1)
        })
        .await
    }

    /// Two columns, and not `updated_at_ms` either.
    ///
    /// This runs every few seconds for the whole length of a capture. Anything
    /// else in the SET list would be a column a progress sample can undo, and
    /// a bumped `updated_at_ms` would make every recording in the list look
    /// freshly changed to clients that poll on it.
    async fn progress_dvr_recording(
        &self,
        id: &str,
        bytes: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE dvr_recordings SET bytes = ?1, last_progress_ms = ?2 \
                 WHERE id = ?3 AND state = 'recording'",
                params![bytes, at_ms, id],
            )?;
            Ok(())
        })
        .await
    }

    /// Idempotent by the `stop_requested_at_ms IS NULL` guard: the first Stop
    /// wins the row, and a viewer who presses it again — or a second viewer —
    /// reads back the same request rather than rewriting whose it was.
    async fn request_dvr_stop(
        &self,
        id: &str,
        at_ms: i64,
        by_user: i64,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE dvr_recordings SET stop_requested_at_ms = ?1, \
                 stop_requested_by_user_id = ?2, updated_at_ms = ?1 \
                 WHERE id = ?3 AND state = 'recording' AND stop_requested_at_ms IS NULL",
                params![at_ms, by_user, id],
            )?;
            let row = load_recording(&tx, &id)?;
            tx.commit()?;
            Ok(row)
        })
        .await
    }

    async fn repoint_dvr_rule_rows(
        &self,
        changes: &[(String, Option<String>)],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let changes = changes.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for (recording_id, rule_id) in &changes {
                tx.execute(
                    &format!(
                        "UPDATE dvr_recordings SET rule_id = ?1, updated_at_ms = ?2 \
                         WHERE id = ?3 AND state IN {PENDING_STATES}"
                    ),
                    params![rule_id, now_ms, recording_id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn link_dvr_recording_media(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let recording_id = recording_id.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE dvr_recordings SET item_id = ?1, file_id = ?2, updated_at_ms = ?3 \
                 WHERE id = ?4",
                params![item_id, file_id, now_ms, recording_id],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn list_dvr_reminders(
        &self,
        user_id: i64,
        state: Option<crate::dvr::DvrReminderState>,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError> {
        let state = state.map(DvrReminderState::as_str);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {REMINDER_COLS} FROM dvr_reminders \
                 WHERE user_id = ?1 AND (?2 IS NULL OR state = ?2) \
                 ORDER BY airing_start, id"
            ))?;
            let rows = statement.query_map(params![user_id, state], reminder_from_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn list_dvr_reminders_in(
        &self,
        state: crate::dvr::DvrReminderState,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError> {
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {REMINDER_COLS} FROM dvr_reminders WHERE state = ?1 \
                 ORDER BY airing_start, id"
            ))?;
            let rows = statement.query_map([state.as_str()], reminder_from_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// The per-user ceiling is charged to new reminders only, for the same
    /// reason the rule ceiling is: re-arming or re-timing one of the two
    /// hundred a viewer already has must not be the request that fails.
    async fn put_dvr_reminder(
        &self,
        reminder: &crate::dvr::DvrReminder,
    ) -> Result<bool, StoreError> {
        let reminder = reminder.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing: i64 = tx.query_row(
                "SELECT COUNT(*) FROM dvr_reminders WHERE id = ?1",
                [reminder.id.as_str()],
                |row| row.get(0),
            )?;
            if existing == 0 {
                let owned: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM dvr_reminders WHERE user_id = ?1",
                    [reminder.user_id],
                    |row| row.get(0),
                )?;
                if owned >= DVR_REMINDERS_PER_USER_MAX {
                    return Ok(false);
                }
            }
            tx.execute(
                &format!(
                    "INSERT INTO dvr_reminders ({REMINDER_COLS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                     ON CONFLICT(id) DO UPDATE SET \
                        user_id = excluded.user_id, channel_id = excluded.channel_id, \
                        guide_number = excluded.guide_number, \
                        airing_start = excluded.airing_start, \
                        airing_end = excluded.airing_end, title = excluded.title, \
                        lead_s = excluded.lead_s, state = excluded.state, \
                        fired_at_ms = excluded.fired_at_ms, acked_at_ms = excluded.acked_at_ms, \
                        updated_at_ms = excluded.updated_at_ms"
                ),
                params![
                    reminder.id,
                    reminder.user_id,
                    reminder.channel_id,
                    reminder.guide_number,
                    reminder.airing_start,
                    reminder.airing_end,
                    reminder.title,
                    reminder.lead_s,
                    reminder.state.as_str(),
                    reminder.fired_at_ms,
                    reminder.acked_at_ms,
                    reminder.created_at_ms,
                    reminder.updated_at_ms,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn delete_dvr_reminder(&self, user_id: i64, id: &str) -> Result<bool, StoreError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "DELETE FROM dvr_reminders WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    /// Fire first, expire second, both in one transaction.
    ///
    /// The two passes overlap: a reminder whose programme has already started
    /// satisfies the lead-time test as well, and expiring it in the same call
    /// that fires it would drop it from the returned set — the viewer would
    /// never be told about a programme that is on air right now. Firing first
    /// and then expiring only what is still `armed` leaves the rows this call
    /// fired for the viewer to acknowledge.
    async fn transition_dvr_reminders(
        &self,
        now_seconds: i64,
        now_ms: i64,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut statement = tx.prepare(&format!(
                // A reminder's whole job is to warn someone *before* the
                // programme starts, so the lead time opens the window and the
                // start closes it. Without the upper bound a reminder set for
                // a programme that has already begun would fire on the next
                // tick and read as an alert about something half over.
                "SELECT {REMINDER_COLS} FROM dvr_reminders \
                 WHERE state = 'armed' AND airing_start - lead_s <= ?1 \
                   AND airing_start > ?1 \
                 ORDER BY airing_start, id"
            ))?;
            let due = statement
                .query_map([now_seconds], reminder_from_row)?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let mut fired = Vec::with_capacity(due.len());
            for reminder in due {
                let changed = tx.execute(
                    "UPDATE dvr_reminders SET state = 'fired', fired_at_ms = ?1, \
                     updated_at_ms = ?1 WHERE id = ?2 AND state = 'armed'",
                    params![now_ms, reminder.id],
                )?;
                if changed == 1 {
                    fired.push(DvrReminder {
                        state: DvrReminderState::Fired,
                        fired_at_ms: Some(now_ms),
                        updated_at_ms: now_ms,
                        ..reminder
                    });
                }
            }
            // Once the programme is on, the reminder is spent — whether it
            // fired and nobody acknowledged it, or its lead time was so short
            // that the start arrived first. Both are `expired`; neither is a
            // row any client should still be drawing an overlay for.
            tx.execute(
                "UPDATE dvr_reminders SET state = 'expired', updated_at_ms = ?1 \
                 WHERE state IN ('armed', 'fired') AND airing_start <= ?2",
                params![now_ms, now_seconds],
            )?;
            tx.commit()?;
            Ok(fired)
        })
        .await
    }

    /// `user_id`, when given, is the ownership check: a viewer acknowledging
    /// or dismissing a reminder has to name one of their own, and the check
    /// belongs in the same statement as the write rather than in a read the
    /// route performs before it. `None` is the owner loop, which has no user.
    async fn set_dvr_reminder_state(
        &self,
        user_id: Option<i64>,
        id: &str,
        from: crate::dvr::DvrReminderState,
        to: crate::dvr::DvrReminderState,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let id = id.to_owned();
        // `acked_at_ms` is stamped by the one transition that earns it and
        // left alone by every other, so a later `expired` does not rewrite
        // when the viewer saw it.
        let acked_at_ms = (to == DvrReminderState::Acked).then_some(now_ms);
        let fired_at_ms = (to == DvrReminderState::Fired).then_some(now_ms);
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE dvr_reminders SET state = ?1, acked_at_ms = COALESCE(?2, acked_at_ms), \
                 fired_at_ms = COALESCE(?3, fired_at_ms), updated_at_ms = ?4 \
                 WHERE id = ?5 AND state = ?6 AND (?7 IS NULL OR user_id = ?7)",
                params![
                    to.as_str(),
                    acked_at_ms,
                    fired_at_ms,
                    now_ms,
                    id,
                    from.as_str(),
                    user_id
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }
}
