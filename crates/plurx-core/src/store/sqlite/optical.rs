use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::error::StoreError;
use crate::optical::{
    OpticalDisc, OpticalFormat, OpticalInspection, OpticalMatchKind, OpticalProgress,
    OpticalProgressWrite, OpticalTitle, OpticalTitleLocator,
};
use crate::store::OpticalStore;

fn invalid_column(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.into(),
        )),
    )
}

fn disc_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpticalDisc> {
    let format: String = row.get(3)?;
    Ok(OpticalDisc {
        disc_id: row.get(0)?,
        fingerprint_version: row.get(1)?,
        fingerprint_evidence_json: row.get(2)?,
        format: OpticalFormat::parse(&format)
            .ok_or_else(|| invalid_column(format!("invalid optical format {format}")))?,
        volume_label: row.get(4)?,
        display_title: row.get(5)?,
        metadata_json: row.get(6)?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

fn title_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpticalTitle> {
    let locator_json: String = row.get(2)?;
    let locator: OpticalTitleLocator = serde_json::from_str(&locator_json)
        .map_err(|error| invalid_column(format!("invalid optical locator: {error}")))?;
    let match_kind: Option<String> = row.get(8)?;
    let facts_json: String = row.get(4)?;
    Ok(OpticalTitle {
        disc_id: row.get(0)?,
        title_id: row.get(1)?,
        locator,
        angles: row.get(3)?,
        facts: serde_json::from_str(&facts_json)
            .map_err(|error| invalid_column(format!("invalid optical media facts: {error}")))?,
        chapters_json: row.get(5)?,
        duration_ms: row.get(6)?,
        matched_item_id: row.get(7)?,
        match_kind: match_kind
            .map(|kind| {
                OpticalMatchKind::parse(&kind)
                    .ok_or_else(|| invalid_column(format!("invalid optical match kind {kind}")))
            })
            .transpose()?,
    })
}

fn progress_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpticalProgress> {
    Ok(OpticalProgress {
        user_id: row.get(0)?,
        disc_id: row.get(1)?,
        title_id: row.get(2)?,
        angle: row.get(3)?,
        position_ms: row.get(4)?,
        duration_ms: row.get(5)?,
        watched: row.get::<_, i64>(6)? != 0,
        audio_selection_json: row.get(7)?,
        subtitle_selection_json: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

const DISC_COLUMNS: &str = "disc_id, fingerprint_version, fingerprint_evidence_json, format, \
    volume_label, display_title, metadata_json, created_at_ms, updated_at_ms";
const TITLE_COLUMNS: &str = "disc_id, title_id, locator_json, angles, facts_json, chapters_json, \
    duration_ms, matched_item_id, match_kind";
const PROGRESS_COLUMNS: &str = "user_id, disc_id, title_id, angle, position_ms, duration_ms, \
    watched, audio_selection_json, subtitle_selection_json, updated_at_ms";

#[async_trait]
impl OpticalStore for SqliteStore {
    async fn upsert_optical_inspection(
        &self,
        inspection: &OpticalInspection,
    ) -> Result<(), StoreError> {
        crate::optical::store::validate_inspection_write(inspection)
            .map_err(StoreError::Database)?;
        let inspection = inspection.clone();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO optical_discs
                     (disc_id, fingerprint_version, fingerprint_evidence_json, format,
                      volume_label, display_title, metadata_json, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(disc_id) DO UPDATE SET
                     fingerprint_version = excluded.fingerprint_version,
                     fingerprint_evidence_json = excluded.fingerprint_evidence_json,
                     format = excluded.format,
                     volume_label = excluded.volume_label,
                     display_title = COALESCE(optical_discs.display_title, excluded.display_title),
                     metadata_json = CASE WHEN optical_discs.metadata_json = '{}'
                                          THEN excluded.metadata_json
                                          ELSE optical_discs.metadata_json END,
                     updated_at_ms = MAX(optical_discs.updated_at_ms, excluded.updated_at_ms)",
                params![
                    inspection.disc.disc_id,
                    inspection.disc.fingerprint_version,
                    inspection.disc.fingerprint_evidence_json,
                    inspection.disc.format.as_str(),
                    inspection.disc.volume_label,
                    inspection.disc.display_title,
                    inspection.disc.metadata_json,
                    inspection.disc.created_at_ms,
                    inspection.disc.updated_at_ms,
                ],
            )?;
            for title in inspection.titles {
                let locator_json = serde_json::to_string(&title.locator)
                    .map_err(|error| StoreError::Database(error.to_string()))?;
                let facts_json = serde_json::to_string(&title.facts)
                    .map_err(|error| StoreError::Database(error.to_string()))?;
                transaction.execute(
                    "INSERT INTO optical_titles
                         (disc_id, title_id, locator_json, angles, facts_json, chapters_json,
                          duration_ms, matched_item_id, match_kind)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(disc_id, title_id) DO UPDATE SET
                         locator_json = excluded.locator_json,
                         angles = excluded.angles,
                         facts_json = excluded.facts_json,
                         chapters_json = excluded.chapters_json,
                         duration_ms = excluded.duration_ms",
                    params![
                        title.disc_id,
                        title.title_id,
                        locator_json,
                        title.angles,
                        facts_json,
                        title.chapters_json,
                        title.duration_ms,
                        title.matched_item_id,
                        title.match_kind.map(OpticalMatchKind::as_str),
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn optical_disc(&self, disc_id: &str) -> Result<Option<OpticalDisc>, StoreError> {
        crate::optical::store::validate_optical_id("disc id", disc_id)
            .map_err(StoreError::Database)?;
        let disc_id = disc_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {DISC_COLUMNS} FROM optical_discs WHERE disc_id = ?1"),
                    params![disc_id],
                    disc_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn optical_titles(&self, disc_id: &str) -> Result<Vec<OpticalTitle>, StoreError> {
        crate::optical::store::validate_optical_id("disc id", disc_id)
            .map_err(StoreError::Database)?;
        let disc_id = disc_id.to_owned();
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {TITLE_COLUMNS} FROM optical_titles \
                 WHERE disc_id = ?1 ORDER BY title_id"
            ))?;
            let titles = statement
                .query_map(params![disc_id], title_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(titles)
        })
        .await
    }

    async fn optical_title(
        &self,
        disc_id: &str,
        title_id: &str,
    ) -> Result<Option<OpticalTitle>, StoreError> {
        crate::optical::store::validate_optical_id("disc id", disc_id)
            .and_then(|_| crate::optical::store::validate_optical_id("title id", title_id))
            .map_err(StoreError::Database)?;
        let disc_id = disc_id.to_owned();
        let title_id = title_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {TITLE_COLUMNS} FROM optical_titles \
                         WHERE disc_id = ?1 AND title_id = ?2"
                    ),
                    params![disc_id, title_id],
                    title_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn set_optical_match(
        &self,
        disc_id: &str,
        title_id: &str,
        matched_item_id: Option<i64>,
        match_kind: Option<OpticalMatchKind>,
    ) -> Result<bool, StoreError> {
        if matched_item_id.is_some() != match_kind.is_some()
            || matched_item_id.is_some_and(|item_id| item_id <= 0)
        {
            return Err(StoreError::Database(
                "optical match item and kind must be valid and present together".into(),
            ));
        }
        let disc_id = disc_id.to_owned();
        let title_id = title_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE optical_titles SET matched_item_id = ?3, match_kind = ?4 \
                 WHERE disc_id = ?1 AND title_id = ?2",
                params![
                    disc_id,
                    title_id,
                    matched_item_id,
                    match_kind.map(OpticalMatchKind::as_str)
                ],
            )? == 1)
        })
        .await
    }

    async fn optical_progress(
        &self,
        user_id: i64,
        disc_id: &str,
        title_id: &str,
        angle: u32,
    ) -> Result<Option<OpticalProgress>, StoreError> {
        let disc_id = disc_id.to_owned();
        let title_id = title_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {PROGRESS_COLUMNS} FROM optical_progress \
                         WHERE user_id = ?1 AND disc_id = ?2 AND title_id = ?3 AND angle = ?4"
                    ),
                    params![user_id, disc_id, title_id, angle],
                    progress_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn put_optical_progress(
        &self,
        progress: &OpticalProgressWrite,
    ) -> Result<OpticalProgress, StoreError> {
        crate::optical::store::validate_progress_write(progress).map_err(StoreError::Database)?;
        let progress = progress.clone();
        self.with_conn(move |conn| {
            let known_duration: Option<i64> = conn
                .query_row(
                    "SELECT duration_ms FROM optical_titles \
                     WHERE disc_id = ?1 AND title_id = ?2",
                    params![progress.disc_id, progress.title_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let duration = known_duration.or(progress.duration_ms);
            if duration.is_some_and(|duration| progress.position_ms > duration) {
                return Err(StoreError::Database(
                    "optical progress position exceeds the known title duration".into(),
                ));
            }
            let watched = duration.is_some_and(|duration| {
                duration > 0 && progress.position_ms as f64 / duration as f64 >= 0.95
            });
            let now_ms = conn.query_row("SELECT unixepoch() * 1000", [], |row| row.get(0))?;
            let at = progress.recorded_at_ms.unwrap_or(now_ms).clamp(0, now_ms);
            let returned = conn
                .query_row(
                    "INSERT INTO optical_progress
                         (user_id, disc_id, title_id, angle, position_ms, duration_ms, watched,
                          audio_selection_json, subtitle_selection_json, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(user_id, disc_id, title_id, angle) DO UPDATE SET
                         position_ms = excluded.position_ms,
                         duration_ms = COALESCE(excluded.duration_ms, optical_progress.duration_ms),
                         watched = optical_progress.watched OR excluded.watched,
                         audio_selection_json = COALESCE(excluded.audio_selection_json,
                                                         optical_progress.audio_selection_json),
                         subtitle_selection_json = COALESCE(excluded.subtitle_selection_json,
                                                            optical_progress.subtitle_selection_json),
                         updated_at_ms = excluded.updated_at_ms
                     WHERE ?11 = 1 OR excluded.updated_at_ms >= optical_progress.updated_at_ms
                     RETURNING user_id, disc_id, title_id, angle, position_ms, duration_ms,
                               watched, audio_selection_json, subtitle_selection_json, updated_at_ms",
                    params![
                        progress.user_id,
                        progress.disc_id,
                        progress.title_id,
                        progress.angle,
                        progress.position_ms,
                        duration,
                        watched as i64,
                        progress.audio_selection_json,
                        progress.subtitle_selection_json,
                        at,
                        progress.recorded_at_ms.is_none() as i64,
                    ],
                    progress_from_row,
                )
                .optional()?;
            if let Some(returned) = returned {
                return Ok(returned);
            }
            conn.query_row(
                &format!(
                    "SELECT {PROGRESS_COLUMNS} FROM optical_progress \
                     WHERE user_id = ?1 AND disc_id = ?2 AND title_id = ?3 AND angle = ?4"
                ),
                params![
                    progress.user_id,
                    progress.disc_id,
                    progress.title_id,
                    progress.angle
                ],
                progress_from_row,
            )
            .map_err(Into::into)
        })
        .await
    }

    async fn delete_user_optical_history(&self, user_id: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM optical_progress WHERE user_id = ?1",
                params![user_id],
            )? as u64)
        })
        .await
    }

    async fn clear_optical_matches_for_item(&self, item_id: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE optical_titles SET matched_item_id = NULL, match_kind = NULL \
                 WHERE matched_item_id = ?1",
                params![item_id],
            )? as u64)
        })
        .await
    }

    async fn optical_play_grant(&self, user_id: i64) -> Result<Option<bool>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT COALESCE(
                         (SELECT granted FROM user_grants
                           WHERE user_id = users.id AND grant_name = ?2),
                         users.is_admin)
                       FROM users WHERE id = ?1",
                    params![user_id, crate::optical::OPTICAL_PLAY_GRANT],
                    |row| Ok(row.get::<_, i64>(0)? != 0),
                )
                .optional()?)
        })
        .await
    }

    async fn set_optical_play_grant(
        &self,
        user_id: i64,
        granted: bool,
    ) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "INSERT INTO user_grants (user_id, grant_name, granted, updated_at_ms)
                 SELECT id, ?2, ?3, unixepoch() * 1000 FROM users WHERE id = ?1
                 ON CONFLICT(user_id, grant_name) DO UPDATE SET
                     granted = excluded.granted,
                     updated_at_ms = excluded.updated_at_ms",
                params![user_id, crate::optical::OPTICAL_PLAY_GRANT, granted as i64],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{OpticalDisc, OpticalInspection, OpticalTitle};
    use crate::store::UserStore;

    fn inspection(disc_id: &str, label: &str, title_id: &str) -> OpticalInspection {
        OpticalInspection {
            disc: OpticalDisc {
                disc_id: disc_id.to_owned(),
                fingerprint_version: 1,
                fingerprint_evidence_json: r#"{"volume_bytes":1024}"#.to_owned(),
                format: OpticalFormat::Dvd,
                volume_label: Some(label.to_owned()),
                display_title: None,
                metadata_json: "{}".to_owned(),
                created_at_ms: 10,
                updated_at_ms: 10,
            },
            titles: vec![OpticalTitle {
                disc_id: disc_id.to_owned(),
                title_id: title_id.to_owned(),
                locator: OpticalTitleLocator::Dvd { title_number: 1 },
                angles: 1,
                facts: crate::playback::PlaybackMediaFacts {
                    container: Some("mpeg".to_owned()),
                    video_codec: Some("mpeg2video".to_owned()),
                    source_delivery: crate::playback::SourceDelivery::ManagedOpticalTitle,
                    probed: true,
                    ..crate::playback::PlaybackMediaFacts::default()
                },
                chapters_json: "[]".to_owned(),
                duration_ms: Some(1_000),
                matched_item_id: None,
                match_kind: None,
            }],
        }
    }

    #[tokio::test]
    async fn optical_store_keeps_same_label_discs_distinct_and_upserts_atomically() {
        let store = SqliteStore::open_in_memory().expect("store");
        let first = inspection("dvd-v1-first", "SAME_LABEL", "title-1");
        let second = inspection("dvd-v1-second", "SAME_LABEL", "title-1");
        store
            .upsert_optical_inspection(&first)
            .await
            .expect("first inspection");
        store
            .upsert_optical_inspection(&second)
            .await
            .expect("second inspection");

        assert_eq!(
            store
                .optical_disc("dvd-v1-first")
                .await
                .expect("first disc")
                .expect("present")
                .volume_label
                .as_deref(),
            Some("SAME_LABEL")
        );
        assert!(store
            .optical_disc("dvd-v1-second")
            .await
            .expect("second disc")
            .is_some());

        let mut invalid = inspection("dvd-v1-rollback", "ROLLBACK", "title-1");
        invalid.titles[0].matched_item_id = Some(999_999);
        invalid.titles[0].match_kind = Some(OpticalMatchKind::Movie);
        assert!(store.upsert_optical_inspection(&invalid).await.is_err());
        assert!(store
            .optical_disc("dvd-v1-rollback")
            .await
            .expect("rollback lookup")
            .is_none());
    }

    #[tokio::test]
    async fn optical_progress_is_per_user_monotonic_and_deletes_with_the_user() {
        let store = SqliteStore::open_in_memory().expect("store");
        let user = store
            .create_user("viewer", "hash", false)
            .await
            .expect("user");
        let admin = store
            .create_user("admin", "hash", true)
            .await
            .expect("admin");
        assert_eq!(
            store.optical_play_grant(user.id).await.expect("grant"),
            Some(false)
        );
        assert_eq!(
            store
                .optical_play_grant(admin.id)
                .await
                .expect("admin grant"),
            Some(true)
        );
        assert!(store
            .set_optical_play_grant(user.id, true)
            .await
            .expect("set grant"));
        assert_eq!(
            store.optical_play_grant(user.id).await.expect("grant"),
            Some(true)
        );
        store
            .upsert_optical_inspection(&inspection("dvd-v1-progress", "DISC", "main"))
            .await
            .expect("inspection");

        let current = store
            .put_optical_progress(&OpticalProgressWrite {
                user_id: user.id,
                disc_id: "dvd-v1-progress".to_owned(),
                title_id: "main".to_owned(),
                angle: 1,
                position_ms: 960,
                duration_ms: None,
                audio_selection_json: Some(r#"{"stream":2}"#.to_owned()),
                subtitle_selection_json: None,
                recorded_at_ms: Some(200),
            })
            .await
            .expect("current progress");
        assert!(current.watched);

        let stale = store
            .put_optical_progress(&OpticalProgressWrite {
                user_id: user.id,
                disc_id: "dvd-v1-progress".to_owned(),
                title_id: "main".to_owned(),
                angle: 1,
                position_ms: 100,
                duration_ms: None,
                audio_selection_json: None,
                subtitle_selection_json: None,
                recorded_at_ms: Some(100),
            })
            .await
            .expect("stale replay");
        assert_eq!(stale.position_ms, 960);
        assert!(stale.watched);

        store.delete_user(user.id).await.expect("delete user");
        assert!(store
            .optical_progress(user.id, "dvd-v1-progress", "main", 1)
            .await
            .expect("progress lookup")
            .is_none());
    }
}
