//! Deterministic library-channel recipes and schedules.
//!
//! This module is deliberately free of HTTP and storage concerns. Both store
//! backends feed it the same bounded catalogue projection, and every client
//! receives schedules produced by this one implementation.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CHANNEL_NAME_MAX: usize = 80;
pub const CHANNEL_DESCRIPTION_MAX: usize = 500;
pub const CHANNEL_LIBRARIES_MAX: usize = 32;
pub const CHANNEL_RULE_VALUES_MAX: usize = 32;
pub const CHANNEL_RULE_VALUE_LENGTH_MAX: usize = 100;
pub const CHANNEL_EXPLICIT_IDS_MAX: usize = 1_000;
pub const CHANNEL_ELIGIBLE_POOL_MAX: usize = 10_000;
pub const CHANNEL_CANDIDATE_ROWS_MAX: usize = 100_000;
pub const CHANNEL_CANDIDATE_PAGE: usize = 500;
pub const CHANNEL_PREVIEW_PAGE_DEFAULT: usize = 50;
pub const CHANNEL_PREVIEW_PAGE_MAX: usize = 100;
pub const CHANNEL_GUIDE_CHANNELS_MAX: usize = 20;
pub const CHANNEL_GUIDE_OCCURRENCES_MAX: usize = 1_000;
pub const CHANNEL_FILE_DURATION_MAX_MS: i64 = 24 * 60 * 60 * 1_000;
pub const CHANNEL_BUILD_CLAIM_MS: i64 = 120_000;
pub const CHANNEL_BUILD_RENEW_MS: i64 = 30_000;
pub const CHANNEL_ACTIVATION_LEAD_MS: i64 = 30_000;
pub const CHANNELS_PER_USER_MAX: i64 = 50;
pub const CHANNELS_SERVER_MAX: i64 = 200;
pub const CHANNEL_REQUESTS_PER_USER_MAX: i64 = 1_000;
pub const CHANNEL_BUILD_QUEUE_MAX: usize = 200;
pub const CHANNEL_GENERATION_STAGE_MAX: usize = 200;
pub const CHANNEL_PRUNE_BATCH_MAX: i64 = 200;
pub const CHANNEL_ABANDONED_BUILD_RETENTION_MS: i64 = 60 * 60 * 1_000;
pub const CHANNEL_SUPERSEDED_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

/// Durable Library-channel entities, shared verbatim by both Store backends.
/// All clock/random values are supplied by the application.
pub(crate) const LIBRARY_CHANNELS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS library_channels (
    id                    TEXT PRIMARY KEY,
    owner_user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 320),
    description           TEXT NOT NULL CHECK (length(description) <= 2000),
    visibility            TEXT NOT NULL CHECK (visibility IN ('personal','shared')),
    enabled               INTEGER NOT NULL CHECK (enabled IN (0,1)),
    definition_revision   INTEGER NOT NULL CHECK (definition_revision > 0),
    recipe_json           TEXT NOT NULL,
    seed                  BLOB NOT NULL CHECK (length(seed) = 32),
    active_generation_id  TEXT,
    active_epoch_ms       INTEGER,
    pending_generation_id TEXT,
    pending_epoch_ms      INTEGER,
    build_state           TEXT NOT NULL DEFAULT 'draft'
                          CHECK (build_state IN ('draft','queued','building','ready','failed')),
    build_error_code      TEXT,
    build_error_message   TEXT,
    build_candidate_count INTEGER NOT NULL DEFAULT 0 CHECK (build_candidate_count >= 0),
    build_entry_count     INTEGER NOT NULL DEFAULT 0 CHECK (build_entry_count >= 0),
    build_last_attempt_ms INTEGER,
    build_last_success_ms INTEGER,
    last_auto_build_ms    INTEGER,
    created_at_ms         INTEGER NOT NULL,
    updated_at_ms         INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS library_channels_owner
    ON library_channels(owner_user_id, id);
CREATE INDEX IF NOT EXISTS library_channels_shared
    ON library_channels(visibility, enabled, id);
CREATE INDEX IF NOT EXISTS library_channels_build_state
    ON library_channels(build_state, build_last_attempt_ms, id);

CREATE TABLE IF NOT EXISTS library_channel_generations (
    id                     TEXT PRIMARY KEY,
    channel_id             TEXT NOT NULL REFERENCES library_channels(id) ON DELETE CASCADE,
    definition_revision    INTEGER NOT NULL CHECK (definition_revision > 0),
    algorithm_version      INTEGER NOT NULL CHECK (algorithm_version > 0),
    content_digest         TEXT NOT NULL CHECK (length(content_digest) = 64),
    state                  TEXT NOT NULL CHECK (state IN ('building','ready','failed')),
    entry_count            INTEGER NOT NULL DEFAULT 0 CHECK (entry_count >= 0),
    loop_duration_ms       INTEGER NOT NULL DEFAULT 0 CHECK (loop_duration_ms >= 0),
    build_claim_id         TEXT,
    build_claim_expires_ms INTEGER,
    created_at_ms          INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS library_channel_generations_channel
    ON library_channel_generations(channel_id, created_at_ms DESC);

CREATE TABLE IF NOT EXISTS library_channel_entries (
    generation_id       TEXT NOT NULL REFERENCES library_channel_generations(id) ON DELETE CASCADE,
    ordinal             INTEGER NOT NULL CHECK (ordinal >= 0),
    item_id             INTEGER NOT NULL CHECK (item_id > 0),
    file_id             INTEGER NOT NULL CHECK (file_id > 0),
    file_size           INTEGER NOT NULL CHECK (file_size >= 0),
    file_mtime          INTEGER NOT NULL CHECK (file_mtime >= 0),
    duration_ms         INTEGER NOT NULL CHECK (duration_ms > 0 AND duration_ms <= 86400000),
    cumulative_start_ms INTEGER NOT NULL CHECK (cumulative_start_ms >= 0),
    show_id             INTEGER,
    PRIMARY KEY (generation_id, ordinal),
    UNIQUE (generation_id, item_id)
) STRICT;

CREATE TABLE IF NOT EXISTS library_channel_favourites (
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id    TEXT NOT NULL REFERENCES library_channels(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, channel_id)
) STRICT;

CREATE TABLE IF NOT EXISTS library_channel_requests (
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_id     TEXT NOT NULL,
    operation_hash TEXT NOT NULL CHECK (length(operation_hash) = 64),
    channel_id     TEXT NOT NULL,
    result_revision INTEGER NOT NULL CHECK (result_revision >= 0),
    created_at_ms  INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (user_id, request_id)
) STRICT;
CREATE INDEX IF NOT EXISTS library_channel_requests_expiry
    ON library_channel_requests(expires_at_ms, user_id);

-- Persist the canonical finite-session recipe before producer placement.
-- The request-ledger delete trigger gives this row the same bounded lifetime.
CREATE TABLE IF NOT EXISTS library_channel_session_recipes (
    user_id         INTEGER NOT NULL,
    request_id      TEXT NOT NULL,
    incarnation_id  TEXT NOT NULL UNIQUE,
    recipe_json     TEXT NOT NULL CHECK (length(recipe_json) BETWEEN 2 AND 32768),
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (user_id, request_id)
) STRICT;
CREATE TRIGGER IF NOT EXISTS library_channel_session_recipes_request_delete
    AFTER DELETE ON media_session_requests
BEGIN
    DELETE FROM library_channel_session_recipes
     WHERE user_id = OLD.user_id AND request_id = OLD.request_id;
END;
"#;

/// Follow-up schema step for durable build acknowledgement lookups. The fields
/// themselves ship atomically with the first Library-channel table creation;
/// this index is a separate version so cluster protocol activation can fence
/// every node onto the build-state-aware implementation.
pub(crate) const LIBRARY_CHANNEL_BUILD_STATE_SCHEMA: &str = r#"
CREATE INDEX IF NOT EXISTS library_channels_build_state
    ON library_channels(build_state, build_last_attempt_ms, id);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelVisibility {
    Personal,
    Shared,
}

impl ChannelVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Shared => "shared",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "personal" => Some(Self::Personal),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOrdering {
    #[default]
    BalancedShuffle,
    ReleaseOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelItemKind {
    Movie,
    Episode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryChannelRecipe {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(deserialize_with = "deserialize_recipe_ids")]
    pub library_ids: Vec<i64>,
    pub kinds: Vec<ChannelItemKind>,
    pub genres_any: Vec<String>,
    pub tags_any: Vec<String>,
    pub keywords_any: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    #[serde(deserialize_with = "deserialize_recipe_ids")]
    pub include_item_ids: Vec<i64>,
    #[serde(deserialize_with = "deserialize_recipe_ids")]
    pub include_show_ids: Vec<i64>,
    #[serde(deserialize_with = "deserialize_recipe_ids")]
    pub exclude_item_ids: Vec<i64>,
    #[serde(deserialize_with = "deserialize_recipe_ids")]
    pub exclude_show_ids: Vec<i64>,
    #[serde(default)]
    pub ordering: ChannelOrdering,
    #[serde(default)]
    pub include_specials: bool,
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
    #[serde(default)]
    pub match_all_in_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryChannel {
    pub id: String,
    pub owner_user_id: i64,
    pub name: String,
    pub description: String,
    pub visibility: ChannelVisibility,
    pub enabled: bool,
    pub revision: i64,
    pub recipe: LibraryChannelRecipe,
    pub seed: [u8; 32],
    pub active_generation_id: Option<String>,
    pub active_epoch_ms: Option<i64>,
    pub pending_generation_id: Option<String>,
    pub pending_epoch_ms: Option<i64>,
    pub build_state: String,
    pub build_error_code: Option<String>,
    pub build_error_message: Option<String>,
    pub build_candidate_count: i64,
    pub build_entry_count: i64,
    pub build_last_attempt_ms: Option<i64>,
    pub build_last_success_ms: Option<i64>,
    pub last_auto_build_ms: Option<i64>,
    pub favourite: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLibraryChannel {
    pub id: String,
    pub owner_user_id: i64,
    pub request_id: String,
    pub request_hash: String,
    pub name: String,
    pub description: String,
    pub visibility: ChannelVisibility,
    pub enabled: bool,
    pub recipe: LibraryChannelRecipe,
    pub seed: [u8; 32],
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChannelUpdate {
    pub subject_next_programme: bool,
    pub channel_id: String,
    pub actor_user_id: i64,
    pub actor_is_admin: bool,
    pub expected_revision: i64,
    pub request_id: String,
    pub request_hash: String,
    pub name: String,
    pub description: String,
    pub visibility: ChannelVisibility,
    pub enabled: bool,
    pub recipe: LibraryChannelRecipe,
    pub seed: [u8; 32],
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChannelDelete {
    pub channel_id: String,
    pub actor_user_id: i64,
    pub actor_is_admin: bool,
    pub expected_revision: i64,
    pub request_id: String,
    pub request_hash: String,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMutation<T> {
    Applied(T),
    Replay(T),
    NotFound,
    Forbidden,
    Stale,
    RequestConflict,
    RequestLedgerFull,
    LimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryChannelGeneration {
    pub id: String,
    pub channel_id: String,
    pub definition_revision: i64,
    pub algorithm_version: i64,
    pub content_digest: String,
    pub state: String,
    pub entry_count: i64,
    pub loop_duration_ms: i64,
    pub build_claim_id: Option<String>,
    pub build_claim_expires_ms: Option<i64>,
    pub created_at_ms: i64,
    pub entries: Vec<ChannelGenerationEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChannelBuildClaim {
    pub channel_id: String,
    pub generation_id: String,
    pub expected_revision: i64,
    pub claim_id: String,
    pub content_digest: String,
    pub now_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChannelPublication {
    pub subject_claim: Option<(String, String)>,
    pub channel_id: String,
    pub generation_id: String,
    pub expected_revision: i64,
    pub claim_id: String,
    pub content_digest: String,
    pub seed: [u8; 32],
    pub entry_count: i64,
    pub loop_duration_ms: i64,
    pub activation_epoch_ms: i64,
    pub pending: bool,
    pub automatic: bool,
    pub now_ms: i64,
}

/// Digest only the facts that define a rotation. Operational recipe fields
/// (for example auto-refresh) do not create a new schedule, while a deliberate
/// reshuffle seed always does even when a one-item rotation happens to retain
/// the same visible order.
pub fn generation_content_digest(seed: [u8; 32], entries: &[ChannelGenerationEntry]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"plurx.library-channel.generation.v2\0");
    digest.update(1_i64.to_be_bytes());
    digest.update(seed);
    digest.update(serde_json::to_vec(entries).expect("bounded generation entries serialize"));
    hex::encode(digest.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChannelBuildFailure {
    pub channel_id: String,
    pub generation_id: Option<String>,
    pub claim_id: Option<String>,
    pub expected_revision: i64,
    pub error_code: String,
    pub error_message: String,
    pub candidate_count: i64,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelBuildMutation {
    Applied,
    NotFound,
    Forbidden,
    Stale,
    Busy,
    Invalid,
}

fn default_true() -> bool {
    true
}

impl Default for LibraryChannelRecipe {
    fn default() -> Self {
        Self {
            version: 1,
            subject: None,
            library_ids: Vec::new(),
            kinds: vec![ChannelItemKind::Movie, ChannelItemKind::Episode],
            genres_any: Vec::new(),
            tags_any: Vec::new(),
            keywords_any: Vec::new(),
            year_min: None,
            year_max: None,
            include_item_ids: Vec::new(),
            include_show_ids: Vec::new(),
            exclude_item_ids: Vec::new(),
            exclude_show_ids: Vec::new(),
            ordering: ChannelOrdering::BalancedShuffle,
            include_specials: false,
            auto_refresh: true,
            match_all_in_scope: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeValidationError {
    pub field: &'static str,
    pub message: String,
}

impl std::fmt::Display for RecipeValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for RecipeValidationError {}

impl LibraryChannelRecipe {
    /// Normalize the persisted representation and enforce every recipe bound.
    pub fn normalize(mut self) -> Result<Self, RecipeValidationError> {
        if self.version != 1 {
            return Err(recipe_error(
                "version",
                "only recipe version 1 is supported",
            ));
        }
        self.subject = self
            .subject
            .map(|s| normalize_subject_text(s.trim()))
            .filter(|s| !s.is_empty());
        if self
            .subject
            .as_ref()
            .is_some_and(|s| s.chars().count() > 500)
        {
            return Err(recipe_error("subject", "use at most 500 characters"));
        }
        normalize_ids("library_ids", &mut self.library_ids, CHANNEL_LIBRARIES_MAX)?;
        normalize_ids(
            "include_item_ids",
            &mut self.include_item_ids,
            CHANNEL_EXPLICIT_IDS_MAX,
        )?;
        normalize_ids(
            "include_show_ids",
            &mut self.include_show_ids,
            CHANNEL_EXPLICIT_IDS_MAX,
        )?;
        normalize_ids(
            "exclude_item_ids",
            &mut self.exclude_item_ids,
            CHANNEL_EXPLICIT_IDS_MAX,
        )?;
        normalize_ids(
            "exclude_show_ids",
            &mut self.exclude_show_ids,
            CHANNEL_EXPLICIT_IDS_MAX,
        )?;
        let explicit_count = self.include_item_ids.len()
            + self.include_show_ids.len()
            + self.exclude_item_ids.len()
            + self.exclude_show_ids.len();
        if explicit_count > CHANNEL_EXPLICIT_IDS_MAX {
            return Err(recipe_error(
                "explicit_ids",
                format!("at most {CHANNEL_EXPLICIT_IDS_MAX} explicit IDs are allowed"),
            ));
        }
        normalize_strings("genres_any", &mut self.genres_any)?;
        normalize_strings("tags_any", &mut self.tags_any)?;
        normalize_strings("keywords_any", &mut self.keywords_any)?;
        self.kinds.sort_by_key(|kind| match kind {
            ChannelItemKind::Movie => 0,
            ChannelItemKind::Episode => 1,
        });
        self.kinds.dedup();
        if self.kinds.is_empty() {
            return Err(recipe_error("kinds", "select at least one supported kind"));
        }
        if let (Some(minimum), Some(maximum)) = (self.year_min, self.year_max) {
            if minimum > maximum {
                return Err(recipe_error("year_min", "must not be after year_max"));
            }
        }
        Ok(self)
    }

    pub fn has_subject_rules(&self) -> bool {
        self.subject.is_some()
            || self.match_all_in_scope
            || !self.genres_any.is_empty()
            || !self.tags_any.is_empty()
            || !self.keywords_any.is_empty()
            || self.year_min.is_some()
            || self.year_max.is_some()
    }
}

fn deserialize_recipe_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(i64),
        Decimal(String),
    }
    Vec::<Id>::deserialize(d)?
        .into_iter()
        .map(|id| match id {
            Id::Number(v) => Ok(v),
            Id::Decimal(v) => v.parse().map_err(serde::de::Error::custom),
        })
        .collect()
}

pub fn normalize_subject_text(value: &str) -> String {
    icu_normalizer::ComposingNormalizer::new_nfc()
        .normalize(value)
        .into_owned()
}

fn recipe_error(field: &'static str, message: impl Into<String>) -> RecipeValidationError {
    RecipeValidationError {
        field,
        message: message.into(),
    }
}

fn normalize_ids(
    field: &'static str,
    values: &mut Vec<i64>,
    maximum: usize,
) -> Result<(), RecipeValidationError> {
    if values.iter().any(|value| *value <= 0) {
        return Err(recipe_error(field, "IDs must be positive"));
    }
    values.sort_unstable();
    values.dedup();
    if values.len() > maximum {
        return Err(recipe_error(
            field,
            format!("at most {maximum} unique values are allowed"),
        ));
    }
    Ok(())
}

fn normalize_strings(
    field: &'static str,
    values: &mut Vec<String>,
) -> Result<(), RecipeValidationError> {
    let mut normalized = BTreeMap::<String, String>::new();
    for raw in values.drain(..) {
        let value = raw.trim();
        let length = value.chars().count();
        if length == 0 || length > CHANNEL_RULE_VALUE_LENGTH_MAX {
            return Err(recipe_error(
                field,
                format!("values must contain 1–{CHANNEL_RULE_VALUE_LENGTH_MAX} characters"),
            ));
        }
        normalized
            .entry(normalized_text(value))
            .or_insert_with(|| value.to_owned());
    }
    if normalized.len() > CHANNEL_RULE_VALUES_MAX {
        return Err(recipe_error(
            field,
            format!("at most {CHANNEL_RULE_VALUES_MAX} unique values are allowed"),
        ));
    }
    *values = normalized.into_values().collect();
    Ok(())
}

fn normalized_text(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCandidate {
    pub item_id: i64,
    pub file_id: i64,
    pub file_size: i64,
    pub file_mtime: i64,
    pub duration_ms: i64,
    pub library_id: i64,
    pub kind: ChannelItemKind,
    pub title: String,
    pub overview: String,
    #[serde(default)]
    pub show_overview: String,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub show_genres: Vec<String>,
    #[serde(default)]
    pub show_tags: Vec<String>,
    pub year: Option<i32>,
    pub show_id: Option<i64>,
    pub show_title: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub special: bool,
    pub explicitly_included: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMatch {
    pub candidate: ChannelCandidate,
    pub reasons: Vec<String>,
}

/// Apply one normalized recipe to a coherent, already-authorized catalogue
/// projection. Explicit inclusions override subject rules; exclusions and
/// eligibility are still final.
pub fn evaluate_recipe(
    recipe: &LibraryChannelRecipe,
    candidates: impl IntoIterator<Item = ChannelCandidate>,
) -> Result<Vec<ChannelMatch>, RecipeValidationError> {
    evaluate_recipe_inner(recipe, candidates, None, false)
}

/// The same eligibility and advanced filters used by publication, before inference.
pub fn subject_candidates(
    recipe: &LibraryChannelRecipe,
    candidates: impl IntoIterator<Item = ChannelCandidate>,
) -> Result<Vec<ChannelMatch>, RecipeValidationError> {
    evaluate_recipe_inner(recipe, candidates, None, true)
}

pub fn evaluate_subject_recipe(
    recipe: &LibraryChannelRecipe,
    candidates: impl IntoIterator<Item = ChannelCandidate>,
    decisions: &BTreeMap<i64, crate::channel_subjects::SubjectDecision>,
) -> Result<Vec<ChannelMatch>, RecipeValidationError> {
    evaluate_recipe_inner(recipe, candidates, Some(decisions), false)
}

fn evaluate_recipe_inner(
    recipe: &LibraryChannelRecipe,
    candidates: impl IntoIterator<Item = ChannelCandidate>,
    decisions: Option<&BTreeMap<i64, crate::channel_subjects::SubjectDecision>>,
    collecting: bool,
) -> Result<Vec<ChannelMatch>, RecipeValidationError> {
    let recipe = recipe.clone().normalize()?;
    let libraries = recipe.library_ids.iter().copied().collect::<BTreeSet<_>>();
    let include_items = recipe
        .include_item_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let include_shows = recipe
        .include_show_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let exclude_items = recipe
        .exclude_item_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let exclude_shows = recipe
        .exclude_show_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let kinds = recipe.kinds.iter().copied().collect::<BTreeSet<_>>();
    let mut matches = BTreeMap::<i64, ChannelMatch>::new();

    for mut candidate in candidates {
        if candidate.item_id <= 0
            || candidate.file_id <= 0
            || candidate.duration_ms <= 0
            || candidate.duration_ms > CHANNEL_FILE_DURATION_MAX_MS
            || candidate.file_size < 0
            || candidate.file_mtime < 0
        {
            continue;
        }
        if !libraries.is_empty() && !libraries.contains(&candidate.library_id) {
            continue;
        }
        if !kinds.contains(&candidate.kind)
            || exclude_items.contains(&candidate.item_id)
            || candidate
                .show_id
                .is_some_and(|show_id| exclude_shows.contains(&show_id))
            || (candidate.special && !recipe.include_specials)
        {
            continue;
        }
        if candidate.kind == ChannelItemKind::Episode
            && (candidate.show_id.is_none()
                || candidate.season_number.is_none()
                || candidate.episode_number.is_none())
        {
            continue;
        }

        let explicit = include_items.contains(&candidate.item_id)
            || candidate
                .show_id
                .is_some_and(|show_id| include_shows.contains(&show_id));
        candidate.explicitly_included = explicit;
        let mut reasons = Vec::new();
        if explicit {
            reasons.push(if include_items.contains(&candidate.item_id) {
                "explicit item".to_owned()
            } else {
                "explicit show".to_owned()
            });
        } else if !recipe.has_subject_rules() {
            continue;
        } else {
            if !recipe.genres_any.is_empty() {
                let Some(value) = first_normalized_match(&recipe.genres_any, &candidate.genres)
                else {
                    continue;
                };
                let inherited = candidate
                    .show_genres
                    .iter()
                    .any(|actual| actual.eq_ignore_ascii_case(value));
                reasons.push(format!(
                    "{}genre: {value}",
                    if inherited { "show." } else { "" }
                ));
            }
            if !recipe.tags_any.is_empty() {
                let Some(value) = first_normalized_match(&recipe.tags_any, &candidate.tags) else {
                    continue;
                };
                let inherited = candidate
                    .show_tags
                    .iter()
                    .any(|actual| actual.eq_ignore_ascii_case(value));
                reasons.push(format!(
                    "{}tag: {value}",
                    if inherited { "show." } else { "" }
                ));
            }
            if !recipe.keywords_any.is_empty() {
                let item_text =
                    normalized_text(&format!("{}\n{}", candidate.title, candidate.overview));
                let show_text = normalized_text(&format!(
                    "{}\n{}",
                    candidate.show_title.as_deref().unwrap_or_default(),
                    candidate.show_overview
                ));
                let Some((value, inherited)) = recipe.keywords_any.iter().find_map(|keyword| {
                    let normalized = normalized_text(keyword);
                    item_text
                        .contains(&normalized)
                        .then_some((keyword, false))
                        .or_else(|| show_text.contains(&normalized).then_some((keyword, true)))
                }) else {
                    continue;
                };
                reasons.push(format!(
                    "{}keyword: {value}",
                    if inherited { "show." } else { "" }
                ));
            }
            if let Some(minimum) = recipe.year_min {
                if !candidate.year.is_some_and(|year| year >= minimum) {
                    continue;
                }
                reasons.push(format!("year >= {minimum}"));
            }
            if let Some(maximum) = recipe.year_max {
                if !candidate.year.is_some_and(|year| year <= maximum) {
                    continue;
                }
                reasons.push(format!("year <= {maximum}"));
            }
            if recipe.match_all_in_scope && reasons.is_empty() {
                reasons.push("all titles in scope".to_owned());
            }
        }
        if recipe.subject.is_some() && !explicit && !collecting {
            let Some(decision) = decisions.and_then(|rows| rows.get(&candidate.item_id)) else {
                continue;
            };
            if decision.verdict != crate::channel_subjects::Verdict::Match {
                continue;
            }
            reasons.push(decision.reason.clone());
        }
        matches
            .entry(candidate.item_id)
            .or_insert(ChannelMatch { candidate, reasons });
        if matches.len() > CHANNEL_ELIGIBLE_POOL_MAX {
            return Err(recipe_error(
                "recipe",
                format!("eligible pool exceeds {CHANNEL_ELIGIBLE_POOL_MAX} titles"),
            ));
        }
    }
    Ok(matches.into_values().collect())
}

fn first_normalized_match<'a>(wanted: &'a [String], actual: &[String]) -> Option<&'a str> {
    let actual = actual
        .iter()
        .map(|value| normalized_text(value))
        .collect::<BTreeSet<_>>();
    wanted
        .iter()
        .find(|value| actual.contains(&normalized_text(value)))
        .map(String::as_str)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelGenerationEntry {
    pub ordinal: u32,
    pub item_id: i64,
    pub file_id: i64,
    pub file_size: i64,
    pub file_mtime: i64,
    pub duration_ms: i64,
    pub cumulative_start_ms: i64,
    pub show_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOccurrence {
    pub cycle: i64,
    pub ordinal: u32,
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
    pub position_ms: i64,
    pub entry: ChannelGenerationEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    Empty,
    BeforeEpoch,
    InvalidEntry,
    Overflow,
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "the generation has no entries",
            Self::BeforeEpoch => "the channel did not exist at this time",
            Self::InvalidEntry => "the generation contains an invalid entry",
            Self::Overflow => "the schedule exceeds signed 64-bit milliseconds",
        })
    }
}

impl std::error::Error for ScheduleError {}

pub fn build_rotation(
    candidates: Vec<ChannelCandidate>,
    ordering: ChannelOrdering,
    seed: [u8; 32],
) -> Result<Vec<ChannelGenerationEntry>, ScheduleError> {
    if candidates.is_empty() {
        return Err(ScheduleError::Empty);
    }
    let mut queues = BTreeMap::<String, Vec<ChannelCandidate>>::new();
    for candidate in candidates {
        if candidate.duration_ms <= 0 || candidate.duration_ms > CHANNEL_FILE_DURATION_MAX_MS {
            return Err(ScheduleError::InvalidEntry);
        }
        let identity = candidate
            .show_id
            .map(|id| format!("show:{id}"))
            .unwrap_or_else(|| format!("movie:{}", candidate.item_id));
        queues.entry(identity).or_default().push(candidate);
    }
    for queue in queues.values_mut() {
        queue.sort_by(|left, right| {
            left.season_number
                .unwrap_or(i32::MAX)
                .cmp(&right.season_number.unwrap_or(i32::MAX))
                .then_with(|| {
                    left.episode_number
                        .unwrap_or(i32::MAX)
                        .cmp(&right.episode_number.unwrap_or(i32::MAX))
                })
                .then_with(|| left.item_id.cmp(&right.item_id))
        });
    }
    let mut queues = queues
        .into_iter()
        .map(|(identity, entries)| (identity, VecDeque::from(entries)))
        .collect::<Vec<_>>();
    if ordering == ChannelOrdering::BalancedShuffle {
        queues.sort_by(|(left, _), (right, _)| {
            queue_digest(&seed, left)
                .cmp(&queue_digest(&seed, right))
                .then_with(|| left.cmp(right))
        });
    }

    let mut ordered = Vec::new();
    while queues.iter().any(|(_, queue)| !queue.is_empty()) {
        match ordering {
            ChannelOrdering::BalancedShuffle => {
                for (_, queue) in &mut queues {
                    if let Some(candidate) = queue.pop_front() {
                        ordered.push(candidate);
                    }
                }
            }
            ChannelOrdering::ReleaseOrder => {
                let selected = queues
                    .iter()
                    .enumerate()
                    .filter_map(|(index, (_, queue))| {
                        queue.front().map(|candidate| (index, candidate))
                    })
                    .min_by(|(_, left), (_, right)| release_cmp(left, right))
                    .map(|(index, _)| index)
                    .ok_or(ScheduleError::Empty)?;
                ordered.push(queues[selected].1.pop_front().ok_or(ScheduleError::Empty)?);
            }
        }
    }

    let mut cumulative = 0_i64;
    ordered
        .into_iter()
        .enumerate()
        .map(|(ordinal, candidate)| {
            let entry = ChannelGenerationEntry {
                ordinal: u32::try_from(ordinal).map_err(|_| ScheduleError::Overflow)?,
                item_id: candidate.item_id,
                file_id: candidate.file_id,
                file_size: candidate.file_size,
                file_mtime: candidate.file_mtime,
                duration_ms: candidate.duration_ms,
                cumulative_start_ms: cumulative,
                show_id: candidate.show_id,
            };
            cumulative = cumulative
                .checked_add(candidate.duration_ms)
                .ok_or(ScheduleError::Overflow)?;
            Ok(entry)
        })
        .collect()
}

fn queue_digest(seed: &[u8; 32], identity: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"plurx.library-channel.balanced-shuffle.v1\0");
    hasher.update(seed);
    hasher.update((identity.len() as u64).to_be_bytes());
    hasher.update(identity.as_bytes());
    hasher.finalize().into()
}

fn release_cmp(left: &ChannelCandidate, right: &ChannelCandidate) -> Ordering {
    match (left.year, right.year) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| left.item_id.cmp(&right.item_id))
}

pub fn generation_loop_duration(entries: &[ChannelGenerationEntry]) -> Result<i64, ScheduleError> {
    let last = entries.last().ok_or(ScheduleError::Empty)?;
    last.cumulative_start_ms
        .checked_add(last.duration_ms)
        .filter(|duration| *duration > 0)
        .ok_or(ScheduleError::Overflow)
}

pub fn resolve_occurrence(
    entries: &[ChannelGenerationEntry],
    epoch_ms: i64,
    server_now_ms: i64,
) -> Result<ResolvedOccurrence, ScheduleError> {
    if server_now_ms < epoch_ms {
        return Err(ScheduleError::BeforeEpoch);
    }
    let loop_duration = generation_loop_duration(entries)?;
    validate_entries(entries, loop_duration)?;
    let elapsed = server_now_ms
        .checked_sub(epoch_ms)
        .ok_or(ScheduleError::Overflow)?;
    let cycle = elapsed.div_euclid(loop_duration);
    let offset = elapsed.rem_euclid(loop_duration);
    let index = entries
        .partition_point(|entry| entry.cumulative_start_ms <= offset)
        .checked_sub(1)
        .ok_or(ScheduleError::InvalidEntry)?;
    let entry = entries[index].clone();
    let cycle_offset = cycle
        .checked_mul(loop_duration)
        .ok_or(ScheduleError::Overflow)?;
    let starts_at_ms = epoch_ms
        .checked_add(cycle_offset)
        .and_then(|value| value.checked_add(entry.cumulative_start_ms))
        .ok_or(ScheduleError::Overflow)?;
    let ends_at_ms = starts_at_ms
        .checked_add(entry.duration_ms)
        .ok_or(ScheduleError::Overflow)?;
    Ok(ResolvedOccurrence {
        cycle,
        ordinal: entry.ordinal,
        starts_at_ms,
        ends_at_ms,
        position_ms: server_now_ms - starts_at_ms,
        entry,
    })
}

fn validate_entries(
    entries: &[ChannelGenerationEntry],
    loop_duration: i64,
) -> Result<(), ScheduleError> {
    let mut expected_start = 0_i64;
    for (index, entry) in entries.iter().enumerate() {
        if usize::try_from(entry.ordinal).ok() != Some(index)
            || entry.item_id <= 0
            || entry.file_id <= 0
            || entry.duration_ms <= 0
            || entry.duration_ms > CHANNEL_FILE_DURATION_MAX_MS
            || entry.cumulative_start_ms != expected_start
        {
            return Err(ScheduleError::InvalidEntry);
        }
        expected_start = expected_start
            .checked_add(entry.duration_ms)
            .ok_or(ScheduleError::Overflow)?;
    }
    if expected_start != loop_duration {
        return Err(ScheduleError::InvalidEntry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(item_id: i64, show_id: Option<i64>, episode: i32) -> ChannelCandidate {
        ChannelCandidate {
            item_id,
            file_id: item_id + 100,
            file_size: 1_000,
            file_mtime: 10,
            duration_ms: item_id * 1_000,
            library_id: 1,
            kind: if show_id.is_some() {
                ChannelItemKind::Episode
            } else {
                ChannelItemKind::Movie
            },
            title: format!("Title {item_id}"),
            overview: String::new(),
            show_overview: String::new(),
            genres: vec!["Documentary".to_owned()],
            tags: Vec::new(),
            show_genres: Vec::new(),
            show_tags: Vec::new(),
            year: Some(2000 + episode),
            show_id,
            show_title: show_id.map(|id| format!("Show {id}")),
            season_number: show_id.map(|_| 1),
            episode_number: show_id.map(|_| episode),
            special: false,
            explicitly_included: false,
        }
    }

    #[test]
    fn exact_boundary_selects_the_successor() {
        let entries = build_rotation(
            vec![candidate(1, None, 0), candidate(2, None, 0)],
            ChannelOrdering::BalancedShuffle,
            [7; 32],
        )
        .expect("rotation");
        let first = resolve_occurrence(&entries, 1_000, 1_000).expect("first");
        let second = resolve_occurrence(&entries, 1_000, first.ends_at_ms).expect("second");
        assert_ne!(first.ordinal, second.ordinal);
        assert_eq!(second.position_ms, 0);
    }

    #[test]
    fn empty_rules_do_not_select_the_server() {
        let matches = evaluate_recipe(&LibraryChannelRecipe::default(), [candidate(1, None, 0)])
            .expect("valid recipe");
        assert!(matches.is_empty());
    }
}
