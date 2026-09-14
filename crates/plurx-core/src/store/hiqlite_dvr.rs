//! Replicated persistence for recording rules, airings and reminders.
//!
//! Every statement here is executed independently by each voter, so nothing in
//! this file may read a clock, a random number or an autoincrementing counter:
//! the caller supplies every such value as a parameter, and two voters
//! replaying the same log entry land on byte-identical rows. That constraint
//! is why so many of these writes carry a `now_ms` argument that a single-node
//! store would have taken from `unixepoch()`.
//!
//! The second habit worth naming up front is that a decision and the write it
//! authorises live in one statement. A read-then-write pair is two raft
//! entries with a window between them, and the owner loop's whole job is to
//! act on rows that other nodes are simultaneously allowed to change — so the
//! state a write expects, the configuration generation it was planned under,
//! and the row limit it must respect are all predicates of the write itself.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::{Param, Row};

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::DvrStore;
use crate::dvr::{
    DvrAttentionRow, DvrEvent, DvrEventHead, DvrEventInput, DvrEventPage, DvrInsertOutcome,
    DvrKeepMode, DvrMatchMode, DvrOrigin, DvrRecording, DvrRecordingFilter, DvrReminder,
    DvrReminderState, DvrRule, DvrState, DvrStatePatch, DvrTransition, DVR_EVENT_PAGE_MAX,
    DVR_EVENT_PRUNE_BATCH, DVR_EVENT_SCHEMA, DVR_EVENT_SERVER_MAX, DVR_RECORDINGS_LIST_PAGE,
    DVR_REMINDERS_PER_USER_MAX, DVR_RULES_MAX, DVR_SCHEMA,
};
use crate::error::StoreError;

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    let mut statements = migration_statements()?;
    statements.extend(event_migration_statements()?);
    client.txn(statements).await.map_err(database_error)?;
    Ok(())
}

pub(super) fn event_migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    split_schema_statements(DVR_EVENT_SCHEMA)
        .into_iter()
        .map(|statement| {
            validate_sql(&statement)?;
            Ok((statement, params!()))
        })
        .collect()
}

pub(super) fn migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    split_schema_statements(DVR_SCHEMA)
        .into_iter()
        .map(|statement| {
            validate_sql(&statement)?;
            Ok((statement, params!()))
        })
        .collect()
}

/// Split the shared SQLite migration into the individual writes hiqlite's
/// transaction API requires. A trigger body contains its own semicolon, so a
/// plain `split(';')` silently truncates it before replication.
fn split_schema_statements(schema: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_trigger = false;

    for line in schema.lines() {
        let trimmed = line.trim();
        if current.is_empty() && trimmed.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);

        if !in_trigger
            && current
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE TRIGGER")
        {
            in_trigger = true;
        }
        let complete = if in_trigger {
            trimmed.eq_ignore_ascii_case("END;")
        } else {
            trimmed.ends_with(';')
        };
        if complete {
            let statement = current.trim().strip_suffix(';').unwrap_or(current.trim());
            statements.push(statement.to_owned());
            current.clear();
            in_trigger = false;
        }
    }

    debug_assert!(
        current.trim().is_empty(),
        "schema statement lacks a terminator"
    );
    statements
}

/// The tuner-configuration fence, as the tail of a `WHERE` clause.
///
/// The owner loop plans a whole tick — which airings get which tuner — against
/// one configuration generation, and an administrator may save new Live TV
/// settings while that tick is in flight. Reading the generation first and
/// writing second leaves a window in which the plan is already void; appending
/// this to the transition's own predicate closes it, because the comparison
/// and the write are then one statement and one raft entry.
///
/// It deliberately carries no placeholder of its own: the caller appends the
/// `$N` it has just bound, so the ordinal stays correct whichever patch
/// variant has already consumed placeholders ahead of it.
const CONFIG_GENERATION_FENCE: &str = " AND COALESCE((SELECT CAST(value AS INTEGER) FROM settings \
       WHERE key = 'live_tv.config_generation'), 0) = ";

const RULE_COLS: &str = "id, owner_user_id, priority, name, match_mode, match_value, \
    channel_id, new_only, keep_mode, keep_value, pad_start_s, pad_end_s, enabled, \
    created_at_ms, updated_at_ms";

const RECORDING_COLS: &str = "id, origin, rule_id, requested_by_user_id, channel_id, \
    guide_number, channel_name, airing_start, airing_end, capture_start, capture_end, \
    title, episode_title, episode, synopsis, image_url, original_air_date, series_id, \
    programme_id, state, state_reason, attempt, gap_s, late_start_s, tuner_owner_node_id, \
    path, bytes, last_progress_ms, stop_requested_at_ms, stop_requested_by_user_id, \
    item_id, file_id, started_at_ms, finished_at_ms, stopped_by_user_id, created_at_ms, \
    updated_at_ms";

const REMINDER_COLS: &str = "id, user_id, channel_id, guide_number, airing_start, airing_end, \
    title, lead_s, state, fired_at_ms, acked_at_ms, created_at_ms, updated_at_ms";

/// A statement's parameters and its `SET` list, built together.
///
/// hiqlite binds `$N` by first appearance, so a statement assembled from a
/// runtime-chosen column set is only correct if the text and the parameter
/// vector grow in lockstep. Handing out the placeholder *as the value is
/// bound* makes that structural rather than a thing to remember: there is no
/// point in the assembly at which a column could be written without its value,
/// or numbered out of turn.
#[derive(Default)]
struct Binder {
    sets: Vec<String>,
    values: Vec<Param>,
}

impl Binder {
    /// Bind one value and return the placeholder that names it.
    fn bind(&mut self, value: impl Into<Param>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }

    /// Bind one value and assign it to a column.
    fn set(&mut self, column: &str, value: impl Into<Param>) {
        let placeholder = self.bind(value);
        self.sets.push(format!("{column} = {placeholder}"));
    }

    /// Assign a column only when the caller supplied a value, keeping what is
    /// stored otherwise. `path` needs this: a finish that carries no path
    /// means "keep the one the capture opened", not "forget where the file
    /// is", and only a concatenation that produced a new final file restates
    /// it.
    fn set_or_keep(&mut self, column: &str, value: impl Into<Param>) {
        let placeholder = self.bind(value);
        self.sets
            .push(format!("{column} = COALESCE({placeholder}, {column})"));
    }

    fn assignments(&self) -> String {
        self.sets.join(", ")
    }

    /// Bind every state in a list and return them as an `IN` list body.
    fn bind_states(&mut self, states: &[DvrState]) -> String {
        states
            .iter()
            .map(|state| self.bind(state.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

struct RuleRow {
    id: String,
    owner_user_id: i64,
    priority: i64,
    name: String,
    match_mode: String,
    match_value: String,
    channel_id: Option<String>,
    new_only: i64,
    keep_mode: String,
    keep_value: i64,
    pad_start_s: i64,
    pad_end_s: i64,
    enabled: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for RuleRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            owner_user_id: row.get("owner_user_id"),
            priority: row.get("priority"),
            name: row.get("name"),
            match_mode: row.get("match_mode"),
            match_value: row.get("match_value"),
            channel_id: row.get("channel_id"),
            new_only: row.get("new_only"),
            keep_mode: row.get("keep_mode"),
            keep_value: row.get("keep_value"),
            pad_start_s: row.get("pad_start_s"),
            pad_end_s: row.get("pad_end_s"),
            enabled: row.get("enabled"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl TryFrom<RuleRow> for DvrRule {
    type Error = StoreError;

    fn try_from(row: RuleRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            owner_user_id: row.owner_user_id,
            priority: row.priority,
            name: row.name,
            match_mode: DvrMatchMode::parse(&row.match_mode).ok_or_else(|| {
                StoreError::Database(format!("unknown dvr match mode `{}`", row.match_mode))
            })?,
            match_value: row.match_value,
            channel_id: row.channel_id,
            new_only: row.new_only != 0,
            keep_mode: DvrKeepMode::parse(&row.keep_mode).ok_or_else(|| {
                StoreError::Database(format!("unknown dvr keep mode `{}`", row.keep_mode))
            })?,
            keep_value: row.keep_value,
            pad_start_s: row.pad_start_s,
            pad_end_s: row.pad_end_s,
            enabled: row.enabled != 0,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

/// Just enough of a rule to renumber it: the reorder needs the ids it is
/// allowed to touch and the priorities it has to stay clear of.
struct RulePriorityRow {
    id: String,
    priority: i64,
}

impl From<&mut Row<'_>> for RulePriorityRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            priority: row.get("priority"),
        }
    }
}

struct StateRow {
    state: String,
}

impl From<&mut Row<'_>> for StateRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            state: row.get("state"),
        }
    }
}

struct RecordingRow {
    id: String,
    origin: String,
    rule_id: Option<String>,
    requested_by_user_id: Option<i64>,
    channel_id: String,
    guide_number: String,
    channel_name: String,
    airing_start: i64,
    airing_end: i64,
    capture_start: i64,
    capture_end: i64,
    title: String,
    episode_title: Option<String>,
    episode: Option<String>,
    synopsis: Option<String>,
    image_url: Option<String>,
    original_air_date: Option<String>,
    series_id: Option<String>,
    programme_id: Option<String>,
    state: String,
    state_reason: Option<String>,
    attempt: i64,
    gap_s: i64,
    late_start_s: i64,
    tuner_owner_node_id: Option<String>,
    path: Option<String>,
    bytes: i64,
    last_progress_ms: Option<i64>,
    stop_requested_at_ms: Option<i64>,
    stop_requested_by_user_id: Option<i64>,
    item_id: Option<i64>,
    file_id: Option<i64>,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    stopped_by_user_id: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for RecordingRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            origin: row.get("origin"),
            rule_id: row.get("rule_id"),
            requested_by_user_id: row.get("requested_by_user_id"),
            channel_id: row.get("channel_id"),
            guide_number: row.get("guide_number"),
            channel_name: row.get("channel_name"),
            airing_start: row.get("airing_start"),
            airing_end: row.get("airing_end"),
            capture_start: row.get("capture_start"),
            capture_end: row.get("capture_end"),
            title: row.get("title"),
            episode_title: row.get("episode_title"),
            episode: row.get("episode"),
            synopsis: row.get("synopsis"),
            image_url: row.get("image_url"),
            original_air_date: row.get("original_air_date"),
            series_id: row.get("series_id"),
            programme_id: row.get("programme_id"),
            state: row.get("state"),
            state_reason: row.get("state_reason"),
            attempt: row.get("attempt"),
            gap_s: row.get("gap_s"),
            late_start_s: row.get("late_start_s"),
            tuner_owner_node_id: row.get("tuner_owner_node_id"),
            path: row.get("path"),
            bytes: row.get("bytes"),
            last_progress_ms: row.get("last_progress_ms"),
            stop_requested_at_ms: row.get("stop_requested_at_ms"),
            stop_requested_by_user_id: row.get("stop_requested_by_user_id"),
            item_id: row.get("item_id"),
            file_id: row.get("file_id"),
            started_at_ms: row.get("started_at_ms"),
            finished_at_ms: row.get("finished_at_ms"),
            stopped_by_user_id: row.get("stopped_by_user_id"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl TryFrom<RecordingRow> for DvrRecording {
    type Error = StoreError;

    fn try_from(row: RecordingRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            origin: DvrOrigin::parse(&row.origin).ok_or_else(|| {
                StoreError::Database(format!("unknown dvr origin `{}`", row.origin))
            })?,
            rule_id: row.rule_id,
            requested_by_user_id: row.requested_by_user_id,
            channel_id: row.channel_id,
            guide_number: row.guide_number,
            channel_name: row.channel_name,
            airing_start: row.airing_start,
            airing_end: row.airing_end,
            capture_start: row.capture_start,
            capture_end: row.capture_end,
            title: row.title,
            episode_title: row.episode_title,
            episode: row.episode,
            synopsis: row.synopsis,
            image_url: row.image_url,
            original_air_date: row.original_air_date,
            series_id: row.series_id,
            programme_id: row.programme_id,
            state: DvrState::parse(&row.state).ok_or_else(|| {
                StoreError::Database(format!("unknown dvr state `{}`", row.state))
            })?,
            state_reason: row.state_reason,
            attempt: row.attempt,
            gap_s: row.gap_s,
            late_start_s: row.late_start_s,
            tuner_owner_node_id: row.tuner_owner_node_id,
            path: row.path,
            bytes: row.bytes,
            last_progress_ms: row.last_progress_ms,
            stop_requested_at_ms: row.stop_requested_at_ms,
            stop_requested_by_user_id: row.stop_requested_by_user_id,
            item_id: row.item_id,
            file_id: row.file_id,
            started_at_ms: row.started_at_ms,
            finished_at_ms: row.finished_at_ms,
            stopped_by_user_id: row.stopped_by_user_id,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

struct EventRow {
    recording_id: String,
    sequence: i64,
    event_id: String,
    kind: String,
    occurred_at_ms: i64,
    attempt: Option<i64>,
    actor_user_id: Option<i64>,
    reason_code: Option<String>,
    facts_json: String,
}

impl From<&mut Row<'_>> for EventRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            recording_id: row.get("recording_id"),
            sequence: row.get("sequence"),
            event_id: row.get("event_id"),
            kind: row.get("kind"),
            occurred_at_ms: row.get("occurred_at_ms"),
            attempt: row.get("attempt"),
            actor_user_id: row.get("actor_user_id"),
            reason_code: row.get("reason_code"),
            facts_json: row.get("facts_json"),
        }
    }
}

impl TryFrom<EventRow> for DvrEvent {
    type Error = StoreError;

    fn try_from(row: EventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            recording_id: row.recording_id,
            sequence: row.sequence,
            event_id: row.event_id,
            kind: row.kind,
            occurred_at_ms: row.occurred_at_ms,
            attempt: row.attempt,
            actor_user_id: row.actor_user_id,
            reason_code: row.reason_code,
            facts: serde_json::from_str(&row.facts_json).map_err(|error| {
                StoreError::Database(format!("invalid DVR event facts: {error}"))
            })?,
        })
    }
}

struct EventHeadRow {
    recording_id: String,
    next_sequence: i64,
    latest_attention_sequence: i64,
    latest_attention_at_ms: Option<i64>,
    history_started_at_ms: i64,
    history_has_gap: i64,
    pruned_through_sequence: i64,
}

impl From<&mut Row<'_>> for EventHeadRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            recording_id: row.get("recording_id"),
            next_sequence: row.get("next_sequence"),
            latest_attention_sequence: row.get("latest_attention_sequence"),
            latest_attention_at_ms: row.get("latest_attention_at_ms"),
            history_started_at_ms: row.get("history_started_at_ms"),
            history_has_gap: row.get("history_has_gap"),
            pruned_through_sequence: row.get("pruned_through_sequence"),
        }
    }
}

impl From<EventHeadRow> for DvrEventHead {
    fn from(row: EventHeadRow) -> Self {
        Self {
            recording_id: row.recording_id,
            next_sequence: row.next_sequence,
            latest_attention_sequence: row.latest_attention_sequence,
            latest_attention_at_ms: row.latest_attention_at_ms,
            history_started_at_ms: row.history_started_at_ms,
            history_has_gap: row.history_has_gap != 0,
            pruned_through_sequence: row.pruned_through_sequence,
        }
    }
}

struct AttentionRowRaw {
    recording: RecordingRow,
    latest_attention_sequence: i64,
    latest_attention_at_ms: i64,
    acknowledged_through_sequence: i64,
    total_count: i64,
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

struct EventIdentityRow {
    recording_id: String,
    sequence: i64,
}

impl From<&mut Row<'_>> for EventIdentityRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            recording_id: row.get("recording_id"),
            sequence: row.get("sequence"),
        }
    }
}

struct AckRow {
    through_sequence: i64,
}

impl From<&mut Row<'_>> for AckRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            through_sequence: row.get("through_sequence"),
        }
    }
}

impl From<&mut Row<'_>> for AttentionRowRaw {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            recording: RecordingRow::from(&mut *row),
            latest_attention_sequence: row.get("latest_attention_sequence"),
            latest_attention_at_ms: row.get("attention_at_ms"),
            acknowledged_through_sequence: row.get("acknowledged_through_sequence"),
            total_count: row.get("total_count"),
        }
    }
}

struct ReminderRow {
    id: String,
    user_id: i64,
    channel_id: String,
    guide_number: String,
    airing_start: i64,
    airing_end: i64,
    title: String,
    lead_s: i64,
    state: String,
    fired_at_ms: Option<i64>,
    acked_at_ms: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for ReminderRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            user_id: row.get("user_id"),
            channel_id: row.get("channel_id"),
            guide_number: row.get("guide_number"),
            airing_start: row.get("airing_start"),
            airing_end: row.get("airing_end"),
            title: row.get("title"),
            lead_s: row.get("lead_s"),
            state: row.get("state"),
            fired_at_ms: row.get("fired_at_ms"),
            acked_at_ms: row.get("acked_at_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl TryFrom<ReminderRow> for DvrReminder {
    type Error = StoreError;

    fn try_from(row: ReminderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            user_id: row.user_id,
            channel_id: row.channel_id,
            guide_number: row.guide_number,
            airing_start: row.airing_start,
            airing_end: row.airing_end,
            title: row.title,
            lead_s: row.lead_s,
            state: DvrReminderState::parse(&row.state).ok_or_else(|| {
                StoreError::Database(format!("unknown dvr reminder state `{}`", row.state))
            })?,
            fired_at_ms: row.fired_at_ms,
            acked_at_ms: row.acked_at_ms,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

/// The states a rule row may still be re-pointed out of, as an `IN` list of
/// literals.
///
/// Derived from [`DvrState::PENDING`] rather than spelled out, so a new
/// pending state cannot be added to the domain and quietly left out of
/// reconciliation.
fn pending_state_literals() -> String {
    DvrState::PENDING
        .iter()
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Assemble the transition's `UPDATE`, with its parameters.
///
/// Separate from the trait method so the placeholder ordering of every patch
/// variant — and of the fenced and unfenced forms of each — can be checked by
/// a test. The repo-wide census in `placeholder_census.rs` reads statements out
/// of the source text, and a statement assembled one column at a time is
/// invisible to it: there is no literal carrying the `$N` it will end up with.
fn transition_statement(
    id: &str,
    from: &[DvrState],
    to: DvrState,
    reason: Option<&str>,
    patch: &DvrStatePatch,
    fence_generation: Option<i64>,
    now_ms: i64,
) -> (String, Vec<Param>) {
    let mut binder = Binder::default();
    binder.set("state", to.as_str());
    binder.set("state_reason", reason);
    binder.set("updated_at_ms", now_ms);
    match patch {
        DvrStatePatch::None => {}
        DvrStatePatch::Started {
            attempt,
            tuner_owner_node_id,
            started_at_ms,
            late_start_s,
            path,
        } => {
            binder.set("attempt", *attempt);
            binder.set("tuner_owner_node_id", tuner_owner_node_id.as_str());
            binder.set("started_at_ms", *started_at_ms);
            binder.set("late_start_s", *late_start_s);
            binder.set("path", path.as_str());
        }
        DvrStatePatch::Reattempt { attempt, gap_s } => {
            binder.set("attempt", *attempt);
            binder.set("gap_s", *gap_s);
        }
        DvrStatePatch::Finished {
            finished_at_ms,
            bytes,
            gap_s,
            path,
            stopped_by_user_id,
        } => {
            binder.set("finished_at_ms", *finished_at_ms);
            binder.set("bytes", *bytes);
            binder.set("gap_s", *gap_s);
            binder.set_or_keep("path", path.as_deref());
            binder.set("stopped_by_user_id", *stopped_by_user_id);
        }
        DvrStatePatch::Rule { rule_id } => {
            binder.set("rule_id", rule_id.as_deref());
        }
        // A literal `NULL` rather than a bound `None`: this is the one write
        // whose whole purpose is to forget where the file was, so it must not
        // go through `set_or_keep`.
        DvrStatePatch::Purged => binder.sets.push("path = NULL".to_owned()),
    }
    let mut predicate = format!("id = {}", binder.bind(id));
    let states = binder.bind_states(from);
    predicate.push_str(&format!(" AND state IN ({states})"));
    if let Some(generation) = fence_generation {
        predicate.push_str(CONFIG_GENERATION_FENCE);
        let bound = binder.bind(generation);
        predicate.push_str(&bound);
    }
    let assignments = binder.assignments();
    (
        format!("UPDATE dvr_recordings SET {assignments} WHERE {predicate}"),
        binder.values,
    )
}

fn transition_event_statements(
    transition: &DvrTransition<'_>,
    event: &DvrEventInput,
) -> Vec<(String, hiqlite::Params)> {
    let mut head = Binder::default();
    let occurred = head.bind(event.occurred_at_ms);
    let id = head.bind(transition.id);
    let states = head.bind_states(transition.from);
    let mut predicate = format!("r.id={id} AND r.state IN ({states})");
    if let Some(generation) = transition.fence_generation {
        predicate.push_str(CONFIG_GENERATION_FENCE);
        predicate.push_str(&head.bind(generation));
    }
    let head_sql = format!(
        "INSERT OR IGNORE INTO dvr_event_heads
         (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
          history_started_at_ms,history_has_gap,pruned_through_sequence)
         SELECT r.id,1,0,NULL,{occurred},1,0 FROM dvr_recordings r WHERE {predicate}"
    );

    let mut insert = Binder::default();
    let recording_id = insert.bind(transition.id);
    let event_id = insert.bind(event.event_id.as_str());
    let kind = insert.bind(event.kind.as_str());
    let event_at = insert.bind(event.occurred_at_ms);
    let attempt = insert.bind(event.attempt);
    let actor = insert.bind(event.actor_user_id);
    let reason = insert.bind(event.reason_code.as_deref());
    let facts = insert.bind(event.facts_json.as_str());
    let states = insert.bind_states(transition.from);
    let mut predicate = format!(
        "r.id={recording_id} AND r.state IN ({states}) AND
         NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id={event_id})"
    );
    if let Some(generation) = transition.fence_generation {
        predicate.push_str(CONFIG_GENERATION_FENCE);
        predicate.push_str(&insert.bind(generation));
    }
    let insert_sql = format!(
        "INSERT INTO dvr_events
         (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
         SELECT {recording_id},h.next_sequence,{event_id},{kind},{event_at},{attempt},{actor},{reason},{facts}
           FROM dvr_event_heads h JOIN dvr_recordings r ON r.id=h.recording_id
          WHERE {predicate}"
    );
    let update_sql =
        "UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
           latest_attention_sequence=CASE WHEN $1 THEN next_sequence ELSE latest_attention_sequence END,
           latest_attention_at_ms=CASE WHEN $1 THEN $2 ELSE latest_attention_at_ms END
         WHERE recording_id=$3 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$4)"
            .to_owned();
    let (transition_sql, transition_params) = transition_statement(
        transition.id,
        transition.from,
        transition.to,
        transition.reason,
        &transition.patch,
        transition.fence_generation,
        transition.now_ms,
    );
    vec![
        (head_sql, head.values),
        (insert_sql, insert.values),
        (
            update_sql,
            params!(
                event.actionable,
                event.occurred_at_ms,
                transition.id,
                event.event_id.as_str()
            ),
        ),
        (transition_sql, transition_params),
    ]
}

/// Assemble one schedule page's `SELECT`, with its parameters. Extracted for
/// the same reason [`transition_statement`] is.
fn recording_page_statement(
    filter: &DvrRecordingFilter,
    after: Option<&str>,
    limit: i64,
) -> (String, Vec<Param>) {
    let mut binder = Binder::default();
    // A first page starts above every real airing, so one descending
    // comparison serves both cases.
    let (after_start, after_id) = after
        .and_then(crate::dvr::parse_recording_cursor)
        .unwrap_or((i64::MAX, ""));
    // The tie arm binds the same instant a second time rather than reusing the
    // first placeholder. hiqlite takes its parameters positionally, in the
    // order the statement introduces them, so a repeated `$1` would leave every
    // later value bound one position early — the page limit would arrive as the
    // row id, and a cluster's schedule page would answer with the wrong rows or
    // refuse outright. `every_assembled_statement_introduces_its_placeholders_in_order`
    // is the check that says so.
    let start = binder.bind(after_start);
    let start_tie = binder.bind(after_start);
    let id = binder.bind(after_id);
    let mut predicate =
        format!("(airing_start < {start} OR (airing_start = {start_tie} AND id > {id}))");
    if filter.states.is_empty() {
        // Deleted rows are history rather than schedule. They come back only
        // when the caller asks for them, by flag or by name.
        if !filter.include_deleted {
            predicate.push_str(" AND state <> 'deleted'");
        }
    } else {
        let states = binder.bind_states(&filter.states);
        predicate.push_str(&format!(" AND state IN ({states})"));
    }
    if let Some(before) = filter.before_capture_start {
        predicate.push_str(&format!(" AND capture_start < {}", binder.bind(before)));
    }
    let page = binder.bind(limit.clamp(1, DVR_RECORDINGS_LIST_PAGE));
    (
        format!(
            // Newest airing first, with the id breaking a tie so the order is
            // total: two channels can carry a programme at the same instant,
            // and a boundary that was not total would repeat or skip one.
            "SELECT {RECORDING_COLS} FROM dvr_recordings \
             WHERE {predicate} ORDER BY airing_start DESC, id LIMIT {page}"
        ),
        binder.values,
    )
}

async fn read_recordings(
    store: &HiqliteAuthStore,
    sql: String,
    values: Vec<Param>,
) -> Result<Vec<DvrRecording>, StoreError> {
    store
        .client()
        .query_consistent_map::<RecordingRow, _>(sql, values)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
}

async fn read_reminders(
    store: &HiqliteAuthStore,
    sql: String,
    values: Vec<Param>,
) -> Result<Vec<DvrReminder>, StoreError> {
    store
        .client()
        .query_consistent_map::<ReminderRow, _>(sql, values)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
}

#[async_trait]
impl DvrStore for HiqliteAuthStore {
    async fn list_dvr_rules(&self) -> Result<Vec<DvrRule>, StoreError> {
        self.client()
            .query_consistent_map::<RuleRow, _>(
                format!("SELECT {RULE_COLS} FROM dvr_rules ORDER BY priority, id"),
                params!(),
            )
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn get_dvr_rule(&self, id: &str) -> Result<Option<DvrRule>, StoreError> {
        self.client()
            .query_consistent_map::<RuleRow, _>(
                format!("SELECT {RULE_COLS} FROM dvr_rules WHERE id = $1"),
                params!(id),
            )
            .await?
            .into_iter()
            .next()
            .map(TryInto::try_into)
            .transpose()
    }

    /// Write a rule, creating it or replacing it wholesale.
    ///
    /// The cap is a predicate of the insert rather than a count read first and
    /// compared second: two nodes accepting the two hundredth rule at the same
    /// moment would both pass a separate check and both write. Because the
    /// `EXISTS` arm short-circuits it, an edit of a rule that already exists is
    /// never refused for being one rule too many — only a genuinely new one is.
    async fn put_dvr_rule(&self, rule: &DvrRule) -> Result<bool, StoreError> {
        let changed = self
            .client()
            .execute(
                format!(
                    "INSERT INTO dvr_rules ({RULE_COLS}) \
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15 \
                     WHERE EXISTS(SELECT 1 FROM dvr_rules WHERE id = $1) \
                        OR (SELECT COUNT(*) FROM dvr_rules) < $16 \
                     ON CONFLICT(id) DO UPDATE SET \
                       owner_user_id = excluded.owner_user_id, priority = excluded.priority, \
                       name = excluded.name, match_mode = excluded.match_mode, \
                       match_value = excluded.match_value, channel_id = excluded.channel_id, \
                       new_only = excluded.new_only, keep_mode = excluded.keep_mode, \
                       keep_value = excluded.keep_value, pad_start_s = excluded.pad_start_s, \
                       pad_end_s = excluded.pad_end_s, enabled = excluded.enabled, \
                       updated_at_ms = excluded.updated_at_ms"
                ),
                params!(
                    rule.id.as_str(),
                    rule.owner_user_id,
                    rule.priority,
                    rule.name.as_str(),
                    rule.match_mode.as_str(),
                    rule.match_value.as_str(),
                    rule.channel_id.as_deref(),
                    rule.new_only,
                    rule.keep_mode.as_str(),
                    rule.keep_value,
                    rule.pad_start_s,
                    rule.pad_end_s,
                    rule.enabled,
                    rule.created_at_ms,
                    rule.updated_at_ms,
                    DVR_RULES_MAX
                ),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn delete_dvr_rule(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self
            .execute("DELETE FROM dvr_rules WHERE id = $1", params!(id))
            .await?;
        Ok(changed == 1)
    }

    /// Renumber every rule into the given order, in two passes.
    ///
    /// `priority` is unique, which is the point of it — the scheduler's answer
    /// to "which of these two airings gets the last tuner" has to be total, not
    /// a tie broken by row order. That uniqueness also means a one-pass
    /// renumber collides with itself the moment the new order overlaps the old
    /// one, which it almost always does. So the first pass parks every row
    /// below the lowest priority currently stored, where no surviving row can
    /// be, and the second pass writes the final `1..=n`. Both passes are in one
    /// transaction, because a crash between them would leave the whole rule set
    /// numbered out of the operator's sight.
    ///
    /// The order must name the rule set exactly. A partial list would silently
    /// leave some rules holding parked negative priorities.
    async fn reorder_dvr_rules(
        &self,
        ids_in_priority_order: &[String],
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let current = self
            .client()
            .query_consistent_map::<RulePriorityRow, _>(
                "SELECT id, priority FROM dvr_rules",
                params!(),
            )
            .await?;
        if current.len() != ids_in_priority_order.len() {
            return Ok(false);
        }
        let mut named = ids_in_priority_order.to_vec();
        named.sort();
        named.dedup();
        let mut known = current.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        known.sort();
        if named != known {
            return Ok(false);
        }
        if known.is_empty() {
            return Ok(true);
        }

        // Zero is the floor even when every stored priority is positive, so
        // the parked range is always negative and always disjoint from the
        // `1..=n` the second pass writes.
        let floor = current
            .iter()
            .map(|row| row.priority)
            .min()
            .unwrap_or(0)
            .min(0);
        let mut statements: Vec<(&'static str, hiqlite::Params)> =
            Vec::with_capacity(ids_in_priority_order.len() * 2);
        for (offset, id) in (1_i64..).zip(ids_in_priority_order) {
            statements.push((
                "UPDATE dvr_rules SET priority = $1 WHERE id = $2",
                params!(floor.saturating_sub(offset), id.as_str()),
            ));
        }
        for (priority, id) in (1_i64..).zip(ids_in_priority_order) {
            statements.push((
                "UPDATE dvr_rules SET priority = $1, updated_at_ms = $2 WHERE id = $3",
                params!(priority, now_ms, id.as_str()),
            ));
        }
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.iter().all(|count| *count == 1))
    }

    /// Materialise one airing, or report the row that already owns it.
    ///
    /// This cannot be an upsert, and the distinction is the whole reason the
    /// method exists. `(channel_id, airing_start)` identifies an airing for its
    /// entire life, across every state, so the row a viewer cancelled is the
    /// same row rule expansion matches again fifteen seconds later. An upsert
    /// would revive it on every tick; `INSERT … SELECT … WHERE NOT EXISTS`
    /// writes nothing and hands the caller the state it found instead, which is
    /// what lets the caller tell "already scheduled" from "deliberately
    /// skipped" without a second read.
    async fn insert_dvr_airing_if_absent(
        &self,
        row: &DvrRecording,
    ) -> Result<DvrInsertOutcome, StoreError> {
        let inserted = self
            .client()
            .txn([(
                format!(
                    "INSERT INTO dvr_recordings ({RECORDING_COLS}) \
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                       $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, \
                       $30, $31, $32, $33, $34, $35, $36, $37 \
                     WHERE NOT EXISTS(SELECT 1 FROM dvr_recordings \
                       WHERE channel_id = $5 AND airing_start = $8)"
                ),
                params!(
                    row.id.as_str(),
                    row.origin.as_str(),
                    row.rule_id.as_deref(),
                    row.requested_by_user_id,
                    row.channel_id.as_str(),
                    row.guide_number.as_str(),
                    row.channel_name.as_str(),
                    row.airing_start,
                    row.airing_end,
                    row.capture_start,
                    row.capture_end,
                    row.title.as_str(),
                    row.episode_title.as_deref(),
                    row.episode.as_deref(),
                    row.synopsis.as_deref(),
                    row.image_url.as_deref(),
                    row.original_air_date.as_deref(),
                    row.series_id.as_deref(),
                    row.programme_id.as_deref(),
                    row.state.as_str(),
                    row.state_reason.as_deref(),
                    row.attempt,
                    row.gap_s,
                    row.late_start_s,
                    row.tuner_owner_node_id.as_deref(),
                    row.path.as_deref(),
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
                    row.updated_at_ms
                ),
            )])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if inserted.first().copied() == Some(1) {
            return Ok(DvrInsertOutcome::Inserted);
        }
        let existing = self
            .client()
            .query_consistent_map::<StateRow, _>(
                "SELECT state FROM dvr_recordings WHERE channel_id = $1 AND airing_start = $2",
                params!(row.channel_id.as_str(), row.airing_start),
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                StoreError::Database(
                    "dvr airing was neither inserted nor already present".to_owned(),
                )
            })?;
        DvrState::parse(&existing.state)
            .map(DvrInsertOutcome::Exists)
            .ok_or_else(|| StoreError::Database(format!("unknown dvr state `{}`", existing.state)))
    }

    async fn insert_dvr_airing_with_event(
        &self,
        row: &DvrRecording,
        event: &DvrEventInput,
    ) -> Result<DvrInsertOutcome, StoreError> {
        if !event.validate() {
            return Err(StoreError::Database(
                "invalid DVR lifecycle event".to_owned(),
            ));
        }
        let statements = vec![
            (format!(
                "INSERT INTO dvr_recordings ({RECORDING_COLS})
                 SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,
                        $20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37
                  WHERE NOT EXISTS(SELECT 1 FROM dvr_recordings
                    WHERE channel_id=$5 AND airing_start=$8)"
            ), params!(
                row.id.as_str(), row.origin.as_str(), row.rule_id.as_deref(),
                row.requested_by_user_id, row.channel_id.as_str(), row.guide_number.as_str(),
                row.channel_name.as_str(), row.airing_start, row.airing_end, row.capture_start,
                row.capture_end, row.title.as_str(), row.episode_title.as_deref(),
                row.episode.as_deref(), row.synopsis.as_deref(), row.image_url.as_deref(),
                row.original_air_date.as_deref(), row.series_id.as_deref(),
                row.programme_id.as_deref(), row.state.as_str(), row.state_reason.as_deref(),
                row.attempt, row.gap_s, row.late_start_s, row.tuner_owner_node_id.as_deref(),
                row.path.as_deref(), row.bytes, row.last_progress_ms, row.stop_requested_at_ms,
                row.stop_requested_by_user_id, row.item_id, row.file_id, row.started_at_ms,
                row.finished_at_ms, row.stopped_by_user_id, row.created_at_ms, row.updated_at_ms
            )),
            ("INSERT OR IGNORE INTO dvr_event_heads
              (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
               history_started_at_ms,history_has_gap,pruned_through_sequence)
              SELECT $1,1,0,NULL,$2,0,0 FROM dvr_recordings WHERE id=$1".to_owned(),
             params!(row.id.as_str(), event.occurred_at_ms)),
            ("INSERT INTO dvr_events
              (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
              SELECT $1,h.next_sequence,$2,$3,$4,$5,$6,$7,$8 FROM dvr_event_heads h
               WHERE h.recording_id=$1 AND NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id=$2)".to_owned(),
             params!(row.id.as_str(), event.event_id.as_str(), event.kind.as_str(),
                 event.occurred_at_ms, event.attempt, event.actor_user_id,
                 event.reason_code.as_deref(), event.facts_json.as_str())),
            ("UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
                latest_attention_sequence=CASE WHEN $1 THEN next_sequence ELSE latest_attention_sequence END,
                latest_attention_at_ms=CASE WHEN $1 THEN $2 ELSE latest_attention_at_ms END
              WHERE recording_id=$3 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$4)".to_owned(),
             params!(event.actionable, event.occurred_at_ms, row.id.as_str(), event.event_id.as_str())),
        ];
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if counts.first().copied() == Some(1) {
            return Ok(DvrInsertOutcome::Inserted);
        }
        let existing = self
            .client()
            .query_consistent_map::<StateRow, _>(
                "SELECT state FROM dvr_recordings WHERE channel_id=$1 AND airing_start=$2",
                params!(row.channel_id.as_str(), row.airing_start),
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                StoreError::Database(
                    "dvr airing was neither inserted nor already present".to_owned(),
                )
            })?;
        DvrState::parse(&existing.state)
            .map(DvrInsertOutcome::Exists)
            .ok_or_else(|| StoreError::Database(format!("unknown dvr state `{}`", existing.state)))
    }

    async fn get_dvr_recording(&self, id: &str) -> Result<Option<DvrRecording>, StoreError> {
        Ok(read_recordings(
            self,
            format!("SELECT {RECORDING_COLS} FROM dvr_recordings WHERE id = $1"),
            params!(id),
        )
        .await?
        .into_iter()
        .next())
    }

    /// One page of the schedule, keyed on `id`.
    ///
    /// Paging on the primary key rather than an offset means a row inserted or
    /// finished between two pages cannot make a third row skip past the reader
    /// — the guide ticks every fifteen seconds, so that is the normal case
    /// rather than a rare one.
    async fn get_dvr_recording_for_airing(
        &self,
        channel_id: &str,
        airing_start: i64,
    ) -> Result<Option<DvrRecording>, StoreError> {
        Ok(read_recordings(
            self,
            format!(
                "SELECT {RECORDING_COLS} FROM dvr_recordings \
                 WHERE channel_id = $1 AND airing_start = $2"
            ),
            params!(channel_id, airing_start),
        )
        .await?
        .into_iter()
        .next())
    }

    async fn list_dvr_recordings(
        &self,
        filter: &DvrRecordingFilter,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DvrRecording>, StoreError> {
        let (sql, values) = recording_page_statement(filter, after, limit);
        read_recordings(self, sql, values).await
    }

    /// Every row in one of these states, in the order the owner loop acts on
    /// them: whatever starts soonest, with `id` breaking the tie so two nodes
    /// replaying the same tick agree on which airing got the last tuner.
    async fn list_dvr_recordings_in(
        &self,
        states: &[DvrState],
    ) -> Result<Vec<DvrRecording>, StoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let mut binder = Binder::default();
        let list = binder.bind_states(states);
        read_recordings(
            self,
            format!(
                "SELECT {RECORDING_COLS} FROM dvr_recordings \
                 WHERE state IN ({list}) ORDER BY capture_start, id"
            ),
            binder.values,
        )
        .await
    }

    /// Move a row to a new state, if it is still where the caller left it.
    ///
    /// The `SET` list is exactly `state`, `state_reason`, `updated_at_ms` and
    /// the columns the patch variant names — never the stop request and never
    /// the progress counters. That is not tidiness: the owner loop's transition
    /// and a viewer's Stop, or a 30-second progress write, land in either order
    /// on different nodes, and a write that touched a column it did not mean to
    /// would erase whichever arrived first.
    async fn transition_dvr_recording(
        &self,
        transition: &DvrTransition<'_>,
    ) -> Result<bool, StoreError> {
        if transition.from.is_empty() {
            // No state the row could be in would satisfy the caller, so there
            // is nothing to ask the cluster.
            return Ok(false);
        }
        let (sql, values) = transition_statement(
            transition.id,
            transition.from,
            transition.to,
            transition.reason,
            &transition.patch,
            transition.fence_generation,
            transition.now_ms,
        );
        let changed = self.client().execute(sql, values).await?;
        Ok(changed == 1)
    }

    async fn transition_dvr_recording_with_event(
        &self,
        transition: &DvrTransition<'_>,
        event: &DvrEventInput,
    ) -> Result<bool, StoreError> {
        if transition.from.is_empty() {
            return Ok(false);
        }
        if !event.validate() {
            return Err(StoreError::Database(
                "invalid DVR lifecycle event".to_owned(),
            ));
        }
        let counts = self
            .client()
            .txn(transition_event_statements(transition, event))
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.last().copied() == Some(1))
    }

    /// Two columns, and no others.
    ///
    /// A capture reports its size every thirty seconds while a viewer may press
    /// Stop at any point in between. Keeping this write to `bytes` and
    /// `last_progress_ms` is what guarantees the two cannot overwrite each
    /// other, whichever order the log puts them in. The `recording` guard stops
    /// a late report from a worker that has already lost the row from reviving
    /// its byte count.
    async fn progress_dvr_recording(
        &self,
        id: &str,
        bytes: i64,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        self.execute(
            "UPDATE dvr_recordings SET bytes = $1, last_progress_ms = $2 \
             WHERE id = $3 AND state = 'recording'",
            params!(bytes, at_ms, id),
        )
        .await?;
        Ok(())
    }

    /// Record a viewer's Stop, once.
    ///
    /// `stop_requested_at_ms IS NULL` makes the write idempotent without a
    /// read: the first Stop wins and every later one changes nothing, so two
    /// impatient taps cannot rewrite who stopped the recording. The row is read
    /// back afterwards rather than assembled from the arguments, because the
    /// caller needs what the cluster actually holds — including the case where
    /// the request landed on a row that had already finished.
    async fn request_dvr_stop(
        &self,
        id: &str,
        at_ms: i64,
        by_user: i64,
    ) -> Result<Option<DvrRecording>, StoreError> {
        self.execute(
            // `updated_at_ms` moves because a stop request is a change of
            // intent someone made, unlike a progress write, which is the
            // capture reporting on itself and deliberately stamps nothing.
            "UPDATE dvr_recordings SET stop_requested_at_ms = $1, \
               stop_requested_by_user_id = $2, updated_at_ms = $1 \
             WHERE id = $3 AND state = 'recording' AND stop_requested_at_ms IS NULL",
            params!(at_ms, by_user, id),
        )
        .await?;
        Ok(read_recordings(
            self,
            format!("SELECT {RECORDING_COLS} FROM dvr_recordings WHERE id = $1"),
            params!(id),
        )
        .await?
        .into_iter()
        .next())
    }

    async fn request_dvr_stop_with_event(
        &self,
        id: &str,
        at_ms: i64,
        by_user: i64,
        event: &DvrEventInput,
    ) -> Result<Option<DvrRecording>, StoreError> {
        if !event.validate() {
            return Err(StoreError::Database(
                "invalid DVR lifecycle event".to_owned(),
            ));
        }
        let statements = vec![
            ("INSERT OR IGNORE INTO dvr_event_heads
              (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
               history_started_at_ms,history_has_gap,pruned_through_sequence)
              SELECT id,1,0,NULL,$1,1,0 FROM dvr_recordings
               WHERE id=$2 AND state='recording' AND stop_requested_at_ms IS NULL".to_owned(),
             params!(event.occurred_at_ms, id)),
            ("INSERT INTO dvr_events
              (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
              SELECT r.id,h.next_sequence,$1,$2,$3,$4,$5,$6,$7
                FROM dvr_recordings r JOIN dvr_event_heads h ON h.recording_id=r.id
               WHERE r.id=$8 AND r.state='recording' AND r.stop_requested_at_ms IS NULL
                 AND NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id=$1)".to_owned(),
             params!(event.event_id.as_str(), event.kind.as_str(), event.occurred_at_ms,
                 event.attempt, event.actor_user_id, event.reason_code.as_deref(),
                 event.facts_json.as_str(), id)),
            ("UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
                latest_attention_sequence=CASE WHEN $1 THEN next_sequence ELSE latest_attention_sequence END,
                latest_attention_at_ms=CASE WHEN $1 THEN $2 ELSE latest_attention_at_ms END
              WHERE recording_id=$3 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$4)".to_owned(),
             params!(event.actionable, event.occurred_at_ms, id, event.event_id.as_str())),
            ("UPDATE dvr_recordings SET stop_requested_at_ms=$1,
                stop_requested_by_user_id=$2,updated_at_ms=$1
              WHERE id=$3 AND state='recording' AND stop_requested_at_ms IS NULL".to_owned(),
             params!(at_ms, by_user, id)),
        ];
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(read_recordings(
            self,
            format!("SELECT {RECORDING_COLS} FROM dvr_recordings WHERE id=$1"),
            params!(id),
        )
        .await?
        .into_iter()
        .next())
    }

    /// Re-point rule rows after reconciliation has decided which rule owns
    /// them.
    ///
    /// Only rows still pending are touched. A capture that has already started,
    /// or a finished one sitting in the library, belongs to the history of the
    /// rule that asked for it, and rewriting its `rule_id` would move a
    /// recording nobody asked to move.
    async fn repoint_dvr_rule_rows(
        &self,
        changes: &[(String, Option<String>)],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if changes.is_empty() {
            return Ok(());
        }
        let pending = pending_state_literals();
        let sql = format!(
            "UPDATE dvr_recordings SET rule_id = $1, updated_at_ms = $2 \
             WHERE id = $3 AND state IN ({pending})"
        );
        let statements = changes
            .iter()
            .map(|(recording_id, rule_id)| {
                (
                    sql.clone(),
                    params!(rule_id.as_deref(), now_ms, recording_id.as_str()),
                )
            })
            .collect::<Vec<_>>();
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    async fn link_dvr_recording_media(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let changed = self
            .execute(
                "UPDATE dvr_recordings SET item_id = $1, file_id = $2, updated_at_ms = $3 \
                 WHERE id = $4",
                params!(item_id, file_id, now_ms, recording_id),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn link_dvr_recording_media_with_event(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
        event: &DvrEventInput,
    ) -> Result<bool, StoreError> {
        if !event.validate() {
            return Err(StoreError::Database(
                "invalid DVR lifecycle event".to_owned(),
            ));
        }
        let statements = vec![
            ("INSERT OR IGNORE INTO dvr_event_heads
              (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
               history_started_at_ms,history_has_gap,pruned_through_sequence)
              SELECT id,1,0,NULL,$1,1,0 FROM dvr_recordings
               WHERE id=$2 AND (item_id IS NOT $3 OR file_id IS NOT $4)".to_owned(),
             params!(event.occurred_at_ms, recording_id, item_id, file_id)),
            ("INSERT INTO dvr_events
              (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
              SELECT r.id,h.next_sequence,$1,$2,$3,$4,$5,$6,$7
                FROM dvr_recordings r JOIN dvr_event_heads h ON h.recording_id=r.id
               WHERE r.id=$8 AND (r.item_id IS NOT $9 OR r.file_id IS NOT $10)
                 AND NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id=$1)".to_owned(),
             params!(event.event_id.as_str(), event.kind.as_str(), event.occurred_at_ms,
                 event.attempt, event.actor_user_id, event.reason_code.as_deref(),
                 event.facts_json.as_str(), recording_id, item_id, file_id)),
            ("UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
                latest_attention_sequence=CASE WHEN $1 THEN next_sequence ELSE latest_attention_sequence END,
                latest_attention_at_ms=CASE WHEN $1 THEN $2 ELSE latest_attention_at_ms END
              WHERE recording_id=$3 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$4)".to_owned(),
             params!(event.actionable, event.occurred_at_ms, recording_id, event.event_id.as_str())),
            ("UPDATE dvr_recordings SET item_id=$1,file_id=$2,updated_at_ms=$3
              WHERE id=$4 AND (item_id IS NOT $1 OR file_id IS NOT $2)".to_owned(),
             params!(item_id, file_id, now_ms, recording_id)),
        ];
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.last().copied() == Some(1))
    }

    async fn append_dvr_observation_event(
        &self,
        recording_id: &str,
        owner_node_id: &str,
        attempt: i64,
        event: &DvrEventInput,
    ) -> Result<bool, StoreError> {
        if !event.validate() {
            return Err(StoreError::Database(
                "invalid DVR lifecycle event".to_owned(),
            ));
        }
        let statements = vec![
            ("INSERT OR IGNORE INTO dvr_event_heads
              (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
               history_started_at_ms,history_has_gap,pruned_through_sequence)
              SELECT id,1,0,NULL,$1,1,0 FROM dvr_recordings
               WHERE id=$2 AND state='recording' AND tuner_owner_node_id=$3 AND attempt=$4".to_owned(),
             params!(event.occurred_at_ms, recording_id, owner_node_id, attempt)),
            ("INSERT INTO dvr_events
              (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
              SELECT r.id,h.next_sequence,$1,$2,$3,$4,$5,$6,$7
                FROM dvr_recordings r JOIN dvr_event_heads h ON h.recording_id=r.id
               WHERE r.id=$8 AND r.state='recording' AND r.tuner_owner_node_id=$9 AND r.attempt=$10
                 AND NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id=$1)".to_owned(),
             params!(event.event_id.as_str(), event.kind.as_str(), event.occurred_at_ms,
                 event.attempt, event.actor_user_id, event.reason_code.as_deref(),
                 event.facts_json.as_str(), recording_id, owner_node_id, attempt)),
            ("UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
                latest_attention_sequence=CASE WHEN $1 THEN next_sequence ELSE latest_attention_sequence END,
                latest_attention_at_ms=CASE WHEN $1 THEN $2 ELSE latest_attention_at_ms END
              WHERE recording_id=$3 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$4)".to_owned(),
             params!(event.actionable, event.occurred_at_ms, recording_id, event.event_id.as_str())),
        ];
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.get(1).copied() == Some(1))
    }

    async fn list_dvr_events(
        &self,
        recording_id: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<DvrEventPage, StoreError> {
        let heads = self.client().query_consistent_map::<EventHeadRow, _>(
            "SELECT recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
                    history_started_at_ms,history_has_gap,pruned_through_sequence
               FROM dvr_event_heads WHERE recording_id=$1",
            params!(recording_id),
        ).await?;
        let head = heads.into_iter().next().map(Into::into);
        let limit = limit.clamp(1, DVR_EVENT_PAGE_MAX);
        let (sql, values) =
            if let Some(after) = after {
                ("SELECT recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,
                    reason_code,facts_json FROM dvr_events
               WHERE recording_id=$1 AND sequence>$2 ORDER BY sequence ASC LIMIT $3".to_owned(),
             params!(recording_id, after, limit))
            } else {
                ("SELECT recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,
                    reason_code,facts_json FROM dvr_events
               WHERE recording_id=$1 AND sequence<$2 ORDER BY sequence DESC LIMIT $3".to_owned(),
             params!(recording_id, before.unwrap_or(i64::MAX), limit))
            };
        let rows = self
            .client()
            .query_consistent_map::<EventRow, _>(sql, values)
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DvrEventPage { rows, head })
    }

    async fn acknowledge_dvr_attention(
        &self,
        recording_id: &str,
        user_id: i64,
        through_sequence: i64,
        acknowledged_at_ms: i64,
        legacy_baseline: &DvrEventInput,
    ) -> Result<Option<i64>, StoreError> {
        if through_sequence < 0 || !legacy_baseline.validate() {
            return Ok(None);
        }
        let mut statements = Vec::new();
        if through_sequence == 0 {
            statements.extend([
                ("INSERT OR IGNORE INTO dvr_event_heads
                  (recording_id,next_sequence,latest_attention_sequence,latest_attention_at_ms,
                   history_started_at_ms,history_has_gap,pruned_through_sequence)
                  SELECT id,1,0,NULL,$1,1,0 FROM dvr_recordings
                   WHERE id=$2 AND (state IN ('failed','missed') OR
                         (state='partial' AND stopped_by_user_id IS NULL))".to_owned(),
                 params!(legacy_baseline.occurred_at_ms, recording_id)),
                ("INSERT INTO dvr_events
                  (recording_id,sequence,event_id,kind,occurred_at_ms,attempt,actor_user_id,reason_code,facts_json)
                  SELECT h.recording_id,h.next_sequence,$1,$2,$3,$4,$5,$6,$7
                    FROM dvr_event_heads h WHERE h.recording_id=$8
                     AND h.next_sequence=1
                     AND NOT EXISTS(SELECT 1 FROM dvr_events WHERE event_id=$1)".to_owned(),
                 params!(legacy_baseline.event_id.as_str(), legacy_baseline.kind.as_str(),
                     legacy_baseline.occurred_at_ms, legacy_baseline.attempt,
                     legacy_baseline.actor_user_id, legacy_baseline.reason_code.as_deref(),
                     legacy_baseline.facts_json.as_str(), recording_id)),
                ("UPDATE dvr_event_heads SET next_sequence=next_sequence+1,
                    latest_attention_sequence=next_sequence,latest_attention_at_ms=$1
                  WHERE recording_id=$2 AND next_sequence=(SELECT sequence FROM dvr_events WHERE event_id=$3)".to_owned(),
                 params!(legacy_baseline.occurred_at_ms, recording_id, legacy_baseline.event_id.as_str())),
            ]);
        }
        statements.push((
            "INSERT INTO dvr_attention_acks
             (user_id,recording_id,through_sequence,acknowledged_at_ms)
             SELECT $1,h.recording_id,
                    CASE WHEN $2=0 THEN h.latest_attention_sequence ELSE $2 END,$3
               FROM dvr_event_heads h WHERE h.recording_id=$4
                AND (CASE WHEN $2=0 THEN h.latest_attention_sequence ELSE $2 END)
                    BETWEEN 0 AND h.next_sequence-1
             ON CONFLICT(user_id,recording_id) DO UPDATE SET
               through_sequence=MAX(through_sequence,excluded.through_sequence),
               acknowledged_at_ms=CASE WHEN excluded.through_sequence>through_sequence
                 THEN excluded.acknowledged_at_ms ELSE acknowledged_at_ms END"
                .to_owned(),
            params!(user_id, through_sequence, acknowledged_at_ms, recording_id),
        ));
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(self.client().query_consistent_map::<AckRow, _>(
            "SELECT through_sequence FROM dvr_attention_acks WHERE user_id=$1 AND recording_id=$2",
            params!(user_id, recording_id),
        ).await?.into_iter().next().map(|row| row.through_sequence))
    }

    async fn list_dvr_attention(
        &self,
        user_id: i64,
        after: Option<(i64, &str)>,
        limit: i64,
    ) -> Result<(Vec<DvrAttentionRow>, i64), StoreError> {
        let (after_at, after_id) = after.unwrap_or((i64::MAX, ""));
        let condition = "(r.state='conflict' OR r.state IN ('failed','missed') OR
             (r.state='partial' AND r.stopped_by_user_id IS NULL) OR
             COALESCE(h.latest_attention_sequence,0)>COALESCE(a.through_sequence,0))";
        let sql = format!(
            "SELECT {RECORDING_COLS},COALESCE(h.latest_attention_sequence,0) AS latest_attention_sequence,
                    COALESCE(h.latest_attention_at_ms,r.finished_at_ms,r.updated_at_ms) AS attention_at_ms,
                    COALESCE(a.through_sequence,0) AS acknowledged_through_sequence,
                    COUNT(*) OVER() AS total_count
               FROM dvr_recordings r
               LEFT JOIN dvr_event_heads h ON h.recording_id=r.id
               LEFT JOIN dvr_attention_acks a ON a.recording_id=r.id AND a.user_id=$1
              WHERE {condition}
                AND (COALESCE(h.latest_attention_at_ms,r.finished_at_ms,r.updated_at_ms)<$2 OR
                    (COALESCE(h.latest_attention_at_ms,r.finished_at_ms,r.updated_at_ms)=$3 AND r.id>$4))
              ORDER BY COALESCE(h.latest_attention_at_ms,r.finished_at_ms,r.updated_at_ms) DESC,r.id
              LIMIT $5"
        );
        let raw = self
            .client()
            .query_consistent_map::<AttentionRowRaw, _>(
                sql,
                params!(
                    user_id,
                    after_at,
                    after_at,
                    after_id,
                    limit.clamp(1, DVR_EVENT_PAGE_MAX)
                ),
            )
            .await?;
        let total = raw.first().map_or(0, |row| row.total_count);
        let rows = raw
            .into_iter()
            .map(|row| {
                Ok(DvrAttentionRow {
                    recording: row.recording.try_into()?,
                    latest_attention_sequence: row.latest_attention_sequence,
                    latest_attention_at_ms: row.latest_attention_at_ms,
                    acknowledged_through_sequence: row.acknowledged_through_sequence,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok((rows, total))
    }

    async fn prune_dvr_events(&self, cutoff_ms: i64, limit: i64) -> Result<i64, StoreError> {
        let counts = self
            .client()
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM dvr_events",
                params!(),
            )
            .await?;
        let total = counts.first().map_or(0, |row| row.count);
        let aged = self
            .client()
            .query_consistent_map::<CountRow, _>(
                "SELECT COUNT(*) AS count FROM dvr_events WHERE occurred_at_ms<$1",
                params!(cutoff_ms),
            )
            .await?
            .first()
            .map_or(0, |row| row.count);
        let remove = aged
            .max(total.saturating_sub(DVR_EVENT_SERVER_MAX))
            .min(limit.clamp(1, DVR_EVENT_PRUNE_BATCH));
        if remove == 0 {
            return Ok(0);
        }
        let victims = self
            .client()
            .query_consistent_map::<EventIdentityRow, _>(
                "SELECT recording_id,sequence FROM dvr_events
             ORDER BY occurred_at_ms,event_id LIMIT $1",
                params!(remove),
            )
            .await?;
        let mut statements = Vec::with_capacity(victims.len() * 2);
        for victim in &victims {
            statements.push((
                "DELETE FROM dvr_events WHERE recording_id=$1 AND sequence=$2".to_owned(),
                params!(victim.recording_id.as_str(), victim.sequence),
            ));
            statements.push((
                "UPDATE dvr_event_heads SET history_has_gap=1,
                pruned_through_sequence=MAX(pruned_through_sequence,$1) WHERE recording_id=$2"
                    .to_owned(),
                params!(victim.sequence, victim.recording_id.as_str()),
            ));
        }
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(victims.len() as i64)
    }

    async fn list_dvr_reminders(
        &self,
        user_id: i64,
        state: Option<DvrReminderState>,
    ) -> Result<Vec<DvrReminder>, StoreError> {
        match state {
            Some(state) => {
                read_reminders(
                    self,
                    format!(
                        "SELECT {REMINDER_COLS} FROM dvr_reminders \
                         WHERE user_id = $1 AND state = $2 ORDER BY airing_start, id"
                    ),
                    params!(user_id, state.as_str()),
                )
                .await
            }
            None => {
                read_reminders(
                    self,
                    format!(
                        "SELECT {REMINDER_COLS} FROM dvr_reminders \
                         WHERE user_id = $1 ORDER BY airing_start, id"
                    ),
                    params!(user_id),
                )
                .await
            }
        }
    }

    async fn list_dvr_reminders_in(
        &self,
        state: DvrReminderState,
    ) -> Result<Vec<DvrReminder>, StoreError> {
        read_reminders(
            self,
            format!(
                "SELECT {REMINDER_COLS} FROM dvr_reminders \
                 WHERE state = $1 ORDER BY airing_start, id"
            ),
            params!(state.as_str()),
        )
        .await
    }

    /// Write a reminder, creating it or replacing it wholesale.
    ///
    /// The per-user cap is a predicate of the insert for the same reason the
    /// rule cap is, and is likewise scoped to genuinely new rows: re-arming a
    /// reminder that already exists is an edit, not another reminder.
    async fn put_dvr_reminder(&self, reminder: &DvrReminder) -> Result<bool, StoreError> {
        let changed = self
            .client()
            .execute(
                format!(
                    "INSERT INTO dvr_reminders ({REMINDER_COLS}) \
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13 \
                     WHERE EXISTS(SELECT 1 FROM dvr_reminders WHERE id = $1) \
                        OR (SELECT COUNT(*) FROM dvr_reminders \
                              WHERE user_id = $2 AND state IN ('armed', 'fired')) < $14 \
                     ON CONFLICT(id) DO UPDATE SET \
                       user_id = excluded.user_id, channel_id = excluded.channel_id, \
                       guide_number = excluded.guide_number, \
                       airing_start = excluded.airing_start, airing_end = excluded.airing_end, \
                       title = excluded.title, lead_s = excluded.lead_s, \
                       state = excluded.state, fired_at_ms = excluded.fired_at_ms, \
                       acked_at_ms = excluded.acked_at_ms, \
                       updated_at_ms = excluded.updated_at_ms"
                ),
                params!(
                    reminder.id.as_str(),
                    reminder.user_id,
                    reminder.channel_id.as_str(),
                    reminder.guide_number.as_str(),
                    reminder.airing_start,
                    reminder.airing_end,
                    reminder.title.as_str(),
                    reminder.lead_s,
                    reminder.state.as_str(),
                    reminder.fired_at_ms,
                    reminder.acked_at_ms,
                    reminder.created_at_ms,
                    reminder.updated_at_ms,
                    DVR_REMINDERS_PER_USER_MAX
                ),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn delete_dvr_reminder(&self, user_id: i64, id: &str) -> Result<bool, StoreError> {
        let changed = self
            .execute(
                "DELETE FROM dvr_reminders WHERE id = $1 AND user_id = $2",
                params!(id, user_id),
            )
            .await?;
        Ok(changed == 1)
    }

    /// Fire the reminders whose lead time has arrived, then expire the ones
    /// that never will.
    ///
    /// The order is the contract. A viewer who set a five-minute reminder for a
    /// programme that starts in one minute is owed the notification, not
    /// silence — so the fire pass runs first and takes the row out of `armed`,
    /// and the expire pass that follows cannot swallow it. Running expiry first
    /// would drop exactly the reminders that mattered most.
    ///
    /// Both passes take the clock as an argument: each voter runs the same
    /// statement, and `unixepoch()` would give each of them a different answer.
    async fn transition_dvr_reminders(
        &self,
        now_seconds: i64,
        now_ms: i64,
    ) -> Result<Vec<DvrReminder>, StoreError> {
        let fired = self
            .client()
            .execute_returning_map::<_, ReminderRow>(
                format!(
                    // A reminder's whole job is to warn someone *before* the
                    // programme starts, so the lead time opens the window and
                    // the start closes it. Without the upper bound a reminder
                    // set for a programme already under way would fire on the
                    // next tick and read as an alert about something half over.
                    "UPDATE dvr_reminders SET state = 'fired', fired_at_ms = $1, \
                       updated_at_ms = $1 \
                     WHERE state = 'armed' AND airing_start - lead_s <= $2 \
                       AND airing_start >= $2 \
                     RETURNING {REMINDER_COLS}"
                ),
                params!(now_ms, now_seconds),
            )
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        // Once the programme is on, the reminder is spent — whether it fired
        // and nobody acknowledged it, or its lead time was so short that the
        // start arrived first. Both are `expired`; neither is a row a client
        // should still be drawing an overlay for.
        self.execute(
            "UPDATE dvr_reminders SET state = 'expired', updated_at_ms = $1 \
             WHERE state IN ('armed', 'fired') AND airing_start < $2",
            params!(now_ms, now_seconds),
        )
        .await?;
        let mut reminders = fired
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<DvrReminder>, StoreError>>()?;
        // `RETURNING` promises no order, and the caller turns this list into
        // notifications; sort it so both backends, and two replays of the same
        // tick, deliver them the same way round.
        reminders.sort_by(|left, right| {
            (left.airing_start, &left.id).cmp(&(right.airing_start, &right.id))
        });
        Ok(reminders)
    }

    /// Move one reminder between states, when it is still in the state the
    /// caller saw.
    ///
    /// `user_id` is the ownership check, and it is `Option` because the owner
    /// loop legitimately acts on reminders it does not own while a route acting
    /// for a person must not. Expressing it as `$5 IS NULL OR user_id = $5`
    /// keeps both cases in one statement, so the ownership test cannot drift
    /// out of step with the state test.
    ///
    /// The timestamp columns follow the target state rather than being written
    /// unconditionally: an ack records when it was acked, and leaves the moment
    /// the reminder fired alone.
    async fn set_dvr_reminder_state(
        &self,
        user_id: Option<i64>,
        id: &str,
        from: DvrReminderState,
        to: DvrReminderState,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let changed = self
            .execute(
                "UPDATE dvr_reminders SET state = $1, \
                   fired_at_ms = CASE WHEN $1 = 'fired' THEN $2 ELSE fired_at_ms END, \
                   acked_at_ms = CASE WHEN $1 = 'acked' THEN $2 ELSE acked_at_ms END, \
                   updated_at_ms = $2 \
                 WHERE id = $3 AND state = $4 AND ($5 IS NULL OR user_id = $5)",
                params!(to.as_str(), now_ms, id, from.as_str(), user_id),
            )
            .await?;
        Ok(changed == 1)
    }
}

#[cfg(test)]
mod schema_tests {
    use super::{
        migration_statements, pending_state_literals, recording_page_statement,
        transition_statement, validate_sql, DvrRecordingFilter, DvrState, DvrStatePatch,
    };

    #[test]
    fn the_migration_creates_every_dvr_table_and_index() {
        let statements = migration_statements().expect("valid replicated migration");
        let created = statements
            .iter()
            .map(|(statement, _)| statement.as_str())
            .collect::<Vec<_>>();
        for table in ["dvr_rules", "dvr_recordings", "dvr_reminders"] {
            assert!(
                created
                    .iter()
                    .any(|statement| statement.starts_with("CREATE TABLE")
                        && statement.contains(table)),
                "{table} is missing from the replicated migration"
            );
        }
        assert!(
            created
                .iter()
                .all(|statement| !statement.trim_end().ends_with(';')),
            "hiqlite takes one statement per entry, without its terminator"
        );
    }

    #[test]
    fn reconciliation_names_every_pending_state() {
        let pending = pending_state_literals();
        for state in ["scheduled", "conflict", "withdrawn", "stale"] {
            assert!(
                pending.contains(&format!("'{state}'")),
                "{state} is pending but reconciliation would skip it"
            );
        }
        assert!(!pending.contains("'recording'"));
    }

    /// The repo-wide census reads statements out of the source text, so a
    /// statement assembled one column at a time never reaches it. These two are
    /// the only such statements in this slice, and every shape they can take is
    /// held to the same rule here.
    #[test]
    fn every_assembled_statement_introduces_its_placeholders_in_order() {
        let patches = [
            DvrStatePatch::None,
            DvrStatePatch::Started {
                attempt: 1,
                tuner_owner_node_id: "node".to_owned(),
                started_at_ms: 1,
                late_start_s: 0,
                path: "/dvr/a.ts".to_owned(),
            },
            DvrStatePatch::Reattempt {
                attempt: 2,
                gap_s: 3,
            },
            DvrStatePatch::Finished {
                finished_at_ms: 9,
                bytes: 10,
                gap_s: 0,
                path: None,
                stopped_by_user_id: Some(4),
            },
            DvrStatePatch::Rule { rule_id: None },
            DvrStatePatch::Purged,
        ];
        for patch in &patches {
            for fence in [None, Some(7)] {
                let (sql, values) = transition_statement(
                    "rec",
                    &[DvrState::Scheduled, DvrState::Conflict],
                    DvrState::Recording,
                    Some("tuner claimed"),
                    patch,
                    fence,
                    1_000,
                );
                validate_sql(&sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(
                    values.len(),
                    sql.matches('$').count(),
                    "every bound value is named exactly once: {sql}"
                );
            }
        }

        for states in [Vec::new(), vec![DvrState::Done, DvrState::Partial]] {
            for include_deleted in [false, true] {
                for before_capture_start in [None, Some(500)] {
                    let filter = DvrRecordingFilter {
                        states: states.clone(),
                        include_deleted,
                        before_capture_start,
                    };
                    let (sql, values) = recording_page_statement(&filter, Some("rec"), 1_000);
                    validate_sql(&sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
                    assert_eq!(values.len(), sql.matches('$').count(), "{sql}");
                }
            }
        }
    }
}
