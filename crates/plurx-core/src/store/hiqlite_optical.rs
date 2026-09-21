use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::OpticalStore;
use crate::error::StoreError;
use crate::optical::{
    OpticalDisc, OpticalFormat, OpticalInspection, OpticalMatchKind, OpticalProgress,
    OpticalProgressWrite, OpticalTitle, OpticalTitleLocator, OPTICAL_SCHEMA,
};

pub(super) fn migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    super::hiqlite_library_channels::split_schema_statements(OPTICAL_SCHEMA)
        .into_iter()
        .map(|statement| {
            validate_sql(&statement)?;
            Ok((statement, params!()))
        })
        .collect()
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    client
        .txn(migration_statements()?)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(())
}

struct DiscRow {
    disc_id: String,
    fingerprint_version: i64,
    fingerprint_evidence_json: String,
    format: String,
    volume_label: Option<String>,
    display_title: Option<String>,
    metadata_json: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for DiscRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            disc_id: row.get("disc_id"),
            fingerprint_version: row.get("fingerprint_version"),
            fingerprint_evidence_json: row.get("fingerprint_evidence_json"),
            format: row.get("format"),
            volume_label: row.get("volume_label"),
            display_title: row.get("display_title"),
            metadata_json: row.get("metadata_json"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl TryFrom<DiscRow> for OpticalDisc {
    type Error = StoreError;

    fn try_from(row: DiscRow) -> Result<Self, Self::Error> {
        Ok(Self {
            disc_id: row.disc_id,
            fingerprint_version: u32::try_from(row.fingerprint_version)
                .map_err(|_| StoreError::Database("invalid optical fingerprint version".into()))?,
            fingerprint_evidence_json: row.fingerprint_evidence_json,
            format: OpticalFormat::parse(&row.format)
                .ok_or_else(|| StoreError::Database("invalid optical format".into()))?,
            volume_label: row.volume_label,
            display_title: row.display_title,
            metadata_json: row.metadata_json,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

struct TitleRow {
    disc_id: String,
    title_id: String,
    locator_json: String,
    angles: i64,
    facts_json: String,
    probe_json: String,
    chapters_json: String,
    duration_ms: Option<i64>,
    matched_item_id: Option<i64>,
    match_kind: Option<String>,
}

impl From<&mut Row<'_>> for TitleRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            disc_id: row.get("disc_id"),
            title_id: row.get("title_id"),
            locator_json: row.get("locator_json"),
            angles: row.get("angles"),
            facts_json: row.get("facts_json"),
            probe_json: row.get("probe_json"),
            chapters_json: row.get("chapters_json"),
            duration_ms: row.get("duration_ms"),
            matched_item_id: row.get("matched_item_id"),
            match_kind: row.get("match_kind"),
        }
    }
}

impl TryFrom<TitleRow> for OpticalTitle {
    type Error = StoreError;

    fn try_from(row: TitleRow) -> Result<Self, Self::Error> {
        Ok(Self {
            disc_id: row.disc_id,
            title_id: row.title_id,
            locator: serde_json::from_str::<OpticalTitleLocator>(&row.locator_json).map_err(
                |error| StoreError::Database(format!("invalid optical locator: {error}")),
            )?,
            angles: u32::try_from(row.angles)
                .ok()
                .filter(|angles| *angles > 0)
                .ok_or_else(|| StoreError::Database("invalid optical angle count".into()))?,
            facts: serde_json::from_str(&row.facts_json).map_err(|error| {
                StoreError::Database(format!("invalid optical media facts: {error}"))
            })?,
            probe_json: row.probe_json,
            chapters_json: row.chapters_json,
            duration_ms: row.duration_ms,
            matched_item_id: row.matched_item_id,
            match_kind: row
                .match_kind
                .map(|kind| {
                    OpticalMatchKind::parse(&kind)
                        .ok_or_else(|| StoreError::Database("invalid optical match kind".into()))
                })
                .transpose()?,
        })
    }
}

struct ProgressRow {
    user_id: i64,
    disc_id: String,
    title_id: String,
    angle: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
    watched: i64,
    audio_selection_json: Option<String>,
    subtitle_selection_json: Option<String>,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for ProgressRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            user_id: row.get("user_id"),
            disc_id: row.get("disc_id"),
            title_id: row.get("title_id"),
            angle: row.get("angle"),
            position_ms: row.get("position_ms"),
            duration_ms: row.get("duration_ms"),
            watched: row.get("watched"),
            audio_selection_json: row.get("audio_selection_json"),
            subtitle_selection_json: row.get("subtitle_selection_json"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl TryFrom<ProgressRow> for OpticalProgress {
    type Error = StoreError;

    fn try_from(row: ProgressRow) -> Result<Self, Self::Error> {
        Ok(Self {
            user_id: row.user_id,
            disc_id: row.disc_id,
            title_id: row.title_id,
            angle: u32::try_from(row.angle)
                .map_err(|_| StoreError::Database("invalid optical angle".into()))?,
            position_ms: row.position_ms,
            duration_ms: row.duration_ms,
            watched: row.watched != 0,
            audio_selection_json: row.audio_selection_json,
            subtitle_selection_json: row.subtitle_selection_json,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

const DISC_COLUMNS: &str = "disc_id, fingerprint_version, fingerprint_evidence_json, format, \
    volume_label, display_title, metadata_json, created_at_ms, updated_at_ms";
const TITLE_COLUMNS: &str =
    "disc_id, title_id, locator_json, angles, facts_json, probe_json, chapters_json, \
    duration_ms, matched_item_id, match_kind";
const PROGRESS_COLUMNS: &str = "user_id, disc_id, title_id, angle, position_ms, duration_ms, \
    watched, audio_selection_json, subtitle_selection_json, updated_at_ms";

#[async_trait]
impl OpticalStore for HiqliteAuthStore {
    async fn upsert_optical_inspection(
        &self,
        inspection: &OpticalInspection,
    ) -> Result<(), StoreError> {
        crate::optical::store::validate_inspection_write(inspection)
            .map_err(StoreError::Database)?;
        let mut statements = Vec::with_capacity(inspection.titles.len() + 1);
        let disc = &inspection.disc;
        statements.push((
            "INSERT INTO optical_discs
                 (disc_id, fingerprint_version, fingerprint_evidence_json, format,
                  volume_label, display_title, metadata_json, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT(disc_id) DO UPDATE SET
                 fingerprint_version = excluded.fingerprint_version,
                 fingerprint_evidence_json = excluded.fingerprint_evidence_json,
                 format = excluded.format,
                 volume_label = excluded.volume_label,
                 display_title = COALESCE(optical_discs.display_title, excluded.display_title),
                 metadata_json = CASE WHEN optical_discs.metadata_json = '{}'
                                      THEN excluded.metadata_json
                                      ELSE optical_discs.metadata_json END,
                 updated_at_ms = MAX(optical_discs.updated_at_ms, excluded.updated_at_ms)"
                .to_owned(),
            params!(
                &disc.disc_id,
                disc.fingerprint_version,
                &disc.fingerprint_evidence_json,
                disc.format.as_str(),
                &disc.volume_label,
                &disc.display_title,
                &disc.metadata_json,
                disc.created_at_ms,
                disc.updated_at_ms
            ),
        ));
        for title in &inspection.titles {
            let locator_json = serde_json::to_string(&title.locator)
                .map_err(|error| StoreError::Database(error.to_string()))?;
            let facts_json = serde_json::to_string(&title.facts)
                .map_err(|error| StoreError::Database(error.to_string()))?;
            statements.push((
                "INSERT INTO optical_titles
                     (disc_id, title_id, locator_json, angles, facts_json, probe_json,
                      chapters_json, duration_ms, matched_item_id, match_kind)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT(disc_id, title_id) DO UPDATE SET
                     locator_json = excluded.locator_json,
                     angles = excluded.angles,
                     facts_json = excluded.facts_json,
                     probe_json = excluded.probe_json,
                     chapters_json = excluded.chapters_json,
                     duration_ms = excluded.duration_ms"
                    .to_owned(),
                params!(
                    &title.disc_id,
                    &title.title_id,
                    locator_json,
                    title.angles,
                    facts_json,
                    &title.probe_json,
                    &title.chapters_json,
                    title.duration_ms,
                    title.matched_item_id,
                    title.match_kind.map(OpticalMatchKind::as_str)
                ),
            ));
        }
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        self.client()
            .txn(statements)
            .await
            .map_err(database_error)?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    async fn optical_disc(&self, disc_id: &str) -> Result<Option<OpticalDisc>, StoreError> {
        crate::optical::store::validate_optical_id("disc id", disc_id)
            .map_err(StoreError::Database)?;
        let sql = format!("SELECT {DISC_COLUMNS} FROM optical_discs WHERE disc_id = $1");
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<DiscRow, _>(sql, params!(disc_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map(TryInto::try_into)
            .transpose()
    }

    async fn optical_titles(&self, disc_id: &str) -> Result<Vec<OpticalTitle>, StoreError> {
        let sql = format!(
            "SELECT {TITLE_COLUMNS} FROM optical_titles WHERE disc_id = $1 ORDER BY title_id"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<TitleRow, _>(sql, params!(disc_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn optical_title(
        &self,
        disc_id: &str,
        title_id: &str,
    ) -> Result<Option<OpticalTitle>, StoreError> {
        let sql = format!(
            "SELECT {TITLE_COLUMNS} FROM optical_titles \
             WHERE disc_id = $1 AND title_id = $2"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<TitleRow, _>(sql, params!(disc_id, title_id))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map(TryInto::try_into)
            .transpose()
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
            return Err(StoreError::Database("invalid optical match".into()));
        }
        let sql = "UPDATE optical_titles SET matched_item_id = $3, match_kind = $4 \
                   WHERE disc_id = $1 AND title_id = $2";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(
                sql,
                params!(
                    disc_id,
                    title_id,
                    matched_item_id,
                    match_kind.map(OpticalMatchKind::as_str)
                ),
            )
            .await
            .map_err(database_error)?
            == 1)
    }

    async fn optical_progress(
        &self,
        user_id: i64,
        disc_id: &str,
        title_id: &str,
        angle: u32,
    ) -> Result<Option<OpticalProgress>, StoreError> {
        let sql = format!(
            "SELECT {PROGRESS_COLUMNS} FROM optical_progress \
             WHERE user_id = $1 AND disc_id = $2 AND title_id = $3 AND angle = $4"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<ProgressRow, _>(sql, params!(user_id, disc_id, title_id, angle))
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
            .map(TryInto::try_into)
            .transpose()
    }

    async fn put_optical_progress(
        &self,
        progress: &OpticalProgressWrite,
    ) -> Result<OpticalProgress, StoreError> {
        crate::optical::store::validate_progress_write(progress).map_err(StoreError::Database)?;
        let title = self
            .optical_title(&progress.disc_id, &progress.title_id)
            .await?
            .ok_or_else(|| StoreError::Database("unknown optical title".into()))?;
        let duration = title.duration_ms.or(progress.duration_ms);
        if duration.is_some_and(|duration| progress.position_ms > duration) {
            return Err(StoreError::Database(
                "optical progress position exceeds the known title duration".into(),
            ));
        }
        let watched = duration.is_some_and(|duration| {
            duration > 0 && progress.position_ms as f64 / duration as f64 >= 0.95
        });
        let now_ms = self
            .now()?
            .checked_mul(1000)
            .ok_or_else(|| StoreError::Database("clock overflow".into()))?;
        let at = progress.recorded_at_ms.unwrap_or(now_ms).clamp(0, now_ms);
        let sql = "INSERT INTO optical_progress
                     (user_id, disc_id, title_id, angle, position_ms, duration_ms, watched,
                      audio_selection_json, subtitle_selection_json, updated_at_ms)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                   ON CONFLICT(user_id, disc_id, title_id, angle) DO UPDATE SET
                     position_ms = excluded.position_ms,
                     duration_ms = COALESCE(excluded.duration_ms, optical_progress.duration_ms),
                     watched = optical_progress.watched OR excluded.watched,
                     audio_selection_json = COALESCE(excluded.audio_selection_json,
                                                     optical_progress.audio_selection_json),
                     subtitle_selection_json = COALESCE(excluded.subtitle_selection_json,
                                                        optical_progress.subtitle_selection_json),
                     updated_at_ms = excluded.updated_at_ms
                   WHERE $11 = 1 OR excluded.updated_at_ms >= optical_progress.updated_at_ms
                   RETURNING user_id, disc_id, title_id, angle, position_ms, duration_ms,
                             watched, audio_selection_json, subtitle_selection_json, updated_at_ms";
        validate_sql(sql)?;
        let returned = self
            .client()
            .execute_returning_map::<_, ProgressRow>(
                sql,
                params!(
                    progress.user_id,
                    &progress.disc_id,
                    &progress.title_id,
                    progress.angle,
                    progress.position_ms,
                    duration,
                    watched,
                    &progress.audio_selection_json,
                    &progress.subtitle_selection_json,
                    at,
                    progress.recorded_at_ms.is_none()
                ),
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if let Some(row) = returned.into_iter().next() {
            return row.try_into();
        }
        self.optical_progress(
            progress.user_id,
            &progress.disc_id,
            &progress.title_id,
            progress.angle,
        )
        .await?
        .ok_or_else(|| StoreError::Database("stale optical replay found no current row".into()))
    }

    async fn delete_user_optical_history(&self, user_id: i64) -> Result<u64, StoreError> {
        let sql = "DELETE FROM optical_progress WHERE user_id = $1";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(sql, params!(user_id))
            .await
            .map_err(database_error)? as u64)
    }

    async fn clear_optical_matches_for_item(&self, item_id: i64) -> Result<u64, StoreError> {
        let sql = "UPDATE optical_titles SET matched_item_id = NULL, match_kind = NULL \
                   WHERE matched_item_id = $1";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(sql, params!(item_id))
            .await
            .map_err(database_error)? as u64)
    }

    async fn optical_play_grant(&self, user_id: i64) -> Result<Option<bool>, StoreError> {
        let sql = "SELECT COALESCE(
                       (SELECT granted FROM user_grants
                         WHERE user_id = users.id AND grant_name = $2),
                       users.is_admin) AS granted
                     FROM users WHERE id = $1";
        validate_sql(sql)?;
        let rows = self
            .client()
            .query_consistent_map::<GrantedRow, _>(
                sql,
                params!(user_id, crate::optical::OPTICAL_PLAY_GRANT),
            )
            .await
            .map_err(database_error)?;
        Ok(rows.into_iter().next().map(|row| row.granted != 0))
    }

    async fn set_optical_play_grant(
        &self,
        user_id: i64,
        granted: bool,
    ) -> Result<bool, StoreError> {
        let sql = "INSERT INTO user_grants (user_id, grant_name, granted, updated_at_ms)
                   SELECT id, $2, $3, $4 FROM users WHERE id = $1
                   ON CONFLICT(user_id, grant_name) DO UPDATE SET
                     granted = excluded.granted,
                     updated_at_ms = excluded.updated_at_ms";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(
                sql,
                params!(
                    user_id,
                    crate::optical::OPTICAL_PLAY_GRANT,
                    granted,
                    self.now()?.saturating_mul(1000)
                ),
            )
            .await
            .map_err(database_error)?
            == 1)
    }
}

struct GrantedRow {
    granted: i64,
}

impl From<&mut Row<'_>> for GrantedRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            granted: row.get("granted"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_hiqlite_migration_is_bounded_valid_sql() {
        let statements = migration_statements().expect("migration statements");
        assert_eq!(statements.len(), 6);
        assert!(statements
            .iter()
            .all(|(statement, _)| !statement.trim().is_empty()));
    }
}
