use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::error::StoreError;
use crate::segplan::{
    AnnotationKind, AnnotationProvenance, SourceIdentity, TimelineAnnotation, TimelineAnnotationSet,
};
use crate::store::TimelineAnnotationStore;

#[async_trait]
impl TimelineAnnotationStore for SqliteStore {
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
        let now_ms = unix_ms()?;
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO timeline_annotation_sets
                    (file_id, source_size, source_mtime, argv_fingerprint,
                     generation_id, version, annotations_json, updated_at_ms,
                     publication_priority)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'normal')
                 ON CONFLICT(file_id) DO UPDATE SET
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    argv_fingerprint = excluded.argv_fingerprint,
                    generation_id = excluded.generation_id,
                    version = excluded.version,
                    annotations_json = excluded.annotations_json,
                    updated_at_ms = excluded.updated_at_ms,
                    publication_priority = 'normal'",
                params![
                    file_id,
                    source_size,
                    set.source_identity.mtime_ms,
                    set.source_identity.argv_fingerprint,
                    set.generation_id,
                    i64::from(set.version),
                    annotations_json,
                    now_ms,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn timeline_annotation_set(
        &self,
        file_id: i64,
        source: &SourceIdentity,
    ) -> Result<Option<TimelineAnnotationSet>, StoreError> {
        let source = source.clone();
        self.with_read(move |conn| {
            let base = conn
                .query_row(
                    "SELECT generation_id, version, annotations_json
                       FROM timeline_annotation_sets
                      WHERE file_id = ?1 AND source_size = ?2
                        AND source_mtime = ?3 AND argv_fingerprint = ?4",
                    params![
                        file_id,
                        i64::try_from(source.size).unwrap_or(i64::MAX),
                        source.mtime_ms,
                        source.argv_fingerprint,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let (mut generation_id, version, mut annotations) = match base {
                Some((generation_id, version, annotations_json)) => {
                    let annotations = serde_json::from_str(&annotations_json).map_err(|error| {
                        StoreError::Database(format!("decode timeline annotations: {error}"))
                    })?;
                    let version = u32::try_from(version).map_err(|_| {
                        StoreError::Database("invalid timeline annotation version".to_owned())
                    })?;
                    (generation_id, version, annotations)
                }
                None => (String::new(), 1, Vec::new()),
            };

            let mut statement = conn.prepare(
                "SELECT kind, start_ticks, end_ticks, timescale, start_ms, end_ms,
                        revision, generation_id
                   FROM timeline_manual_overrides
                  WHERE file_id = ?1 AND source_size = ?2
                    AND source_mtime = ?3
                  ORDER BY updated_at_ms, kind",
            )?;
            let manual = statement.query_map(
                params![
                    file_id,
                    i64::try_from(source.size).unwrap_or(i64::MAX),
                    source.mtime_ms,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )?;
            for row in manual {
                let (
                    kind,
                    start_ticks,
                    end_ticks,
                    timescale,
                    start_ms,
                    end_ms,
                    revision,
                    manual_generation,
                ) = row?;
                let kind = AnnotationKind::parse(&kind).ok_or_else(|| {
                    StoreError::Database("invalid manual annotation kind".to_owned())
                })?;
                let timescale = u32::try_from(timescale).map_err(|_| {
                    StoreError::Database("invalid manual annotation timescale".to_owned())
                })?;
                let revision = u64::try_from(revision).map_err(|_| {
                    StoreError::Database("invalid manual annotation revision".to_owned())
                })?;
                annotations.retain(|annotation: &TimelineAnnotation| annotation.kind != kind);
                annotations.push(TimelineAnnotation {
                    kind,
                    start_ticks,
                    end_ticks,
                    timescale,
                    start_ms,
                    end_ms,
                    provenance: AnnotationProvenance::Manual,
                    confidence_millis: 1_000,
                    detector_version: "manual-v1".to_owned(),
                    manual_override_revision: Some(revision),
                });
                generation_id = manual_generation;
            }
            if generation_id.is_empty() {
                return Ok(None);
            }
            annotations.sort_by_key(|annotation| (annotation.start_ms, annotation.kind));
            Ok(Some(TimelineAnnotationSet {
                source_identity: source,
                generation_id,
                version,
                annotations,
            }))
        })
        .await
    }

    async fn put_timeline_annotation_set_if_missing(
        &self,
        file_id: i64,
        duration_ms: i64,
        set: &TimelineAnnotationSet,
    ) -> Result<bool, StoreError> {
        let set = crate::store::timeline_annotations::validated(set, duration_ms)?;
        let source_size = i64::try_from(set.source_identity.size)
            .map_err(|_| StoreError::Database("source size exceeds SQLite INTEGER".to_owned()))?;
        let annotations_json = serde_json::to_string(&set.annotations).map_err(|error| {
            StoreError::Database(format!("encode timeline annotations: {error}"))
        })?;
        let now_ms = unix_ms()?;
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "INSERT INTO timeline_annotation_sets
                    (file_id, source_size, source_mtime, argv_fingerprint,
                     generation_id, version, annotations_json, updated_at_ms,
                     publication_priority)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'normal'
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?1 AND size = ?2 AND mtime = ?3)
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests
                       WHERE file_id = ?1 AND source_size = ?2 AND source_mtime = ?3
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
                params![
                    file_id,
                    source_size,
                    set.source_identity.mtime_ms,
                    set.source_identity.argv_fingerprint,
                    set.generation_id,
                    i64::from(set.version),
                    annotations_json,
                    now_ms,
                ],
            )? == 1)
        })
        .await
    }

    async fn forget_timeline_annotation_set(&self, file_id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM timeline_annotation_sets WHERE file_id = ?1",
                params![file_id],
            )? > 0)
        })
        .await
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
        let source = source.clone();
        let generation_id = generation_id.to_owned();
        let now_ms = unix_ms()?;
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "INSERT INTO timeline_manual_overrides
                    (file_id, kind, source_size, source_mtime, argv_fingerprint,
                     start_ticks, end_ticks, timescale, start_ms, end_ms,
                     revision, generation_id, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?1 AND size = ?3 AND mtime = ?4)
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
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    file_id,
                    annotation.kind.as_str(),
                    i64::try_from(source.size).unwrap_or(i64::MAX),
                    source.mtime_ms,
                    source.argv_fingerprint,
                    annotation.start_ticks,
                    annotation.end_ticks,
                    i64::from(annotation.timescale),
                    annotation.start_ms,
                    annotation.end_ms,
                    generation_id,
                    now_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Task(
                    "timeline annotation source changed".to_owned(),
                ));
            }
            let revision: i64 = conn.query_row(
                "SELECT revision FROM timeline_manual_overrides
                  WHERE file_id = ?1 AND kind = ?2",
                params![file_id, annotation.kind.as_str()],
                |row| row.get(0),
            )?;
            u64::try_from(revision)
                .map_err(|_| StoreError::Database("invalid manual annotation revision".to_owned()))
        })
        .await
    }

    async fn discard_manual_timeline_annotation(
        &self,
        file_id: i64,
        source: &SourceIdentity,
        kind: AnnotationKind,
        expected_revision: u64,
    ) -> Result<bool, StoreError> {
        let source = source.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM timeline_manual_overrides
                  WHERE file_id = ?1 AND kind = ?2 AND source_size = ?3
                    AND source_mtime = ?4 AND revision = ?5",
                params![
                    file_id,
                    kind.as_str(),
                    i64::try_from(source.size).unwrap_or(i64::MAX),
                    source.mtime_ms,
                    i64::try_from(expected_revision).unwrap_or(i64::MAX),
                ],
            )? > 0)
        })
        .await
    }
}

fn unix_ms() -> Result<i64, StoreError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| StoreError::Task(format!("system clock precedes unix epoch: {error}")))?
        .as_millis()
        .min(i64::MAX as u128) as i64)
}
