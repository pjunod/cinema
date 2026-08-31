use crate::error::StoreError;
use crate::segplan::TimelineAnnotationSet;

pub const TIMELINE_ANNOTATIONS_SCHEMA: &str = "CREATE TABLE timeline_annotation_sets (
    file_id           INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    generation_id     TEXT NOT NULL,
    version           INTEGER NOT NULL CHECK (version > 0),
    annotations_json  TEXT NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT;";

pub const TIMELINE_MANUAL_OVERRIDES_SCHEMA: &str = "CREATE TABLE timeline_manual_overrides (
    file_id           INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN ('intro','recap','credits','preview')),
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    start_ticks       INTEGER NOT NULL CHECK (start_ticks >= 0),
    end_ticks         INTEGER NOT NULL CHECK (end_ticks > start_ticks),
    timescale         INTEGER NOT NULL CHECK (timescale > 0),
    start_ms          INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms            INTEGER NOT NULL CHECK (end_ms > start_ms),
    revision          INTEGER NOT NULL CHECK (revision > 0),
    generation_id     TEXT NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (file_id, kind)
) STRICT;";

pub(crate) fn validated(
    set: &TimelineAnnotationSet,
    duration_ms: i64,
) -> Result<TimelineAnnotationSet, StoreError> {
    let normalized = set
        .clone()
        .validate_and_normalize(duration_ms)
        .map_err(|message| {
            StoreError::Database(format!("invalid timeline annotation set: {message}"))
        })?;
    i64::try_from(normalized.source_identity.size).map_err(|_| {
        StoreError::Database("timeline annotation source size exceeds SQLite INTEGER".to_owned())
    })?;
    let encoded = serde_json::to_vec(&normalized.annotations).map_err(|error| {
        StoreError::Database(format!(
            "encode timeline annotations for validation: {error}"
        ))
    })?;
    if encoded.len() > crate::segplan::MAX_TIMELINE_ANNOTATIONS_JSON_BYTES {
        return Err(StoreError::Database(format!(
            "timeline annotation payload exceeds {} bytes",
            crate::segplan::MAX_TIMELINE_ANNOTATIONS_JSON_BYTES
        )));
    }
    Ok(normalized)
}
