use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::TimelineAnnotationStore;
use crate::error::StoreError;
use crate::segplan::{
    AnnotationKind, AnnotationProvenance, SourceIdentity, TimelineAnnotation, TimelineAnnotationSet,
};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS timeline_annotation_sets (
    file_id           INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime      INTEGER NOT NULL,
    argv_fingerprint  TEXT NOT NULL,
    generation_id     TEXT NOT NULL,
    version           INTEGER NOT NULL CHECK (version > 0),
    annotations_json  TEXT NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT";

pub(super) const MANUAL_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS timeline_manual_overrides (
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
) STRICT";

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    validate_sql(SCHEMA)?;
    client
        .execute(SCHEMA, params!())
        .await
        .map_err(database_error)?;
    validate_sql(MANUAL_SCHEMA)?;
    client
        .execute(MANUAL_SCHEMA, params!())
        .await
        .map_err(database_error)?;
    Ok(())
}

struct ManualRow {
    kind: String,
    start_ticks: i64,
    end_ticks: i64,
    timescale: i64,
    start_ms: i64,
    end_ms: i64,
    revision: i64,
    generation_id: String,
}

impl From<&mut Row<'_>> for ManualRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            kind: row.get("kind"),
            start_ticks: row.get("start_ticks"),
            end_ticks: row.get("end_ticks"),
            timescale: row.get("timescale"),
            start_ms: row.get("start_ms"),
            end_ms: row.get("end_ms"),
            revision: row.get("revision"),
            generation_id: row.get("generation_id"),
        }
    }
}

struct RevisionRow(i64);

impl From<&mut Row<'_>> for RevisionRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("revision"))
    }
}

struct SetRow {
    generation_id: String,
    version: i64,
    annotations_json: String,
}

impl From<&mut Row<'_>> for SetRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            generation_id: row.get("generation_id"),
            version: row.get("version"),
            annotations_json: row.get("annotations_json"),
        }
    }
}

#[async_trait]
impl TimelineAnnotationStore for HiqliteAuthStore {
    async fn put_timeline_annotation_set(
        &self,
        file_id: i64,
        duration_ms: i64,
        set: &TimelineAnnotationSet,
    ) -> Result<(), StoreError> {
        let set = crate::store::timeline_annotations::validated(set, duration_ms)?;
        let source_size = i64::try_from(set.source_identity.size)
            .map_err(|_| StoreError::Database("source size exceeds SQLite INTEGER".to_owned()))?;
        let annotations_json = serde_json::to_string(&set.annotations).map_err(|error| {
            StoreError::Database(format!("encode timeline annotations: {error}"))
        })?;
        self.execute(
            "INSERT INTO timeline_annotation_sets
                (file_id, source_size, source_mtime, argv_fingerprint,
                 generation_id, version, annotations_json, updated_at_ms,
                 publication_priority)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'normal')
             ON CONFLICT(file_id) DO UPDATE SET
                source_size = excluded.source_size,
                source_mtime = excluded.source_mtime,
                argv_fingerprint = excluded.argv_fingerprint,
                generation_id = excluded.generation_id,
                version = excluded.version,
                annotations_json = excluded.annotations_json,
                updated_at_ms = excluded.updated_at_ms,
                publication_priority = 'normal'",
            params!(
                file_id,
                source_size,
                set.source_identity.mtime_ms,
                set.source_identity.argv_fingerprint,
                set.generation_id,
                i64::from(set.version),
                annotations_json,
                self.now()?
            ),
        )
        .await?;
        Ok(())
    }

    async fn timeline_annotation_set(
        &self,
        file_id: i64,
        source: &SourceIdentity,
    ) -> Result<Option<TimelineAnnotationSet>, StoreError> {
        let source_size = match i64::try_from(source.size) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let mut rows = self
            .client()
            .query_consistent_map::<SetRow, _>(
                "SELECT generation_id, version, annotations_json
                   FROM timeline_annotation_sets
                  WHERE file_id = $1 AND source_size = $2
                    AND source_mtime = $3 AND argv_fingerprint = $4",
                params!(
                    file_id,
                    source_size,
                    source.mtime_ms,
                    source.argv_fingerprint.clone()
                ),
            )
            .await?;
        let (mut generation_id, version, mut annotations) = match rows.pop() {
            Some(row) => {
                let version = u32::try_from(row.version).map_err(|_| {
                    StoreError::Database("invalid timeline annotation version".to_owned())
                })?;
                let annotations = serde_json::from_str(&row.annotations_json).map_err(|error| {
                    StoreError::Database(format!("decode timeline annotations: {error}"))
                })?;
                (row.generation_id, version, annotations)
            }
            None => (String::new(), 1, Vec::new()),
        };
        let manual = self
            .client()
            .query_consistent_map::<ManualRow, _>(
                "SELECT kind, start_ticks, end_ticks, timescale, start_ms, end_ms,
                        revision, generation_id
                   FROM timeline_manual_overrides
                  WHERE file_id = $1 AND source_size = $2
                    AND source_mtime = $3
                  ORDER BY updated_at_ms, kind",
                params!(file_id, source_size, source.mtime_ms),
            )
            .await?;
        for row in manual {
            let kind = AnnotationKind::parse(&row.kind)
                .ok_or_else(|| StoreError::Database("invalid manual annotation kind".to_owned()))?;
            let timescale = u32::try_from(row.timescale).map_err(|_| {
                StoreError::Database("invalid manual annotation timescale".to_owned())
            })?;
            let revision = u64::try_from(row.revision).map_err(|_| {
                StoreError::Database("invalid manual annotation revision".to_owned())
            })?;
            annotations.retain(|annotation: &TimelineAnnotation| annotation.kind != kind);
            annotations.push(TimelineAnnotation {
                kind,
                start_ticks: row.start_ticks,
                end_ticks: row.end_ticks,
                timescale,
                start_ms: row.start_ms,
                end_ms: row.end_ms,
                provenance: AnnotationProvenance::Manual,
                confidence_millis: 1_000,
                detector_version: "manual-v1".to_owned(),
                manual_override_revision: Some(revision),
            });
            generation_id = row.generation_id;
        }
        if generation_id.is_empty() {
            return Ok(None);
        }
        annotations.sort_by_key(|annotation| (annotation.start_ms, annotation.kind));
        Ok(Some(TimelineAnnotationSet {
            source_identity: source.clone(),
            generation_id,
            version,
            annotations,
        }))
    }

    async fn put_timeline_annotation_set_if_missing(
        &self,
        file_id: i64,
        duration_ms: i64,
        set: &TimelineAnnotationSet,
    ) -> Result<bool, StoreError> {
        let set = super::timeline_annotations::validated(set, duration_ms)?;
        let source_size = i64::try_from(set.source_identity.size)
            .map_err(|_| StoreError::Database("source size exceeds SQLite INTEGER".to_owned()))?;
        let annotations_json = serde_json::to_string(&set.annotations).map_err(|error| {
            StoreError::Database(format!("encode timeline annotations: {error}"))
        })?;
        Ok(self
            .execute(
                "INSERT INTO timeline_annotation_sets
                    (file_id, source_size, source_mtime, argv_fingerprint,
                     generation_id, version, annotations_json, updated_at_ms,
                     publication_priority)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, 'normal'
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $1 AND size = $2 AND mtime = $3)
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests
                       WHERE file_id = $1 AND source_size = $2 AND source_mtime = $3
                         AND component = 'skip_markers'
                         AND state IN ('queued', 'running', 'submitted')
                    )
                 ON CONFLICT(file_id) DO UPDATE SET
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    argv_fingerprint = excluded.argv_fingerprint,
                    generation_id = excluded.generation_id,
                    version = excluded.version,
                    annotations_json = excluded.annotations_json,
                    updated_at_ms = excluded.updated_at_ms,
                    publication_priority = 'normal'
                  WHERE timeline_annotation_sets.source_size <> excluded.source_size
                     OR timeline_annotation_sets.source_mtime <> excluded.source_mtime
                     OR timeline_annotation_sets.argv_fingerprint <> excluded.argv_fingerprint",
                params!(
                    file_id,
                    source_size,
                    set.source_identity.mtime_ms,
                    set.source_identity.argv_fingerprint,
                    set.generation_id,
                    i64::from(set.version),
                    annotations_json,
                    self.now()?
                ),
            )
            .await?
            == 1)
    }

    async fn forget_timeline_annotation_set(&self, file_id: i64) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "DELETE FROM timeline_annotation_sets WHERE file_id = $1",
                params!(file_id),
            )
            .await?
            > 0)
    }

    async fn set_manual_timeline_annotation(
        &self,
        file_id: i64,
        duration_ms: i64,
        source: &SourceIdentity,
        annotation: &TimelineAnnotation,
        generation_id: &str,
    ) -> Result<u64, StoreError> {
        if annotation.provenance != AnnotationProvenance::Manual {
            return Err(StoreError::Database(
                "manual override write requires manual provenance".to_owned(),
            ));
        }
        let checked = crate::store::timeline_annotations::validated(
            &TimelineAnnotationSet {
                source_identity: source.clone(),
                generation_id: generation_id.to_owned(),
                version: 1,
                annotations: vec![annotation.clone()],
            },
            duration_ms,
        )?;
        let annotation = checked.annotations.into_iter().next().ok_or_else(|| {
            StoreError::Database("manual override normalization removed the annotation".to_owned())
        })?;
        let source_size = i64::try_from(source.size)
            .map_err(|_| StoreError::Database("source size exceeds SQLite INTEGER".to_owned()))?;
        let sql = "INSERT INTO timeline_manual_overrides
                    (file_id, kind, source_size, source_mtime, argv_fingerprint,
                     start_ticks, end_ticks, timescale, start_ms, end_ms,
                     revision, generation_id, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $11, $12
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $1 AND size = $3 AND mtime = $4)
                 ON CONFLICT(file_id, kind) DO UPDATE SET
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    argv_fingerprint = excluded.argv_fingerprint,
                    start_ticks = excluded.start_ticks,
                    end_ticks = excluded.end_ticks,
                    timescale = excluded.timescale,
                    start_ms = excluded.start_ms,
                    end_ms = excluded.end_ms,
                    revision = timeline_manual_overrides.revision + 1,
                    generation_id = excluded.generation_id,
                    updated_at_ms = excluded.updated_at_ms
                 RETURNING revision";
        validate_sql(sql)?;
        let row = self
            .client()
            .execute_returning_map::<_, RevisionRow>(
                sql,
                params!(
                    file_id,
                    annotation.kind.as_str(),
                    source_size,
                    source.mtime_ms,
                    source.argv_fingerprint.clone(),
                    annotation.start_ticks,
                    annotation.end_ticks,
                    i64::from(annotation.timescale),
                    annotation.start_ms,
                    annotation.end_ms,
                    generation_id,
                    self.now()?
                ),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?
            .into_iter()
            .next()
            .ok_or_else(|| StoreError::Task("timeline annotation source changed".to_owned()))?;
        u64::try_from(row.0)
            .map_err(|_| StoreError::Database("invalid manual annotation revision".to_owned()))
    }

    async fn discard_manual_timeline_annotation(
        &self,
        file_id: i64,
        source: &SourceIdentity,
        kind: AnnotationKind,
        expected_revision: u64,
    ) -> Result<bool, StoreError> {
        let source_size = match i64::try_from(source.size) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        Ok(self
            .execute(
                "DELETE FROM timeline_manual_overrides
                  WHERE file_id = $1 AND kind = $2 AND source_size = $3
                    AND source_mtime = $4 AND revision = $5",
                params!(
                    file_id,
                    kind.as_str(),
                    source_size,
                    source.mtime_ms,
                    i64::try_from(expected_revision).unwrap_or(i64::MAX)
                ),
            )
            .await?
            > 0)
    }
}
