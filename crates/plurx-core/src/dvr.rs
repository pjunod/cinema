//! Recording rules, scheduled airings, and reminders.
//!
//! This module is deliberately free of HTTP, tuner and filesystem concerns.
//! Both Store backends persist the same rows, one owner loop acts on them, and
//! every client renders the same three shapes.
//!
//! Two facts shape everything here:
//!
//! 1. **An airing is `(channel_id, airing_start)`, for its whole life.** One
//!    unique index covers every state, so a one-off request, four rules that
//!    all match the same broadcast, and the viewer's later decision to skip it
//!    all land on one row. Rule expansion is insert-if-absent and never an
//!    upsert, which is what stops a cancelled recording reappearing on the
//!    next fifteen-second tick.
//! 2. **The owner loop is the only writer of the live states.** Routes on any
//!    node write intent — `scheduled`, `cancelled`, a stop request — and the
//!    owner alone moves a row through `recording` to a terminal state, because
//!    it is the only process holding the file and the tuner.

use serde::{Deserialize, Serialize};

/// Rules are a per-server resource, not a per-user one: they compete for the
/// same tuners, so their priority order has to be total.
pub const DVR_RULES_MAX: i64 = 200;
pub const DVR_RULE_NAME_MAX: usize = 80;
pub const DVR_MATCH_VALUE_MAX: usize = 200;
pub const DVR_RECORDINGS_LIST_PAGE: i64 = 100;
pub const DVR_REMINDERS_PER_USER_MAX: i64 = 200;
/// How far ahead the schedule is planned and served. The guide horizon is the
/// real ceiling; this bounds the answer when the guide reaches further.
pub const DVR_SCHEDULE_DAYS_MAX: i64 = 14;
pub const DVR_PAD_MAX_S: i64 = 3600;
pub const DVR_LEAD_MAX_S: i64 = 3600;
/// An airing with less than a minute of capture left is not worth a tuner, a
/// file and a library item. It is also the cut-off that stops a restarted
/// owner recording whatever happens to be on air in place of a programme that
/// finished hours ago.
pub const DVR_MIN_USEFUL_S: i64 = 60;
pub const DVR_WEBHOOK_QUEUE: usize = 64;
/// A transport that fails every tick must not fill the DVR root with parts.
/// The ninth failure finishes the row `failed`.
pub const DVR_ATTEMPTS_MAX: i64 = 8;
/// Defaults for a new rule and for a one-off recorded straight from a cell.
pub const DVR_DEFAULT_PAD_START_S: i64 = 60;
pub const DVR_DEFAULT_PAD_END_S: i64 = 120;
pub const DVR_DEFAULT_REMINDER_LEAD_S: i64 = 300;
pub const DVR_DEFAULT_TUNER_RESERVE: u8 = 1;
pub const DVR_DEFAULT_FREE_FLOOR_GB: i64 = 50;
/// `original_air_date` within this of the airing counts as a first run when
/// the source carries no explicit `<new/>` marker.
pub const DVR_NEW_WINDOW_S: i64 = 7 * 24 * 60 * 60;

/// Durable DVR entities, shared verbatim by both Store backends.
///
/// Every clock and random value is supplied by the application: `unixepoch()`
/// and `random()` would each produce a different answer on every voter and
/// break replication determinism, and `AUTOINCREMENT` would make a row's
/// identity depend on which voter executed first.
pub(crate) const DVR_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dvr_rules (
    id               TEXT PRIMARY KEY,
    owner_user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    priority         INTEGER NOT NULL,
    name             TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    match_mode       TEXT NOT NULL CHECK (match_mode IN ('series_id', 'title')),
    match_value      TEXT NOT NULL CHECK (length(match_value) BETWEEN 1 AND 200),
    channel_id       TEXT,
    new_only         INTEGER NOT NULL DEFAULT 1 CHECK (new_only IN (0,1)),
    keep_mode        TEXT NOT NULL CHECK (keep_mode IN ('all', 'last_n', 'until_watched', 'days')),
    keep_value       INTEGER NOT NULL DEFAULT 0 CHECK (keep_value >= 0),
    pad_start_s      INTEGER NOT NULL CHECK (pad_start_s BETWEEN 0 AND 3600),
    pad_end_s        INTEGER NOT NULL CHECK (pad_end_s BETWEEN 0 AND 3600),
    enabled          INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    created_at_ms    INTEGER NOT NULL,
    updated_at_ms    INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS dvr_rules_priority ON dvr_rules(priority);
CREATE INDEX IF NOT EXISTS dvr_rules_owner ON dvr_rules(owner_user_id, id);

CREATE TABLE IF NOT EXISTS dvr_recordings (
    id                        TEXT PRIMARY KEY,
    origin                    TEXT NOT NULL CHECK (origin IN ('manual', 'rule')),
    rule_id                   TEXT REFERENCES dvr_rules(id) ON DELETE SET NULL,
    requested_by_user_id      INTEGER REFERENCES users(id) ON DELETE SET NULL,
    channel_id                TEXT NOT NULL,
    guide_number              TEXT NOT NULL,
    channel_name              TEXT NOT NULL,
    airing_start              INTEGER NOT NULL,
    airing_end                INTEGER NOT NULL,
    capture_start             INTEGER NOT NULL,
    capture_end               INTEGER NOT NULL,
    title                     TEXT NOT NULL,
    episode_title             TEXT,
    episode                   TEXT,
    synopsis                  TEXT,
    image_url                 TEXT,
    original_air_date         TEXT,
    series_id                 TEXT,
    programme_id              TEXT,
    state                     TEXT NOT NULL CHECK (state IN
                                ('scheduled','conflict','withdrawn','stale','recording',
                                 'done','partial','failed','missed','cancelled','deleted')),
    state_reason              TEXT,
    attempt                   INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    gap_s                     INTEGER NOT NULL DEFAULT 0 CHECK (gap_s >= 0),
    late_start_s              INTEGER NOT NULL DEFAULT 0 CHECK (late_start_s >= 0),
    tuner_owner_node_id       TEXT,
    path                      TEXT,
    bytes                     INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0),
    last_progress_ms          INTEGER,
    stop_requested_at_ms      INTEGER,
    stop_requested_by_user_id INTEGER,
    item_id                   INTEGER,
    file_id                   INTEGER,
    started_at_ms             INTEGER,
    finished_at_ms            INTEGER,
    stopped_by_user_id        INTEGER,
    created_at_ms             INTEGER NOT NULL,
    updated_at_ms             INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS dvr_recordings_airing
    ON dvr_recordings(channel_id, airing_start);
CREATE INDEX IF NOT EXISTS dvr_recordings_state
    ON dvr_recordings(state, capture_start);
CREATE INDEX IF NOT EXISTS dvr_recordings_rule
    ON dvr_recordings(rule_id, state);

CREATE TABLE IF NOT EXISTS dvr_reminders (
    id             TEXT PRIMARY KEY,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id     TEXT NOT NULL,
    guide_number   TEXT NOT NULL,
    airing_start   INTEGER NOT NULL,
    airing_end     INTEGER NOT NULL,
    title          TEXT NOT NULL,
    lead_s         INTEGER NOT NULL CHECK (lead_s BETWEEN 0 AND 3600),
    state          TEXT NOT NULL CHECK (state IN ('armed', 'fired', 'acked', 'expired', 'moved')),
    fired_at_ms    INTEGER,
    acked_at_ms    INTEGER,
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS dvr_reminders_user_airing
    ON dvr_reminders(user_id, channel_id, airing_start);
CREATE INDEX IF NOT EXISTS dvr_reminders_due ON dvr_reminders(state, airing_start);
"#;

/// Append-only DVR lifecycle history and per-user review position.
///
/// This is a separate migration from [`DVR_SCHEMA`]: that schema shipped as
/// SQLite v57 / replicated v37 and must remain byte-for-byte stable. Event
/// sequence allocation is always performed in the same transaction as the
/// event-producing state change.
pub(crate) const DVR_EVENT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dvr_event_heads (
    recording_id TEXT PRIMARY KEY REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    next_sequence INTEGER NOT NULL CHECK (next_sequence >= 1),
    latest_attention_sequence INTEGER NOT NULL DEFAULT 0,
    latest_attention_at_ms INTEGER,
    history_started_at_ms INTEGER NOT NULL,
    history_has_gap INTEGER NOT NULL DEFAULT 0 CHECK (history_has_gap IN (0,1)),
    pruned_through_sequence INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS dvr_events (
    recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    event_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    attempt INTEGER,
    actor_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    reason_code TEXT,
    facts_json TEXT NOT NULL CHECK (length(facts_json) <= 4096),
    PRIMARY KEY (recording_id, sequence)
) STRICT;
CREATE INDEX IF NOT EXISTS dvr_events_recent
    ON dvr_events(occurred_at_ms DESC, event_id DESC);

CREATE TABLE IF NOT EXISTS dvr_attention_acks (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
    through_sequence INTEGER NOT NULL CHECK (through_sequence >= 0),
    acknowledged_at_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, recording_id)
) STRICT;
"#;

pub const DVR_EVENT_PAGE_DEFAULT: i64 = 50;
pub const DVR_EVENT_PAGE_MAX: i64 = 100;
pub const DVR_EVENT_FACTS_MAX: usize = 4096;
pub const DVR_EVENT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
pub const DVR_EVENT_SERVER_MAX: i64 = 100_000;
pub const DVR_EVENT_PRUNE_BATCH: i64 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvrEventInput {
    pub event_id: String,
    pub kind: String,
    pub occurred_at_ms: i64,
    pub attempt: Option<i64>,
    pub actor_user_id: Option<i64>,
    pub reason_code: Option<String>,
    /// A bounded JSON object. It is persisted as supplied so replicated voters
    /// never serialize a map in different orders.
    pub facts_json: String,
    pub actionable: bool,
}

impl DvrEventInput {
    pub fn validate(&self) -> bool {
        !self.event_id.is_empty()
            && self.event_id.len() <= 128
            && !self.kind.is_empty()
            && self.kind.len() <= 64
            && self.facts_json.len() <= DVR_EVENT_FACTS_MAX
            && serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&self.facts_json)
                .is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvrEvent {
    pub recording_id: String,
    pub sequence: i64,
    pub event_id: String,
    pub kind: String,
    pub occurred_at_ms: i64,
    pub attempt: Option<i64>,
    pub actor_user_id: Option<i64>,
    pub reason_code: Option<String>,
    pub facts: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvrEventHead {
    pub recording_id: String,
    pub next_sequence: i64,
    pub latest_attention_sequence: i64,
    pub latest_attention_at_ms: Option<i64>,
    pub history_started_at_ms: i64,
    pub history_has_gap: bool,
    pub pruned_through_sequence: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvrEventPage {
    pub rows: Vec<DvrEvent>,
    pub head: Option<DvrEventHead>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvrAttentionRow {
    pub recording: DvrRecording,
    pub latest_attention_sequence: i64,
    pub latest_attention_at_ms: i64,
    pub acknowledged_through_sequence: i64,
}

/// How a rule decides a guide programme is one of its episodes.
///
/// Two modes rather than one because the two guide sources differ in what
/// they can promise. A tier that carries `SeriesID` gives an exact answer that
/// survives a renamed programme; a tier that does not leaves only the title,
/// and a title is ambiguous enough across channels that it is locked to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvrMatchMode {
    SeriesId,
    Title,
}

impl DvrMatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SeriesId => "series_id",
            Self::Title => "title",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "series_id" => Some(Self::SeriesId),
            "title" => Some(Self::Title),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvrKeepMode {
    #[default]
    All,
    LastN,
    UntilWatched,
    Days,
}

impl DvrKeepMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::LastN => "last_n",
            Self::UntilWatched => "until_watched",
            Self::Days => "days",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "last_n" => Some(Self::LastN),
            "until_watched" => Some(Self::UntilWatched),
            "days" => Some(Self::Days),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvrRule {
    pub id: String,
    pub owner_user_id: i64,
    /// Lower wins. Unique across the server and renumbered on reorder, so the
    /// scheduler's answer to "which of these two airings gets the tuner" is
    /// total rather than a tie broken by row order.
    pub priority: i64,
    pub name: String,
    pub match_mode: DvrMatchMode,
    pub match_value: String,
    /// `None` matches any channel. Always set for a `Title` rule created from
    /// a guide cell.
    pub channel_id: Option<String>,
    pub new_only: bool,
    pub keep_mode: DvrKeepMode,
    pub keep_value: i64,
    pub pad_start_s: i64,
    pub pad_end_s: i64,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Who asked for this airing, and therefore who may take it away.
///
/// A rule owns the rows it materialised and may withdraw them when it is
/// edited or disabled; a person's one-off is theirs, and no rule edit touches
/// it. The two need different words for "this is no longer going to happen"
/// — `withdrawn` and `stale` — which is why origin is a stored fact rather
/// than an inference from `rule_id` being null.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvrOrigin {
    Manual,
    Rule,
}

impl DvrOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Rule => "rule",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "rule" => Some(Self::Rule),
            _ => None,
        }
    }
}

/// Where an airing is in its life.
///
/// `cancelled` is the only state a person writes and the owner loop may never
/// leave: it is a decision, and expansion skips it for good. Everything from
/// `recording` onward is written by the owner alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvrState {
    /// Planned, and a tuner is expected to be free.
    Scheduled,
    /// Planned, but no tuner is free for it. `state_reason` says why, and the
    /// row stays in the schedule so the UI can offer to reorder or skip it.
    Conflict,
    /// A rule row no enabled rule matches any more. Not `cancelled`: if a rule
    /// matches it again, expansion re-materialises it.
    Withdrawn,
    /// A manual row whose programme moved or was renamed. Surfaced rather than
    /// withdrawn, because it is the requester's to re-decide.
    Stale,
    Recording,
    Done,
    /// Captured, with a gap or a late start. Never silently called `done`.
    Partial,
    Failed,
    /// Never started, and can no longer usefully start.
    Missed,
    Cancelled,
    /// Terminal, and the file is gone.
    Deleted,
}

impl DvrState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Conflict => "conflict",
            Self::Withdrawn => "withdrawn",
            Self::Stale => "stale",
            Self::Recording => "recording",
            Self::Done => "done",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Missed => "missed",
            Self::Cancelled => "cancelled",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "scheduled" => Some(Self::Scheduled),
            "conflict" => Some(Self::Conflict),
            "withdrawn" => Some(Self::Withdrawn),
            "stale" => Some(Self::Stale),
            "recording" => Some(Self::Recording),
            "done" => Some(Self::Done),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            "missed" => Some(Self::Missed),
            "cancelled" => Some(Self::Cancelled),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }

    /// Waiting for its moment: reconciliation may still change it, and the
    /// scheduler still plans a tuner for it.
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            Self::Scheduled | Self::Conflict | Self::Withdrawn | Self::Stale
        )
    }

    /// Nothing else will happen to it on its own.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done
                | Self::Partial
                | Self::Failed
                | Self::Missed
                | Self::Cancelled
                | Self::Deleted
        )
    }

    /// A capture that produced a file the library can hold.
    pub fn has_media(self) -> bool {
        matches!(self, Self::Done | Self::Partial)
    }

    pub const PENDING: &'static [DvrState] = &[
        DvrState::Scheduled,
        DvrState::Conflict,
        DvrState::Withdrawn,
        DvrState::Stale,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvrRecording {
    pub id: String,
    pub origin: DvrOrigin,
    pub rule_id: Option<String>,
    pub requested_by_user_id: Option<i64>,
    pub channel_id: String,
    pub guide_number: String,
    pub channel_name: String,
    pub airing_start: i64,
    pub airing_end: i64,
    pub capture_start: i64,
    pub capture_end: i64,
    pub title: String,
    pub episode_title: Option<String>,
    pub episode: Option<String>,
    pub synopsis: Option<String>,
    pub image_url: Option<String>,
    pub original_air_date: Option<String>,
    pub series_id: Option<String>,
    pub programme_id: Option<String>,
    pub state: DvrState,
    pub state_reason: Option<String>,
    pub attempt: i64,
    pub gap_s: i64,
    pub late_start_s: i64,
    pub tuner_owner_node_id: Option<String>,
    pub path: Option<String>,
    pub bytes: i64,
    pub last_progress_ms: Option<i64>,
    pub stop_requested_at_ms: Option<i64>,
    pub stop_requested_by_user_id: Option<i64>,
    pub item_id: Option<i64>,
    pub file_id: Option<i64>,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub stopped_by_user_id: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What `insert_dvr_airing_if_absent` did.
///
/// `Exists` carries the state it found, which is how the caller distinguishes
/// "already scheduled, nothing to do" from "the viewer cancelled this, leave
/// it alone" without a second read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvrInsertOutcome {
    Inserted,
    Exists(DvrState),
}

/// Which rows a list call wants. Deliberately not a free-text SQL fragment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DvrRecordingFilter {
    /// Empty means every state except `deleted`.
    pub states: Vec<DvrState>,
    /// `deleted` rows are history: they are excluded unless asked for by name.
    pub include_deleted: bool,
    /// Only rows whose capture starts before this, when set.
    pub before_capture_start: Option<i64>,
}

/// The columns a state transition writes beyond `state`, `state_reason` and
/// `updated_at_ms`.
///
/// An enum rather than a bag of options so that every write site names which
/// set of columns it means to touch. The one column set that is deliberately
/// absent is the stop request: `progress_dvr_recording` and every transition
/// leave `stop_requested_at_ms` alone, so a progress write racing a viewer's
/// Stop can never erase it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DvrStatePatch {
    /// State and reason only.
    None,
    /// A capture opened its first or a later attempt.
    Started {
        attempt: i64,
        tuner_owner_node_id: String,
        started_at_ms: i64,
        late_start_s: i64,
        path: String,
    },
    /// A recovered capture opened attempt `N+1` after losing its worker.
    Reattempt { attempt: i64, gap_s: i64 },
    /// A capture closed. `stopped_by_user_id` is copied from the stop request
    /// the tick consumed, and is `None` for one that ran to its end.
    Finished {
        finished_at_ms: i64,
        bytes: i64,
        gap_s: i64,
        path: Option<String>,
        stopped_by_user_id: Option<i64>,
    },
    /// Reconciliation re-pointed a rule row at a different rule.
    Rule { rule_id: Option<String> },
    /// The files are gone. Clearing the path is what stops the owner's sweep
    /// trying to remove them again on every tick for ever.
    Purged,
}

/// One conditional move of a recording from one state to another.
///
/// A struct rather than seven positional arguments because the owner loop
/// makes this call from ten different steps, and at a call site
/// `from: &[DvrState::Recording]` beside `fence_generation` says what is being
/// asserted; a bare list of `&[…], …, Some(7), now` does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DvrTransition<'a> {
    pub id: &'a str,
    /// The states this write is willing to find. Anything else and it writes
    /// nothing and reports `false`.
    pub from: &'a [DvrState],
    pub to: DvrState,
    /// One sentence, for every state that is not plainly `scheduled`. It is
    /// what the schedule screen shows under a conflicted or missed row.
    pub reason: Option<&'a str>,
    pub patch: DvrStatePatch,
    /// The tuner configuration generation this write was planned under, when
    /// the caller is the owner loop. Checked inside the same statement, so a
    /// settings save cannot slip between the check and the write.
    pub fence_generation: Option<i64>,
    pub now_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvrReminderState {
    Armed,
    Fired,
    Acked,
    Expired,
    /// The airing this reminder named is no longer in the guide at that time.
    Moved,
}

impl DvrReminderState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Fired => "fired",
            Self::Acked => "acked",
            Self::Expired => "expired",
            Self::Moved => "moved",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "armed" => Some(Self::Armed),
            "fired" => Some(Self::Fired),
            "acked" => Some(Self::Acked),
            "expired" => Some(Self::Expired),
            "moved" => Some(Self::Moved),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvrReminder {
    pub id: String,
    pub user_id: i64,
    pub channel_id: String,
    pub guide_number: String,
    pub airing_start: i64,
    pub airing_end: i64,
    pub title: String,
    pub lead_s: i64,
    pub state: DvrReminderState,
    pub fired_at_ms: Option<i64>,
    pub acked_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// The cursor that continues a recordings page after this row.
///
/// `(airing_start, id)` rather than an id alone: the list is ordered by when
/// the programme aired, and a v4 UUID orders by nothing a person recognises.
pub fn recording_cursor(row: &DvrRecording) -> String {
    format!("{}:{}", row.airing_start, row.id)
}

/// Split a cursor back into its parts. An unparseable one starts from the top
/// rather than failing the request: a stale cursor in a bookmarked URL should
/// show the first page, not an error.
pub fn parse_recording_cursor(cursor: &str) -> Option<(i64, &str)> {
    let (start, id) = cursor.split_once(':')?;
    Some((start.parse().ok()?, id))
}

/// The title a `Title` rule stores and compares against.
///
/// Case, Unicode width and stray whitespace all vary between a guide's bulk
/// answer and its per-channel pages for the same programme, and a trailing
/// year is a disambiguator some sources add and others do not. Normalising
/// once, at both write and match time, is what makes "record this every week"
/// keep working when the source's spelling drifts.
pub fn normalise_title(value: &str) -> String {
    // Compatibility width folding by hand: the fullwidth forms are one
    // contiguous block mapped to ASCII by a fixed offset, and the ideographic
    // space is the other character a guide actually emits. A crate for this
    // would be a dependency for two ranges.
    let folded = value
        .chars()
        .map(|character| match character as u32 {
            0xFF01..=0xFF5E => char::from_u32(character as u32 - 0xFEE0).unwrap_or(character),
            0x3000 => ' ',
            _ => character,
        })
        .collect::<String>();
    let lowered = folded.to_lowercase();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_trailing_year(&collapsed).to_owned()
}

fn strip_trailing_year(value: &str) -> &str {
    let trimmed = value.trim_end();
    let Some(body) = trimmed.strip_suffix(')') else {
        return trimmed;
    };
    let Some((head, year)) = body.rsplit_once('(') else {
        return trimmed;
    };
    if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
        head.trim_end()
    } else {
        trimmed
    }
}

/// Whether an airing counts as a first run for a `new_only` rule.
///
/// The guide's own `<new/>` marker wins when it is there. Otherwise the air
/// date decides, and a programme with no air date at all is treated as new:
/// recording a repeat is a recoverable annoyance, and silently recording
/// nothing at all is the failure people actually complain about.
pub fn is_first_run(
    is_new: Option<bool>,
    original_air_date: Option<&str>,
    airing_start: i64,
) -> bool {
    if let Some(is_new) = is_new {
        return is_new;
    }
    let Some(air_date) = original_air_date.and_then(parse_air_date) else {
        return true;
    };
    airing_start.saturating_sub(air_date) <= DVR_NEW_WINDOW_S
}

/// `YYYY-MM-DD` at midnight UTC. The guide writes calendar days, and a day is
/// all the precision a seven-day window needs.
fn parse_air_date(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = value.get(0..4)?.parse::<i64>().ok()?;
    let month = value.get(5..7)?.parse::<i64>().ok()?;
    let day = value.get(8..10)?.parse::<i64>().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

/// Where a capture's files live, given the DVR root.
///
/// The recording id's first eight characters make the name unique per row, so
/// two channels showing programmes with the same title at the same moment
/// cannot collide — and neither can a rerun of a rule that was deleted and
/// recreated. Every component is sanitised: these strings come from a guide
/// host, and one of them reaching the filesystem as `../` would be a path
/// traversal with a broadcaster on the other end.
pub fn recording_basename(title: &str, airing_start: i64, guide_number: &str, id: &str) -> String {
    let title = sanitize_path_component(title, 120);
    let number = sanitize_path_component(guide_number, 16);
    let stamp = utc_stamp(airing_start);
    let short = id.chars().take(8).collect::<String>();
    format!("{title} - {stamp} - {number} - {short}")
}

pub fn recording_folder(title: &str) -> String {
    sanitize_path_component(title, 120)
}

/// `YYYY-MM-DD HHMM`, UTC. A filename is sorted and read by a person, so it
/// takes a readable stamp rather than an epoch.
fn utc_stamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let hour = rest / 3600;
    let minute = (rest % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}{minute:02}")
}

/// One path segment, safe on every filesystem plurx runs on.
fn sanitize_path_component(value: &str, limit: usize) -> String {
    let mut out = String::new();
    for character in value.chars() {
        let replacement = match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        };
        if out.len() + replacement.len_utf8() > limit {
            break;
        }
        out.push(replacement);
    }
    // Separators became spaces above, so `../../etc` is already harmless — but
    // it would still be *named* `.. .. etc`, and a name that looks like a
    // traversal attempt invites someone to decide it is one. Dots-only words
    // are directory entries rather than names, so they go; a trailing dot goes
    // too, because some filesystems drop it silently and two recordings would
    // then share one path.
    let collapsed = out
        .split_whitespace()
        .filter(|word| !word.chars().all(|c| c == '.'))
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed.trim_end_matches('.').trim();
    if trimmed.is_empty() {
        "Recording".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_normalise_across_the_spellings_one_source_uses_for_one_programme() {
        assert_eq!(normalise_title("  Kitchen   Table  "), "kitchen table");
        assert_eq!(normalise_title("Kitchen Table (2024)"), "kitchen table");
        assert_eq!(
            normalise_title("Kitchen Table (Special)"),
            "kitchen table (special)",
            "only a four-digit year is a disambiguator worth stripping"
        );
        assert_eq!(
            normalise_title("Ｋｉｔｃｈｅｎ　Ｔａｂｌｅ"),
            "kitchen table",
            "a fullwidth spelling is the same programme, and some guides send one"
        );
    }

    #[test]
    fn a_first_run_believes_the_guide_first_and_the_air_date_second() {
        let airing = 1_789_000_800; // 2026-09-13
        assert!(is_first_run(Some(true), Some("1999-01-01"), airing));
        assert!(!is_first_run(Some(false), Some("2026-09-13"), airing));
        assert!(is_first_run(None, Some("2026-09-10"), airing));
        assert!(!is_first_run(None, Some("2019-04-02"), airing));
        assert!(
            is_first_run(None, None, airing),
            "recording a repeat is recoverable; recording nothing is the complaint"
        );
    }

    #[test]
    fn a_basename_is_unique_per_row_and_survives_a_hostile_title() {
        let left = recording_basename("Kitchen Table", 1_789_000_800, "7.1", "abcdef0123456789");
        let right = recording_basename("Kitchen Table", 1_789_000_800, "5.1", "0123456789abcdef");
        assert_ne!(left, right, "two channels, one title, one instant");
        assert_eq!(left, "Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01");
        assert_eq!(
            recording_basename("../../etc", 1_789_000_800, "7.1", "abcdef0123456789"),
            "etc - 2026-09-10 0040 - 7.1 - abcdef01",
            "a guide host must not be able to name a path"
        );
        assert_eq!(recording_folder("  "), "Recording");
        assert_eq!(recording_folder("."), "Recording");
    }

    #[test]
    fn every_state_round_trips_through_its_wire_spelling() {
        for state in [
            DvrState::Scheduled,
            DvrState::Conflict,
            DvrState::Withdrawn,
            DvrState::Stale,
            DvrState::Recording,
            DvrState::Done,
            DvrState::Partial,
            DvrState::Failed,
            DvrState::Missed,
            DvrState::Cancelled,
            DvrState::Deleted,
        ] {
            assert_eq!(DvrState::parse(state.as_str()), Some(state));
            // The CHECK constraint in DVR_SCHEMA lists exactly these spellings.
            assert!(
                DVR_SCHEMA.contains(&format!("'{}'", state.as_str())),
                "{} is missing from the schema's CHECK",
                state.as_str()
            );
        }
    }
}
