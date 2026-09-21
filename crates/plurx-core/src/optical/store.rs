use serde::{Deserialize, Serialize};

use super::{OpticalFormat, OpticalTitleLocator, MAX_OPTICAL_ID_BYTES};
use crate::playback::{PlaybackMediaFacts, SourceDelivery};

pub const OPTICAL_SCHEMA: &str = r#"
CREATE TABLE optical_discs (
    disc_id TEXT PRIMARY KEY,
    fingerprint_version INTEGER NOT NULL CHECK (fingerprint_version > 0),
    fingerprint_evidence_json TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('dvd', 'bluray')),
    volume_label TEXT,
    display_title TEXT,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
);
CREATE TABLE optical_titles (
    disc_id TEXT NOT NULL,
    title_id TEXT NOT NULL,
    locator_json TEXT NOT NULL,
    angles INTEGER NOT NULL CHECK (angles >= 1),
    facts_json TEXT NOT NULL,
    chapters_json TEXT NOT NULL,
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    matched_item_id INTEGER REFERENCES items(id) ON DELETE SET NULL,
    match_kind TEXT CHECK (match_kind IN ('movie', 'episode', 'extra')),
    CHECK ((matched_item_id IS NULL) = (match_kind IS NULL)),
    PRIMARY KEY (disc_id, title_id),
    FOREIGN KEY (disc_id) REFERENCES optical_discs(disc_id) ON DELETE CASCADE
);
CREATE TABLE optical_progress (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    disc_id TEXT NOT NULL,
    title_id TEXT NOT NULL,
    angle INTEGER NOT NULL CHECK (angle >= 1),
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    watched INTEGER NOT NULL DEFAULT 0 CHECK (watched IN (0, 1)),
    audio_selection_json TEXT,
    subtitle_selection_json TEXT,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (user_id, disc_id, title_id, angle),
    FOREIGN KEY (disc_id, title_id)
        REFERENCES optical_titles(disc_id, title_id) ON DELETE CASCADE
);
CREATE TABLE user_grants (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    grant_name TEXT NOT NULL,
    granted INTEGER NOT NULL CHECK (granted IN (0, 1)),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (user_id, grant_name)
);
CREATE INDEX optical_titles_matched_item ON optical_titles(matched_item_id)
    WHERE matched_item_id IS NOT NULL;
CREATE INDEX optical_progress_user_updated
    ON optical_progress(user_id, updated_at_ms DESC);
"#;

pub const OPTICAL_PLAY_GRANT: &str = "optical.play";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalDisc {
    pub disc_id: String,
    pub fingerprint_version: u32,
    pub fingerprint_evidence_json: String,
    pub format: OpticalFormat,
    pub volume_label: Option<String>,
    pub display_title: Option<String>,
    pub metadata_json: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpticalMatchKind {
    Movie,
    Episode,
    Extra,
}

impl OpticalMatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Episode => "episode",
            Self::Extra => "extra",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "movie" => Some(Self::Movie),
            "episode" => Some(Self::Episode),
            "extra" => Some(Self::Extra),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpticalTitle {
    pub disc_id: String,
    pub title_id: String,
    pub locator: OpticalTitleLocator,
    pub angles: u32,
    pub facts: PlaybackMediaFacts,
    pub chapters_json: String,
    pub duration_ms: Option<i64>,
    pub matched_item_id: Option<i64>,
    pub match_kind: Option<OpticalMatchKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpticalInspection {
    pub disc: OpticalDisc,
    pub titles: Vec<OpticalTitle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalProgress {
    pub user_id: i64,
    pub disc_id: String,
    pub title_id: String,
    pub angle: u32,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub watched: bool,
    pub audio_selection_json: Option<String>,
    pub subtitle_selection_json: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpticalProgressWrite {
    pub user_id: i64,
    pub disc_id: String,
    pub title_id: String,
    pub angle: u32,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub audio_selection_json: Option<String>,
    pub subtitle_selection_json: Option<String>,
    pub recorded_at_ms: Option<i64>,
}

pub(crate) fn validate_document(name: &str, value: &str) -> Result<(), String> {
    const MAX_JSON_BYTES: usize = 1024 * 1024;
    if value.len() > MAX_JSON_BYTES {
        return Err(format!("{name} exceeds {MAX_JSON_BYTES} bytes"));
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|error| format!("invalid {name}: {error}"))
}

pub(crate) fn validate_optical_id(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_OPTICAL_ID_BYTES {
        return Err(format!(
            "{name} must contain 1..={MAX_OPTICAL_ID_BYTES} bytes"
        ));
    }
    Ok(())
}

pub(crate) fn validate_inspection_write(value: &OpticalInspection) -> Result<(), String> {
    validate_optical_id("disc id", &value.disc.disc_id)?;
    if value.disc.fingerprint_version == 0 {
        return Err("fingerprint version must be positive".into());
    }
    if value.disc.created_at_ms < 0 || value.disc.updated_at_ms < value.disc.created_at_ms {
        return Err("disc timestamps are invalid".into());
    }
    validate_document(
        "fingerprint evidence",
        &value.disc.fingerprint_evidence_json,
    )?;
    validate_document("disc metadata", &value.disc.metadata_json)?;
    if value.titles.is_empty() {
        return Err("inspection must contain at least one title".into());
    }
    for title in &value.titles {
        if title.disc_id != value.disc.disc_id {
            return Err("title disc id differs from inspection disc id".into());
        }
        validate_optical_id("title id", &title.title_id)?;
        if title.angles == 0 {
            return Err("optical title must report at least one angle".into());
        }
        if title.duration_ms.is_some_and(|duration| duration < 0) {
            return Err("title duration must be nonnegative".into());
        }
        if title.facts.source_delivery != SourceDelivery::ManagedOpticalTitle {
            return Err("optical title facts must require managed optical delivery".into());
        }
        if title.facts.learned_limit_identity.is_some() {
            return Err("optical title facts cannot carry a file decoder identity".into());
        }
        let facts_json = serde_json::to_string(&title.facts)
            .map_err(|error| format!("invalid title facts: {error}"))?;
        validate_document("title facts", &facts_json)?;
        validate_document("title chapters", &title.chapters_json)?;
        let locator = serde_json::to_value(title.locator)
            .map_err(|error| format!("invalid title locator: {error}"))?;
        if locator.is_null() {
            return Err("title locator is missing".into());
        }
        if title.matched_item_id.is_some() != title.match_kind.is_some() {
            return Err("matched item and match kind must be present together".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_progress_write(value: &OpticalProgressWrite) -> Result<(), String> {
    if value.user_id <= 0 {
        return Err("user id must be positive".into());
    }
    validate_optical_id("disc id", &value.disc_id)?;
    validate_optical_id("title id", &value.title_id)?;
    if value.angle == 0 || value.position_ms < 0 {
        return Err("angle and position are invalid".into());
    }
    if value.duration_ms.is_some_and(|duration| duration < 0) {
        return Err("duration must be nonnegative".into());
    }
    if value.recorded_at_ms.is_some_and(|at| at < 0) {
        return Err("recorded timestamp must be nonnegative".into());
    }
    for (name, document) in [
        ("audio selection", value.audio_selection_json.as_deref()),
        (
            "subtitle selection",
            value.subtitle_selection_json.as_deref(),
        ),
    ] {
        if let Some(document) = document {
            validate_document(name, document)?;
        }
    }
    Ok(())
}
