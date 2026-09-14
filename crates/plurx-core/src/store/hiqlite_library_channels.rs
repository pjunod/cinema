//! Replicated persistence for deterministic library channels.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::LibraryChannelStore;
use crate::error::StoreError;
use crate::library_channels::{
    generation_content_digest, ChannelBuildMutation, ChannelCandidate, ChannelGenerationEntry,
    ChannelItemKind, ChannelMutation, ChannelVisibility, LibraryChannel, LibraryChannelBuildClaim,
    LibraryChannelBuildFailure, LibraryChannelDelete, LibraryChannelGeneration,
    LibraryChannelPublication, LibraryChannelUpdate, NewLibraryChannel, CHANNELS_PER_USER_MAX,
    CHANNELS_SERVER_MAX, CHANNEL_ABANDONED_BUILD_RETENTION_MS, CHANNEL_GENERATION_STAGE_MAX,
    CHANNEL_PRUNE_BATCH_MAX, CHANNEL_REQUESTS_PER_USER_MAX, CHANNEL_SUPERSEDED_RETENTION_MS,
    LIBRARY_CHANNELS_SCHEMA, LIBRARY_CHANNEL_BUILD_STATE_SCHEMA,
};

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    client
        .txn(migration_statements()?)
        .await
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    split_schema_statements(LIBRARY_CHANNELS_SCHEMA)
        .into_iter()
        .map(|statement| {
            validate_sql(&statement)?;
            Ok((statement, params!()))
        })
        .collect()
}

pub(super) fn build_state_migration_statements(
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    split_schema_statements(LIBRARY_CHANNEL_BUILD_STATE_SCHEMA)
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

const CHANNEL_COLS: &str = "c.id, c.owner_user_id, c.name, c.description, c.visibility, \
    c.enabled, c.definition_revision, c.recipe_json, c.seed, c.active_generation_id, \
    c.active_epoch_ms, c.pending_generation_id, c.pending_epoch_ms, c.build_state, \
    c.build_error_code, c.build_error_message, c.build_candidate_count, c.build_entry_count, \
    c.build_last_attempt_ms, c.build_last_success_ms, c.last_auto_build_ms, c.created_at_ms, \
    c.updated_at_ms";

struct ChannelRow {
    id: String,
    owner_user_id: i64,
    name: String,
    description: String,
    visibility: String,
    enabled: i64,
    definition_revision: i64,
    recipe_json: String,
    seed: Vec<u8>,
    active_generation_id: Option<String>,
    active_epoch_ms: Option<i64>,
    pending_generation_id: Option<String>,
    pending_epoch_ms: Option<i64>,
    build_state: String,
    build_error_code: Option<String>,
    build_error_message: Option<String>,
    build_candidate_count: i64,
    build_entry_count: i64,
    build_last_attempt_ms: Option<i64>,
    build_last_success_ms: Option<i64>,
    last_auto_build_ms: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
    favourite: i64,
}

impl From<&mut Row<'_>> for ChannelRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            owner_user_id: row.get("owner_user_id"),
            name: row.get("name"),
            description: row.get("description"),
            visibility: row.get("visibility"),
            enabled: row.get("enabled"),
            definition_revision: row.get("definition_revision"),
            recipe_json: row.get("recipe_json"),
            seed: row.get("seed"),
            active_generation_id: row.get("active_generation_id"),
            active_epoch_ms: row.get("active_epoch_ms"),
            pending_generation_id: row.get("pending_generation_id"),
            pending_epoch_ms: row.get("pending_epoch_ms"),
            build_state: row.get("build_state"),
            build_error_code: row.get("build_error_code"),
            build_error_message: row.get("build_error_message"),
            build_candidate_count: row.get("build_candidate_count"),
            build_entry_count: row.get("build_entry_count"),
            build_last_attempt_ms: row.get("build_last_attempt_ms"),
            build_last_success_ms: row.get("build_last_success_ms"),
            last_auto_build_ms: row.get("last_auto_build_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
            favourite: row.get("favourite"),
        }
    }
}

impl TryFrom<ChannelRow> for LibraryChannel {
    type Error = StoreError;

    fn try_from(row: ChannelRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            owner_user_id: row.owner_user_id,
            name: row.name,
            description: row.description,
            visibility: ChannelVisibility::parse(&row.visibility).ok_or_else(|| {
                StoreError::Database(format!("unknown channel visibility `{}`", row.visibility))
            })?,
            enabled: row.enabled != 0,
            revision: row.definition_revision,
            recipe: serde_json::from_str(&row.recipe_json).map_err(|error| {
                StoreError::Database(format!("library channel recipe: {error}"))
            })?,
            seed: <[u8; 32]>::try_from(row.seed).map_err(|_| {
                StoreError::Database("library channel seed is not 32 bytes".to_owned())
            })?,
            active_generation_id: row.active_generation_id,
            active_epoch_ms: row.active_epoch_ms,
            pending_generation_id: row.pending_generation_id,
            pending_epoch_ms: row.pending_epoch_ms,
            build_state: row.build_state,
            build_error_code: row.build_error_code,
            build_error_message: row.build_error_message,
            build_candidate_count: row.build_candidate_count,
            build_entry_count: row.build_entry_count,
            build_last_attempt_ms: row.build_last_attempt_ms,
            build_last_success_ms: row.build_last_success_ms,
            last_auto_build_ms: row.last_auto_build_ms,
            favourite: row.favourite != 0,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

struct RequestRow {
    channel_id: String,
    operation_hash: String,
    result_revision: i64,
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

async fn request_ledger_full(
    store: &HiqliteAuthStore,
    user_id: i64,
    now_ms: i64,
) -> Result<bool, StoreError> {
    store
        .execute(
            "DELETE FROM library_channel_requests WHERE user_id = $1 AND expires_at_ms <= $2",
            params!(user_id, now_ms),
        )
        .await?;
    let count = store
        .client()
        .query_consistent_map::<CountRow, _>(
            "SELECT COUNT(*) AS count FROM library_channel_requests WHERE user_id = $1",
            params!(user_id),
        )
        .await?
        .into_iter()
        .next()
        .map_or(0, |row| row.count);
    Ok(count >= CHANNEL_REQUESTS_PER_USER_MAX)
}

impl From<&mut Row<'_>> for RequestRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            channel_id: row.get("channel_id"),
            operation_hash: row.get("operation_hash"),
            result_revision: row.get("result_revision"),
        }
    }
}

struct CatalogRow {
    item_id: i64,
    file_id: i64,
    file_size: i64,
    file_mtime: i64,
    duration_ms: i64,
    library_id: i64,
    kind: String,
    title: String,
    overview: String,
    show_overview: String,
    genres: String,
    tags: String,
    year: Option<i64>,
    show_id: Option<i64>,
    show_title: Option<String>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    special: i64,
    show_genres: String,
    show_tags: String,
}

impl From<&mut Row<'_>> for CatalogRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            item_id: row.get("item_id"),
            file_id: row.get("file_id"),
            file_size: row.get("file_size"),
            file_mtime: row.get("file_mtime"),
            duration_ms: row.get("duration_ms"),
            library_id: row.get("library_id"),
            kind: row.get("kind"),
            title: row.get("title"),
            overview: row.get("overview"),
            show_overview: row.get("show_overview"),
            genres: row.get("genres"),
            tags: row.get("tags"),
            year: row.get("year"),
            show_id: row.get("show_id"),
            show_title: row.get("show_title"),
            season_number: row.get("season_number"),
            episode_number: row.get("episode_number"),
            special: row.get("special"),
            show_genres: row.get("show_genres"),
            show_tags: row.get("show_tags"),
        }
    }
}

struct GenerationRow {
    id: String,
    channel_id: String,
    definition_revision: i64,
    algorithm_version: i64,
    content_digest: String,
    state: String,
    entry_count: i64,
    loop_duration_ms: i64,
    build_claim_id: Option<String>,
    build_claim_expires_ms: Option<i64>,
    created_at_ms: i64,
}

impl From<&mut Row<'_>> for GenerationRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            channel_id: row.get("channel_id"),
            definition_revision: row.get("definition_revision"),
            algorithm_version: row.get("algorithm_version"),
            content_digest: row.get("content_digest"),
            state: row.get("state"),
            entry_count: row.get("entry_count"),
            loop_duration_ms: row.get("loop_duration_ms"),
            build_claim_id: row.get("build_claim_id"),
            build_claim_expires_ms: row.get("build_claim_expires_ms"),
            created_at_ms: row.get("created_at_ms"),
        }
    }
}

struct EntryRow {
    ordinal: i64,
    item_id: i64,
    file_id: i64,
    file_size: i64,
    file_mtime: i64,
    duration_ms: i64,
    cumulative_start_ms: i64,
    show_id: Option<i64>,
}

impl From<&mut Row<'_>> for EntryRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            ordinal: row.get("ordinal"),
            item_id: row.get("item_id"),
            file_id: row.get("file_id"),
            file_size: row.get("file_size"),
            file_mtime: row.get("file_mtime"),
            duration_ms: row.get("duration_ms"),
            cumulative_start_ms: row.get("cumulative_start_ms"),
            show_id: row.get("show_id"),
        }
    }
}

fn channel_query(scope: &str) -> String {
    format!(
        "SELECT {CHANNEL_COLS}, EXISTS(SELECT 1 FROM library_channel_favourites f \
         WHERE f.channel_id = c.id AND f.user_id = $1) AS favourite \
         FROM library_channels c WHERE {scope}"
    )
}

async fn query_channel(
    store: &HiqliteAuthStore,
    actor_user_id: i64,
    channel_id: &str,
) -> Result<Option<LibraryChannel>, StoreError> {
    let rows = store
        .client()
        .query_consistent_map::<ChannelRow, _>(
            channel_query("c.id = $2"),
            params!(actor_user_id, channel_id),
        )
        .await?;
    rows.into_iter().next().map(TryInto::try_into).transpose()
}

fn visible(channel: &LibraryChannel, actor_user_id: i64, management: bool) -> bool {
    channel.owner_user_id == actor_user_id
        || (channel.visibility == ChannelVisibility::Shared && channel.enabled)
        || management
}

fn decode_list(value: &str) -> Result<Vec<String>, StoreError> {
    serde_json::from_str(value)
        .map_err(|error| StoreError::Database(format!("channel catalogue list: {error}")))
}

#[async_trait]
impl LibraryChannelStore for HiqliteAuthStore {
    async fn subject_job(
        &self,
        query: crate::channel_subjects::JobQuery,
    ) -> Result<Option<crate::channel_subjects::SubjectJob>, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<SubjectRow, _>(query.sql(), params!())
            .await
            .map_err(database_error)?;
        rows.into_iter()
            .next()
            .map(|row| {
                let mut job: crate::channel_subjects::SubjectJob =
                    serde_json::from_str(&row.payload)
                        .map_err(|e| StoreError::Database(e.to_string()))?;
                job.state = row.state;
                Ok(job)
            })
            .transpose()
    }
    async fn subject_write(
        &self,
        write: crate::channel_subjects::JobWrite,
    ) -> Result<bool, StoreError> {
        let statements = write
            .sql()
            .into_iter()
            .map(|sql| {
                validate_sql(&sql)?;
                Ok((sql, params!()))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let results = self
            .client()
            .txn(statements)
            .await
            .map_err(database_error)?;
        let counts = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(counts.last().copied().unwrap_or(0) > 0)
    }
    async fn subject_decisions(
        &self,
        owner: i64,
        subject: &str,
        profile: &str,
        ids: &[(i64, String)],
    ) -> Result<Vec<crate::channel_subjects::CachedDecision>, StoreError> {
        let sql = crate::channel_subjects::decision_query(owner, subject, profile, ids);
        let rows = self
            .client()
            .query_consistent_map::<SubjectPayload, _>(sql, params!())
            .await
            .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let mut d: crate::channel_subjects::CachedDecision =
                    serde_json::from_str(&row.0)
                        .map_err(|e| StoreError::Database(e.to_string()))?;
                d.last_used_ms = row.1;
                Ok(d)
            })
            .collect()
    }

    async fn list_library_channels(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        management: bool,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<LibraryChannel>, StoreError> {
        let scope = if management && actor_is_admin {
            "c.id > $2"
        } else {
            "c.id > $2 AND (c.owner_user_id = $1 OR (c.visibility = 'shared' AND c.enabled = 1))"
        };
        self.client()
            .query_consistent_map::<ChannelRow, _>(
                format!("{} ORDER BY c.id LIMIT $3", channel_query(scope)),
                params!(
                    actor_user_id,
                    after_id.unwrap_or_default(),
                    limit.clamp(1, 100)
                ),
            )
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn get_library_channel(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
    ) -> Result<Option<LibraryChannel>, StoreError> {
        Ok(query_channel(self, actor_user_id, channel_id)
            .await?
            .filter(|channel| visible(channel, actor_user_id, actor_is_admin)))
    }

    async fn create_library_channel(
        &self,
        actor_is_admin: bool,
        channel: &NewLibraryChannel,
    ) -> Result<ChannelMutation<LibraryChannel>, StoreError> {
        if channel.visibility == ChannelVisibility::Shared && !actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        let replays = self
            .client()
            .query_consistent_map::<RequestRow, _>(
                "SELECT channel_id, operation_hash, result_revision \
                 FROM library_channel_requests WHERE user_id = $1 AND request_id = $2 \
                 AND expires_at_ms > $3",
                params!(
                    channel.owner_user_id,
                    channel.request_id.as_str(),
                    channel.now_ms
                ),
            )
            .await?;
        if let Some(replay) = replays.first() {
            if replay.operation_hash != channel.request_hash {
                return Ok(ChannelMutation::RequestConflict);
            }
            return query_channel(self, channel.owner_user_id, &replay.channel_id)
                .await?
                .map(ChannelMutation::Replay)
                .ok_or_else(|| {
                    StoreError::Database("channel request points to a missing channel".to_owned())
                });
        }
        if request_ledger_full(self, channel.owner_user_id, channel.now_ms).await? {
            return Ok(ChannelMutation::RequestLedgerFull);
        }
        let recipe = serde_json::to_string(&channel.recipe)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let results = self
            .client()
            .txn([
                (
                    "INSERT INTO library_channels \
                     (id, owner_user_id, name, description, visibility, enabled, \
                      definition_revision, recipe_json, seed, build_state, created_at_ms, updated_at_ms) \
                     SELECT $1, $2, $3, $4, $5, $6, 1, $7, $8, 'queued', $9, $9 \
                     WHERE (SELECT COUNT(*) FROM library_channels WHERE owner_user_id = $2) < $10 \
                       AND (SELECT COUNT(*) FROM library_channels) < $11 \
                       AND (SELECT COUNT(*) FROM library_channel_requests WHERE user_id = $2) < $12",
                    params!(
                        channel.id.as_str(),
                        channel.owner_user_id,
                        channel.name.as_str(),
                        channel.description.as_str(),
                        channel.visibility.as_str(),
                        channel.enabled,
                        recipe,
                        channel.seed.as_slice(),
                        channel.now_ms,
                        CHANNELS_PER_USER_MAX,
                        CHANNELS_SERVER_MAX,
                        CHANNEL_REQUESTS_PER_USER_MAX
                    ),
                ),
                (
                    "INSERT INTO library_channel_requests \
                     (user_id, request_id, operation_hash, channel_id, result_revision, \
                      created_at_ms, expires_at_ms) \
                     SELECT $1, $2, $3, $4, 1, $5, $6 \
                     WHERE EXISTS(SELECT 1 FROM library_channels WHERE id = $4) \
                       AND (SELECT COUNT(*) FROM library_channel_requests WHERE user_id = $1) < $7",
                    params!(
                        channel.owner_user_id,
                        channel.request_id.as_str(),
                        channel.request_hash.as_str(),
                        channel.id.as_str(),
                        channel.now_ms,
                        channel.now_ms + 86_400_000,
                        CHANNEL_REQUESTS_PER_USER_MAX
                    ),
                ),
            ])
            .await?;
        let counts = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if counts == [0, 0]
            && request_ledger_full(self, channel.owner_user_id, channel.now_ms).await?
        {
            return Ok(ChannelMutation::RequestLedgerFull);
        }
        if counts != [1, 1] {
            return Ok(ChannelMutation::LimitExceeded);
        }
        query_channel(self, channel.owner_user_id, &channel.id)
            .await?
            .map(ChannelMutation::Applied)
            .ok_or_else(|| StoreError::Database("created channel disappeared".to_owned()))
    }

    async fn update_library_channel(
        &self,
        update: &LibraryChannelUpdate,
    ) -> Result<ChannelMutation<LibraryChannel>, StoreError> {
        if update.visibility == ChannelVisibility::Shared && !update.actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        let Some(current) = query_channel(self, update.actor_user_id, &update.channel_id).await?
        else {
            return Ok(ChannelMutation::NotFound);
        };
        if current.owner_user_id != update.actor_user_id && !update.actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        let replays = self
            .client()
            .query_consistent_map::<RequestRow, _>(
                "SELECT channel_id, operation_hash, result_revision FROM library_channel_requests \
                 WHERE user_id = $1 AND request_id = $2 AND expires_at_ms > $3",
                params!(
                    update.actor_user_id,
                    update.request_id.as_str(),
                    update.now_ms
                ),
            )
            .await?;
        if let Some(replay) = replays.first() {
            if replay.operation_hash != update.request_hash {
                return Ok(ChannelMutation::RequestConflict);
            }
            if current.revision == replay.result_revision {
                return Ok(ChannelMutation::Replay(current));
            }
            return Ok(ChannelMutation::Stale);
        }
        if request_ledger_full(self, update.actor_user_id, update.now_ms).await? {
            return Ok(ChannelMutation::RequestLedgerFull);
        }
        if current.revision != update.expected_revision {
            return Ok(ChannelMutation::Stale);
        }
        let recipe = serde_json::to_string(&update.recipe)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let results = self
            .client()
            .txn([
                (
                    "UPDATE library_channels SET name = $1, description = $2, visibility = $3, \
                     enabled = $4, definition_revision = definition_revision + 1, recipe_json = $5, \
                     seed = $6, build_state = 'queued', build_error_code = NULL, \
                     build_error_message = NULL, updated_at_ms = $7 WHERE id = $8 AND definition_revision = $9 \
                     AND (owner_user_id = $10 OR $11) \
                     AND (SELECT COUNT(*) FROM library_channel_requests WHERE user_id = $10) < $12",
                    params!(
                        update.name.as_str(), update.description.as_str(), update.visibility.as_str(),
                        update.enabled, recipe, update.seed.as_slice(), update.now_ms,
                        update.channel_id.as_str(), update.expected_revision, update.actor_user_id,
                        update.actor_is_admin,
                        CHANNEL_REQUESTS_PER_USER_MAX
                    ),
                ),
                (
                    "INSERT INTO library_channel_requests \
                     (user_id, request_id, operation_hash, channel_id, result_revision, \
                      created_at_ms, expires_at_ms) \
                     SELECT $1, $2, $3, $4, $5, $6, $7 \
                     WHERE EXISTS(SELECT 1 FROM library_channels WHERE id = $4 \
                       AND definition_revision = $5) \
                       AND (SELECT COUNT(*) FROM library_channel_requests WHERE user_id = $1) < $8",
                    params!(
                        update.actor_user_id, update.request_id.as_str(), update.request_hash.as_str(),
                        update.channel_id.as_str(), update.expected_revision + 1, update.now_ms,
                        update.now_ms + 86_400_000,
                        CHANNEL_REQUESTS_PER_USER_MAX
                    ),
                ),
            ])
            .await?;
        let counts = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if counts != [1, 1] {
            return Ok(ChannelMutation::Stale);
        }
        query_channel(self, update.actor_user_id, &update.channel_id)
            .await?
            .map(ChannelMutation::Applied)
            .ok_or_else(|| StoreError::Database("updated channel disappeared".to_owned()))
    }

    async fn delete_library_channel(
        &self,
        deletion: &LibraryChannelDelete,
    ) -> Result<ChannelMutation<()>, StoreError> {
        let replays = self
            .client()
            .query_consistent_map::<RequestRow, _>(
                "SELECT channel_id, operation_hash, result_revision \
                 FROM library_channel_requests WHERE user_id = $1 AND request_id = $2 \
                 AND expires_at_ms > $3",
                params!(
                    deletion.actor_user_id,
                    deletion.request_id.as_str(),
                    deletion.now_ms
                ),
            )
            .await?;
        if let Some(replay) = replays.first() {
            if replay.channel_id != deletion.channel_id
                || replay.operation_hash != deletion.request_hash
            {
                return Ok(ChannelMutation::RequestConflict);
            }
            return Ok(ChannelMutation::Replay(()));
        }
        if request_ledger_full(self, deletion.actor_user_id, deletion.now_ms).await? {
            return Ok(ChannelMutation::RequestLedgerFull);
        }
        let Some(current) =
            query_channel(self, deletion.actor_user_id, &deletion.channel_id).await?
        else {
            return Ok(ChannelMutation::NotFound);
        };
        if current.owner_user_id != deletion.actor_user_id && !deletion.actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        if current.revision != deletion.expected_revision {
            return Ok(ChannelMutation::Stale);
        }
        let results = self
            .client()
            .txn([
                (
                    "INSERT INTO library_channel_requests \
                 (user_id, request_id, operation_hash, channel_id, result_revision, \
                  created_at_ms, expires_at_ms) \
                 SELECT $1, $2, $3, $4, 0, $5, $6 \
                 WHERE EXISTS(SELECT 1 FROM library_channels WHERE id = $4 \
                   AND definition_revision = $7 AND (owner_user_id = $1 OR $8)) \
                   AND (SELECT COUNT(*) FROM library_channel_requests WHERE user_id = $1) < $9",
                    params!(
                        deletion.actor_user_id,
                        deletion.request_id.as_str(),
                        deletion.request_hash.as_str(),
                        deletion.channel_id.as_str(),
                        deletion.now_ms,
                        deletion.now_ms + 86_400_000,
                        deletion.expected_revision,
                        deletion.actor_is_admin,
                        CHANNEL_REQUESTS_PER_USER_MAX
                    ),
                ),
                (
                    "DELETE FROM library_channels WHERE id = $1 AND definition_revision = $2 \
                 AND (owner_user_id = $3 OR $4) \
                 AND EXISTS(SELECT 1 FROM library_channel_requests \
                   WHERE user_id = $3 AND request_id = $5 AND operation_hash = $6)",
                    params!(
                        deletion.channel_id.as_str(),
                        deletion.expected_revision,
                        deletion.actor_user_id,
                        deletion.actor_is_admin,
                        deletion.request_id.as_str(),
                        deletion.request_hash.as_str()
                    ),
                ),
            ])
            .await?;
        let counts = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(if counts == [1, 1] {
            ChannelMutation::Applied(())
        } else {
            ChannelMutation::Stale
        })
    }

    async fn set_library_channel_favourite(
        &self,
        user_id: i64,
        channel_id: &str,
        favourite: bool,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(channel) = query_channel(self, user_id, channel_id).await? else {
            return Ok(false);
        };
        if channel.owner_user_id != user_id
            && (channel.visibility != ChannelVisibility::Shared || !channel.enabled)
        {
            return Ok(false);
        }
        let changed = if favourite {
            self.execute(
                "INSERT INTO library_channel_favourites (user_id, channel_id, created_at_ms) \
                 SELECT $1, $2, $3 WHERE EXISTS(SELECT 1 FROM library_channels \
                   WHERE id = $2 AND (owner_user_id = $1 OR (visibility = 'shared' AND enabled = 1))) \
                 ON CONFLICT(user_id, channel_id) DO UPDATE SET created_at_ms = created_at_ms",
                params!(user_id, channel_id, now_ms),
            )
            .await?
        } else {
            self.execute(
                "DELETE FROM library_channel_favourites WHERE user_id = $1 AND channel_id = $2",
                params!(user_id, channel_id),
            )
            .await?
        };
        Ok(changed <= 1)
    }

    async fn library_channel_catalog_snapshot(
        &self,
        limit: i64,
    ) -> Result<Vec<ChannelCandidate>, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<CatalogRow, _>(
                "SELECT i.id AS item_id, f.id AS file_id, f.size AS file_size, \
                    f.mtime AS file_mtime, f.duration_ms AS duration_ms, \
                    i.library_id, i.kind, i.title, \
                    COALESCE(i.overview, '') AS overview, COALESCE(sh.overview, '') AS show_overview, \
                    i.genres, i.tags, \
                    COALESCE(CAST(substr(i.air_date, 1, 4) AS INTEGER), i.year, sh.year) AS year, \
                    sh.id AS show_id, sh.title AS show_title, s.season_number, i.episode_number, \
                    CASE WHEN s.season_number = 0 THEN 1 ELSE 0 END AS special, \
                    COALESCE(sh.genres, '[]') AS show_genres, COALESCE(sh.tags, '[]') AS show_tags \
             FROM items i \
             LEFT JOIN items s ON i.kind = 'episode' AND s.id = i.parent_id AND s.kind = 'season' \
             LEFT JOIN items sh ON i.kind = 'episode' AND sh.id = s.parent_id AND sh.kind = 'show' \
             JOIN files f ON f.item_id = i.id AND f.probe_json IS NOT NULL \
               AND f.video_codec IS NOT NULL AND f.duration_ms > 0 AND f.duration_ms <= 86400000 \
             WHERE i.kind IN ('movie','episode') ORDER BY i.id, f.id LIMIT $1",
                params!(limit.clamp(1, 100_001)),
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let kind = match row.kind.as_str() {
                    "movie" => ChannelItemKind::Movie,
                    "episode" => ChannelItemKind::Episode,
                    value => {
                        return Err(StoreError::Database(format!(
                            "unsupported channel kind `{value}`"
                        )))
                    }
                };
                let show_genres = decode_list(&row.show_genres)?;
                let mut genres = decode_list(&row.genres)?;
                genres.extend(show_genres.clone());
                genres.sort_by_key(|value| value.to_lowercase());
                genres.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
                let show_tags = decode_list(&row.show_tags)?;
                let mut tags = decode_list(&row.tags)?;
                tags.extend(show_tags.clone());
                tags.sort_by_key(|value| value.to_lowercase());
                tags.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
                Ok(ChannelCandidate {
                    item_id: row.item_id,
                    file_id: row.file_id,
                    file_size: row.file_size,
                    file_mtime: row.file_mtime,
                    duration_ms: row.duration_ms,
                    library_id: row.library_id,
                    kind,
                    title: row.title,
                    overview: row.overview,
                    show_overview: row.show_overview,
                    genres,
                    tags,
                    show_genres,
                    show_tags,
                    year: row.year.and_then(|value| i32::try_from(value).ok()),
                    show_id: row.show_id,
                    show_title: row.show_title,
                    season_number: row
                        .season_number
                        .and_then(|value| i32::try_from(value).ok()),
                    episode_number: row
                        .episode_number
                        .and_then(|value| i32::try_from(value).ok()),
                    special: row.special != 0,
                    explicitly_included: false,
                })
            })
            .collect()
    }

    async fn list_library_channel_refresh_candidates(
        &self,
        after_id: Option<&str>,
        limit: i64,
        now_ms: i64,
    ) -> Result<Vec<LibraryChannel>, StoreError> {
        self.client()
            .query_consistent_map::<ChannelRow, _>(
                format!(
                    "SELECT {CHANNEL_COLS}, 0 AS favourite FROM library_channels c \
                     WHERE c.id > $1 AND (c.build_state = 'queued' OR (c.enabled = 1 \
                       AND COALESCE(json_extract(c.recipe_json, '$.auto_refresh'), 1) = 1 \
                       AND (COALESCE(c.last_auto_build_ms, c.build_last_attempt_ms) IS NULL \
                         OR COALESCE(c.last_auto_build_ms, c.build_last_attempt_ms) <= $2) \
                     )) ORDER BY c.id LIMIT $3"
                ),
                params!(
                    after_id.unwrap_or_default(),
                    now_ms.saturating_sub(15 * 60 * 1_000),
                    limit.clamp(1, 200)
                ),
            )
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn prune_library_channel_state(
        &self,
        now_ms: i64,
        limit: i64,
    ) -> Result<i64, StoreError> {
        let limit = limit.clamp(1, CHANNEL_PRUNE_BATCH_MAX);
        let requests = self.execute(
            "DELETE FROM library_channel_requests WHERE rowid IN (SELECT rowid \
             FROM library_channel_requests WHERE expires_at_ms <= $1 ORDER BY expires_at_ms LIMIT $2)",
            params!(now_ms, limit),
        ).await?;
        let requests = requests as i64;
        let remaining = limit.saturating_sub(requests);
        let abandoned = if remaining > 0 {
            self.execute(
                "DELETE FROM library_channel_generations WHERE id IN (SELECT id \
                 FROM library_channel_generations WHERE state = 'building' \
                   AND build_claim_expires_ms <= $1 ORDER BY created_at_ms LIMIT $2)",
                params!(
                    now_ms.saturating_sub(CHANNEL_ABANDONED_BUILD_RETENTION_MS),
                    remaining
                ),
            )
            .await? as i64
        } else {
            0
        };
        let remaining = remaining.saturating_sub(abandoned);
        let superseded = if remaining > 0 {
            self.execute(
                "DELETE FROM library_channel_generations WHERE id IN (SELECT g.id \
                 FROM library_channel_generations g WHERE g.state = 'ready' AND g.created_at_ms <= $1 \
                   AND NOT EXISTS(SELECT 1 FROM library_channels c \
                     WHERE c.active_generation_id = g.id OR c.pending_generation_id = g.id) \
                 ORDER BY g.created_at_ms LIMIT $2)",
                params!(now_ms.saturating_sub(CHANNEL_SUPERSEDED_RETENTION_MS), remaining),
            ).await? as i64
        } else {
            0
        };
        Ok(requests
            .saturating_add(abandoned)
            .saturating_add(superseded))
    }

    async fn claim_library_channel_build(
        &self,
        claim: &LibraryChannelBuildClaim,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let results = self.client().txn([
            ("INSERT INTO library_channel_generations \
             (id, channel_id, definition_revision, algorithm_version, content_digest, state, \
              build_claim_id, build_claim_expires_ms, created_at_ms) \
             SELECT $1, $2, $3, 1, $4, 'building', $5, $6, $7 \
             WHERE EXISTS(SELECT 1 FROM library_channels WHERE id = $2 AND definition_revision = $3) \
               AND NOT EXISTS(SELECT 1 FROM library_channel_generations WHERE channel_id = $2 \
                 AND state = 'building' AND build_claim_expires_ms > $7)",
            params!(claim.generation_id.as_str(), claim.channel_id.as_str(), claim.expected_revision,
                claim.content_digest.as_str(), claim.claim_id.as_str(), claim.expires_at_ms, claim.now_ms)
            ),
            ("UPDATE library_channels SET build_state = 'building', build_error_code = NULL, \
              build_error_message = NULL, build_last_attempt_ms = $1 WHERE id = $2 \
              AND definition_revision = $3 AND EXISTS(SELECT 1 FROM library_channel_generations \
                WHERE id = $4 AND build_claim_id = $5)",
             params!(claim.now_ms, claim.channel_id.as_str(), claim.expected_revision,
                claim.generation_id.as_str(), claim.claim_id.as_str()))
        ]).await?.into_iter().collect::<Result<Vec<_>, _>>().map_err(database_error)?;
        Ok(if results == [1, 1] {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Busy
        })
    }

    async fn renew_library_channel_build(
        &self,
        channel_id: &str,
        generation_id: &str,
        claim_id: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let changed = self
            .execute(
                "UPDATE library_channel_generations SET build_claim_expires_ms = $1 \
                 WHERE id = $2 AND channel_id = $3 AND state = 'building' \
                   AND build_claim_id = $4 AND build_claim_expires_ms > $5",
                params!(expires_at_ms, generation_id, channel_id, claim_id, now_ms),
            )
            .await?;
        Ok(if changed == 1 {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Stale
        })
    }

    async fn stage_library_channel_entries(
        &self,
        channel_id: &str,
        generation_id: &str,
        claim_id: &str,
        entries: &[ChannelGenerationEntry],
        now_ms: i64,
    ) -> Result<ChannelBuildMutation, StoreError> {
        if entries.is_empty() || entries.len() > CHANNEL_GENERATION_STAGE_MAX {
            return Ok(ChannelBuildMutation::Invalid);
        }
        let statements = entries
            .iter()
            .map(|entry| {
                (
                    "INSERT INTO library_channel_entries \
             (generation_id, ordinal, item_id, file_id, file_size, file_mtime, duration_ms, \
              cumulative_start_ms, show_id) \
             SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9 \
             WHERE EXISTS(SELECT 1 FROM library_channel_generations WHERE id = $1 \
               AND channel_id = $10 AND state = 'building' AND build_claim_id = $11 \
               AND build_claim_expires_ms > $12)",
                    params!(
                        generation_id,
                        entry.ordinal,
                        entry.item_id,
                        entry.file_id,
                        entry.file_size,
                        entry.file_mtime,
                        entry.duration_ms,
                        entry.cumulative_start_ms,
                        entry.show_id,
                        channel_id,
                        claim_id,
                        now_ms
                    ),
                )
            })
            .collect::<Vec<_>>();
        let counts = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(if counts.iter().all(|count| *count == 1) {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Stale
        })
    }

    async fn publish_library_channel_generation(
        &self,
        publication: &LibraryChannelPublication,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let staged = self
            .client()
            .query_consistent_map::<EntryRow, _>(
                "SELECT ordinal, item_id, file_id, file_size, file_mtime, duration_ms, \
             cumulative_start_ms, show_id FROM library_channel_entries WHERE generation_id = $1 \
             ORDER BY ordinal",
                params!(publication.generation_id.as_str()),
            )
            .await?
            .into_iter()
            .map(|entry| {
                Ok(ChannelGenerationEntry {
                    ordinal: u32::try_from(entry.ordinal).map_err(|_| {
                        StoreError::Database("channel ordinal is out of range".to_owned())
                    })?,
                    item_id: entry.item_id,
                    file_id: entry.file_id,
                    file_size: entry.file_size,
                    file_mtime: entry.file_mtime,
                    duration_ms: entry.duration_ms,
                    cumulative_start_ms: entry.cumulative_start_ms,
                    show_id: entry.show_id,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let continuous = staged
            .iter()
            .enumerate()
            .scan(0_i64, |offset, (index, entry)| {
                let valid = entry.ordinal as usize == index && entry.cumulative_start_ms == *offset;
                *offset = offset.saturating_add(entry.duration_ms);
                Some(valid)
            })
            .all(|valid| valid);
        if !continuous
            || generation_content_digest(publication.seed, &staged) != publication.content_digest
        {
            return Ok(ChannelBuildMutation::Invalid);
        }
        let pointer_sql = if publication.pending {
            "UPDATE library_channels SET pending_generation_id = $1, pending_epoch_ms = $2, \
             build_state = 'ready', build_candidate_count = $3, build_entry_count = $3, \
             build_last_success_ms = $4, last_auto_build_ms = CASE WHEN $5 THEN $4 ELSE last_auto_build_ms END, \
             updated_at_ms = $4 WHERE id = $6 AND definition_revision = $7 \
             AND active_generation_id IS NOT NULL AND EXISTS(SELECT 1 FROM library_channel_generations g \
               WHERE g.id = $1 AND g.channel_id = $6 AND g.state = 'building' \
                 AND g.build_claim_id = $8 AND g.build_claim_expires_ms > $4 \
                 AND g.content_digest = $9 AND (SELECT COUNT(*) FROM library_channel_entries e \
                   WHERE e.generation_id = g.id) = $3 AND (SELECT COALESCE(MAX(e.cumulative_start_ms + e.duration_ms), 0) \
                   FROM library_channel_entries e WHERE e.generation_id = g.id) = $10)"
        } else {
            "UPDATE library_channels SET active_generation_id = $1, active_epoch_ms = $2, \
             pending_generation_id = NULL, pending_epoch_ms = NULL, build_state = 'ready', \
             build_candidate_count = $3, build_entry_count = $3, build_last_success_ms = $4, \
             last_auto_build_ms = CASE WHEN $5 THEN $4 ELSE last_auto_build_ms END, updated_at_ms = $4 \
             WHERE id = $6 AND definition_revision = $7 AND EXISTS(SELECT 1 FROM library_channel_generations g \
               WHERE g.id = $1 AND g.channel_id = $6 AND g.state = 'building' \
                 AND g.build_claim_id = $8 AND g.build_claim_expires_ms > $4 \
                 AND g.content_digest = $9 AND (SELECT COUNT(*) FROM library_channel_entries e \
                   WHERE e.generation_id = g.id) = $3 AND (SELECT COALESCE(MAX(e.cumulative_start_ms + e.duration_ms), 0) \
                   FROM library_channel_entries e WHERE e.generation_id = g.id) = $10)"
        };
        let pointer_sql = if let Some((job, claim)) = &publication.subject_claim {
            format!("{pointer_sql} AND EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE id=CAST(X'{}' AS TEXT) AND claim_id=CAST(X'{}' AS TEXT) AND channel_id=$6 AND channel_revision=$7 AND state='running' AND claim_expires_ms>$4)",hex::encode(job),hex::encode(claim))
        } else {
            pointer_sql.to_owned()
        };
        let results = self
            .client()
            .txn([
                (
                    "UPDATE library_channels SET active_generation_id = pending_generation_id, \
                     active_epoch_ms = pending_epoch_ms, pending_generation_id = NULL, \
                     pending_epoch_ms = NULL WHERE id = $1 AND pending_generation_id IS NOT NULL \
                       AND pending_epoch_ms <= $2"
                        .to_owned(),
                    params!(publication.channel_id.as_str(), publication.now_ms),
                ),
                (
                    pointer_sql,
                    params!(
                        publication.generation_id.as_str(),
                        publication.activation_epoch_ms,
                        publication.entry_count,
                        publication.now_ms,
                        publication.automatic,
                        publication.channel_id.as_str(),
                        publication.expected_revision,
                        publication.claim_id.as_str(),
                        publication.content_digest.as_str(),
                        publication.loop_duration_ms
                    ),
                ),
                (
                    "UPDATE library_channel_generations SET state = 'ready', entry_count = $1, \
              loop_duration_ms = $2, build_claim_id = NULL, build_claim_expires_ms = NULL \
              WHERE id = $3 AND channel_id = $4 AND state = 'building' AND build_claim_id = $5"
                        .to_owned(),
                    params!(
                        publication.entry_count,
                        publication.loop_duration_ms,
                        publication.generation_id.as_str(),
                        publication.channel_id.as_str(),
                        publication.claim_id.as_str()
                    ),
                ),
            ])
            .await?;
        let counts = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(if counts.len() == 3 && counts[1..] == [1, 1] {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Stale
        })
    }

    async fn fail_library_channel_build(
        &self,
        failure: &LibraryChannelBuildFailure,
    ) -> Result<ChannelBuildMutation, StoreError> {
        if let (Some(generation_id), Some(claim_id)) = (
            failure.generation_id.as_deref(),
            failure.claim_id.as_deref(),
        ) {
            self.execute(
                "UPDATE library_channel_generations SET build_claim_expires_ms = $1 \
                 WHERE id = $2 AND channel_id = $3 AND build_claim_id = $4",
                params!(
                    failure.now_ms,
                    generation_id,
                    failure.channel_id.as_str(),
                    claim_id
                ),
            )
            .await?;
        }
        let changed = self
            .execute(
                "UPDATE library_channels SET build_state = 'failed', build_error_code = $1, \
             build_error_message = $2, build_candidate_count = $3, build_last_attempt_ms = $4, \
             updated_at_ms = $4 WHERE id = $5 AND definition_revision = $6",
                params!(
                    failure.error_code.as_str(),
                    failure.error_message.as_str(),
                    failure.candidate_count,
                    failure.now_ms,
                    failure.channel_id.as_str(),
                    failure.expected_revision
                ),
            )
            .await?;
        Ok(if changed == 1 {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Stale
        })
    }

    async fn complete_library_channel_build_without_publication(
        &self,
        channel_id: &str,
        expected_revision: i64,
        candidate_count: i64,
        automatic: bool,
        now_ms: i64,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let changed = self.execute(
            "UPDATE library_channels SET build_state = 'ready', build_error_code = NULL, \
             build_error_message = NULL, build_candidate_count = $1, \
             build_entry_count = COALESCE((SELECT entry_count FROM library_channel_generations \
               WHERE id = COALESCE(pending_generation_id, active_generation_id)), 0), \
             build_last_attempt_ms = $2, build_last_success_ms = $2, \
             last_auto_build_ms = CASE WHEN $3 THEN $2 ELSE last_auto_build_ms END, updated_at_ms = $2 \
             WHERE id = $4 AND definition_revision = $5",
            params!(candidate_count, now_ms, automatic, channel_id, expected_revision),
        ).await?;
        Ok(if changed == 1 {
            ChannelBuildMutation::Applied
        } else {
            ChannelBuildMutation::Stale
        })
    }

    async fn read_library_channel_generation(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
        generation_id: &str,
    ) -> Result<Option<LibraryChannelGeneration>, StoreError> {
        let Some(channel) = query_channel(self, actor_user_id, channel_id).await? else {
            return Ok(None);
        };
        if !visible(&channel, actor_user_id, actor_is_admin) {
            return Ok(None);
        }
        let rows = self.client().query_consistent_map::<GenerationRow, _>(
            "SELECT id, channel_id, definition_revision, algorithm_version, content_digest, state, \
             entry_count, loop_duration_ms, build_claim_id, build_claim_expires_ms, created_at_ms \
             FROM library_channel_generations WHERE id = $1 AND channel_id = $2",
            params!(generation_id, channel_id)
        ).await?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let entries = self
            .client()
            .query_consistent_map::<EntryRow, _>(
                "SELECT ordinal, item_id, file_id, file_size, file_mtime, duration_ms, \
             cumulative_start_ms, show_id FROM library_channel_entries \
             WHERE generation_id = $1 ORDER BY ordinal",
                params!(generation_id),
            )
            .await?
            .into_iter()
            .map(|entry| {
                Ok(ChannelGenerationEntry {
                    ordinal: u32::try_from(entry.ordinal).map_err(|_| {
                        StoreError::Database("channel ordinal is out of range".to_owned())
                    })?,
                    item_id: entry.item_id,
                    file_id: entry.file_id,
                    file_size: entry.file_size,
                    file_mtime: entry.file_mtime,
                    duration_ms: entry.duration_ms,
                    cumulative_start_ms: entry.cumulative_start_ms,
                    show_id: entry.show_id,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(Some(LibraryChannelGeneration {
            id: row.id,
            channel_id: row.channel_id,
            definition_revision: row.definition_revision,
            algorithm_version: row.algorithm_version,
            content_digest: row.content_digest,
            state: row.state,
            entry_count: row.entry_count,
            loop_duration_ms: row.loop_duration_ms,
            build_claim_id: row.build_claim_id,
            build_claim_expires_ms: row.build_claim_expires_ms,
            created_at_ms: row.created_at_ms,
            entries,
        }))
    }
}

struct SubjectRow {
    payload: String,
    state: String,
}
impl From<&mut Row<'_>> for SubjectRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            payload: row.get("payload"),
            state: row.get("state"),
        }
    }
}
struct SubjectPayload(String, i64);
impl From<&mut Row<'_>> for SubjectPayload {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("payload"), row.get("last_used_ms"))
    }
}
pub(super) fn subject_migration_statements() -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    split_schema_statements(crate::channel_subjects::SCHEMA)
        .into_iter()
        .map(|sql| {
            validate_sql(&sql)?;
            Ok((sql, params!()))
        })
        .collect()
}

#[cfg(test)]
mod schema_tests {
    use super::migration_statements;

    #[test]
    fn migration_keeps_trigger_body_in_one_statement() {
        let statements = migration_statements().expect("valid replicated migration");
        let trigger = statements
            .iter()
            .map(|(statement, _)| statement)
            .find(|statement| {
                statement.starts_with(
                    "CREATE TRIGGER IF NOT EXISTS library_channel_session_recipes_request_delete",
                )
            })
            .expect("request cleanup trigger");

        assert!(trigger.contains("DELETE FROM library_channel_session_recipes"));
        assert!(trigger.ends_with("END"));
    }
}
