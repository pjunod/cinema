//! SQLite persistence for deterministic library channels.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::{conversion_err, SqliteStore};
use crate::error::StoreError;
use crate::library_channels::{
    generation_content_digest, ChannelBuildMutation, ChannelCandidate, ChannelGenerationEntry,
    ChannelItemKind, ChannelMutation, ChannelVisibility, LibraryChannel, LibraryChannelBuildClaim,
    LibraryChannelBuildFailure, LibraryChannelDelete, LibraryChannelGeneration,
    LibraryChannelPublication, LibraryChannelRecipe, LibraryChannelUpdate, NewLibraryChannel,
    CHANNELS_PER_USER_MAX, CHANNELS_SERVER_MAX, CHANNEL_ABANDONED_BUILD_RETENTION_MS,
    CHANNEL_GENERATION_STAGE_MAX, CHANNEL_PRUNE_BATCH_MAX, CHANNEL_REQUESTS_PER_USER_MAX,
    CHANNEL_SUPERSEDED_RETENTION_MS,
};
use crate::store::LibraryChannelStore;

const CHANNEL_COLS: &str = "c.id, c.owner_user_id, c.name, c.description, c.visibility, \
    c.enabled, c.definition_revision, c.recipe_json, c.seed, c.active_generation_id, \
    c.active_epoch_ms, c.pending_generation_id, c.pending_epoch_ms, c.build_state, \
    c.build_error_code, c.build_error_message, c.build_candidate_count, c.build_entry_count, \
    c.build_last_attempt_ms, c.build_last_success_ms, c.last_auto_build_ms, c.created_at_ms, \
    c.updated_at_ms";

fn channel_from_row(row: &Row<'_>) -> rusqlite::Result<LibraryChannel> {
    let visibility_raw: String = row.get(4)?;
    let visibility = ChannelVisibility::parse(&visibility_raw).ok_or_else(|| {
        conversion_err(4, format!("unknown channel visibility `{visibility_raw}`"))
    })?;
    let recipe_json: String = row.get(7)?;
    let recipe = serde_json::from_str::<LibraryChannelRecipe>(&recipe_json)
        .map_err(|error| conversion_err(7, format!("library channel recipe: {error}")))?;
    let seed: Vec<u8> = row.get(8)?;
    let seed = <[u8; 32]>::try_from(seed)
        .map_err(|_| conversion_err(8, "library channel seed is not 32 bytes".to_owned()))?;
    Ok(LibraryChannel {
        id: row.get(0)?,
        owner_user_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        visibility,
        enabled: row.get::<_, i64>(5)? != 0,
        revision: row.get(6)?,
        recipe,
        seed,
        active_generation_id: row.get(9)?,
        active_epoch_ms: row.get(10)?,
        pending_generation_id: row.get(11)?,
        pending_epoch_ms: row.get(12)?,
        build_state: row.get(13)?,
        build_error_code: row.get(14)?,
        build_error_message: row.get(15)?,
        build_candidate_count: row.get(16)?,
        build_entry_count: row.get(17)?,
        build_last_attempt_ms: row.get(18)?,
        build_last_success_ms: row.get(19)?,
        last_auto_build_ms: row.get(20)?,
        favourite: row.get::<_, i64>(23)? != 0,
        created_at_ms: row.get(21)?,
        updated_at_ms: row.get(22)?,
    })
}

fn recipe_json(recipe: &LibraryChannelRecipe) -> Result<String, StoreError> {
    serde_json::to_string(recipe).map_err(|error| StoreError::Database(error.to_string()))
}

fn channel_query(where_clause: &str) -> String {
    format!(
        "SELECT {CHANNEL_COLS}, EXISTS(SELECT 1 FROM library_channel_favourites f \
         WHERE f.channel_id = c.id AND f.user_id = ?1) AS favourite \
         FROM library_channels c WHERE {where_clause}"
    )
}

fn load_channel(
    conn: &rusqlite::Connection,
    actor_user_id: i64,
    channel_id: &str,
) -> Result<Option<LibraryChannel>, StoreError> {
    Ok(conn
        .query_row(
            &channel_query("c.id = ?2"),
            params![actor_user_id, channel_id],
            channel_from_row,
        )
        .optional()?)
}

fn request_slot_available(
    tx: &rusqlite::Transaction<'_>,
    user_id: i64,
    now_ms: i64,
) -> Result<bool, StoreError> {
    tx.execute(
        "DELETE FROM library_channel_requests WHERE user_id = ?1 AND expires_at_ms <= ?2",
        params![user_id, now_ms],
    )?;
    let live: i64 = tx.query_row(
        "SELECT COUNT(*) FROM library_channel_requests WHERE user_id = ?1",
        [user_id],
        |row| row.get(0),
    )?;
    Ok(live < CHANNEL_REQUESTS_PER_USER_MAX)
}

fn visible(channel: &LibraryChannel, actor_user_id: i64, management: bool) -> bool {
    channel.owner_user_id == actor_user_id
        || (channel.visibility == ChannelVisibility::Shared && channel.enabled)
        || management
}

fn parse_string_list(column: usize, value: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&value)
        .map_err(|error| conversion_err(column, format!("channel catalogue list: {error}")))
}

#[async_trait]
impl LibraryChannelStore for SqliteStore {
    async fn subject_job(
        &self,
        query: crate::channel_subjects::JobQuery,
    ) -> Result<Option<crate::channel_subjects::SubjectJob>, StoreError> {
        self.with_read(move |conn| {
            let row = conn
                .query_row(&query.sql(), [], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            row.map(|(payload, state)| {
                let mut job: crate::channel_subjects::SubjectJob =
                    serde_json::from_str(&payload)
                        .map_err(|e| StoreError::Database(e.to_string()))?;
                job.state = state;
                Ok(job)
            })
            .transpose()
        })
        .await
    }
    async fn subject_write(
        &self,
        write: crate::channel_subjects::JobWrite,
    ) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut changed = 0;
            for sql in write.sql() {
                changed = tx.execute(&sql, [])?;
            }
            tx.commit()?;
            Ok(changed > 0)
        })
        .await
    }
    async fn subject_decisions(
        &self,
        owner: i64,
        subject: &str,
        profile: &str,
        ids: &[(i64, String)],
    ) -> Result<Vec<crate::channel_subjects::CachedDecision>, StoreError> {
        let sql = crate::channel_subjects::decision_query(owner, subject, profile, ids);
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            rows.map(|row| {
                let (payload, last_used_ms) = row?;
                let mut decision: crate::channel_subjects::CachedDecision =
                    serde_json::from_str(&payload)
                        .map_err(|e| StoreError::Database(e.to_string()))?;
                decision.last_used_ms = last_used_ms;
                Ok(decision)
            })
            .collect()
        })
        .await
    }

    async fn list_library_channels(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        management: bool,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<LibraryChannel>, StoreError> {
        let after_id = after_id.unwrap_or_default().to_owned();
        let limit = limit.clamp(1, 100);
        self.with_read(move |conn| {
            let scope = if management && actor_is_admin {
                "c.id > ?2"
            } else {
                "c.id > ?2 AND (c.owner_user_id = ?1 OR (c.visibility = 'shared' AND c.enabled = 1))"
            };
            let sql = format!("{} ORDER BY c.id LIMIT ?3", channel_query(scope));
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(
                params![actor_user_id, after_id, limit],
                channel_from_row,
            )?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn get_library_channel(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
    ) -> Result<Option<LibraryChannel>, StoreError> {
        let channel_id = channel_id.to_owned();
        self.with_read(move |conn| {
            Ok(load_channel(conn, actor_user_id, &channel_id)?
                .filter(|channel| visible(channel, actor_user_id, actor_is_admin)))
        })
        .await
    }

    async fn create_library_channel(
        &self,
        actor_is_admin: bool,
        channel: &NewLibraryChannel,
    ) -> Result<ChannelMutation<LibraryChannel>, StoreError> {
        let channel = channel.clone();
        if channel.visibility == ChannelVisibility::Shared && !actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        let recipe = recipe_json(&channel.recipe)?;
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let replay: Option<(String, String)> = tx
                .query_row(
                    "SELECT channel_id, operation_hash FROM library_channel_requests \
                     WHERE user_id = ?1 AND request_id = ?2 AND expires_at_ms > ?3",
                    params![channel.owner_user_id, channel.request_id, channel.now_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((channel_id, operation_hash)) = replay {
                if operation_hash != channel.request_hash {
                    return Ok(ChannelMutation::RequestConflict);
                }
                let result =
                    load_channel(&tx, channel.owner_user_id, &channel_id)?.ok_or_else(|| {
                        StoreError::Database(
                            "channel request points to a missing channel".to_owned(),
                        )
                    })?;
                return Ok(ChannelMutation::Replay(result));
            }
            if !request_slot_available(&tx, channel.owner_user_id, channel.now_ms)? {
                return Ok(ChannelMutation::RequestLedgerFull);
            }
            let owned: i64 = tx.query_row(
                "SELECT COUNT(*) FROM library_channels WHERE owner_user_id = ?1",
                [channel.owner_user_id],
                |row| row.get(0),
            )?;
            let total: i64 = tx.query_row("SELECT COUNT(*) FROM library_channels", [], |row| {
                row.get(0)
            })?;
            if owned >= CHANNELS_PER_USER_MAX || total >= CHANNELS_SERVER_MAX {
                return Ok(ChannelMutation::LimitExceeded);
            }
            tx.execute(
                "INSERT INTO library_channels \
                 (id, owner_user_id, name, description, visibility, enabled, \
                  definition_revision, recipe_json, seed, build_state, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, 'queued', ?9, ?9)",
                params![
                    channel.id,
                    channel.owner_user_id,
                    channel.name,
                    channel.description,
                    channel.visibility.as_str(),
                    channel.enabled,
                    recipe,
                    channel.seed.as_slice(),
                    channel.now_ms,
                ],
            )?;
            tx.execute(
                "INSERT INTO library_channel_requests \
                 (user_id, request_id, operation_hash, channel_id, result_revision, \
                  created_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
                params![
                    channel.owner_user_id,
                    channel.request_id,
                    channel.request_hash,
                    channel.id,
                    channel.now_ms,
                    channel.now_ms + 86_400_000,
                ],
            )?;
            let result = load_channel(&tx, channel.owner_user_id, &channel.id)?
                .ok_or_else(|| StoreError::Database("created channel disappeared".to_owned()))?;
            tx.commit()?;
            Ok(ChannelMutation::Applied(result))
        })
        .await
    }

    async fn update_library_channel(
        &self,
        update: &LibraryChannelUpdate,
    ) -> Result<ChannelMutation<LibraryChannel>, StoreError> {
        let update = update.clone();
        if update.visibility == ChannelVisibility::Shared && !update.actor_is_admin {
            return Ok(ChannelMutation::Forbidden);
        }
        let recipe = recipe_json(&update.recipe)?;
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let current = load_channel(&tx, update.actor_user_id, &update.channel_id)?;
            let Some(current) = current else {
                return Ok(ChannelMutation::NotFound);
            };
            if current.owner_user_id != update.actor_user_id && !update.actor_is_admin {
                return Ok(ChannelMutation::Forbidden);
            }
            let replay: Option<(String, i64)> = tx
                .query_row(
                    "SELECT operation_hash, result_revision FROM library_channel_requests \
                     WHERE user_id = ?1 AND request_id = ?2 AND expires_at_ms > ?3",
                    params![update.actor_user_id, update.request_id, update.now_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((operation_hash, result_revision)) = replay {
                if operation_hash != update.request_hash {
                    return Ok(ChannelMutation::RequestConflict);
                }
                if current.revision == result_revision {
                    return Ok(ChannelMutation::Replay(current));
                }
                return Ok(ChannelMutation::Stale);
            }
            if !request_slot_available(&tx, update.actor_user_id, update.now_ms)? {
                return Ok(ChannelMutation::RequestLedgerFull);
            }
            if current.revision != update.expected_revision {
                return Ok(ChannelMutation::Stale);
            }
            let changed = tx.execute(
                "UPDATE library_channels SET name = ?1, description = ?2, \
                 visibility = ?3, enabled = ?4, definition_revision = definition_revision + 1, \
                 recipe_json = ?5, seed = ?6, build_state = 'queued', \
                 build_error_code = NULL, build_error_message = NULL, updated_at_ms = ?7 \
                 WHERE id = ?8 AND definition_revision = ?9 \
                   AND (owner_user_id = ?10 OR ?11)",
                params![
                    update.name,
                    update.description,
                    update.visibility.as_str(),
                    update.enabled,
                    recipe,
                    update.seed.as_slice(),
                    update.now_ms,
                    update.channel_id,
                    update.expected_revision,
                    update.actor_user_id,
                    update.actor_is_admin,
                ],
            )?;
            if changed != 1 {
                return Ok(ChannelMutation::Stale);
            }
            tx.execute(
                "INSERT INTO library_channel_requests \
                 (user_id, request_id, operation_hash, channel_id, result_revision, \
                  created_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    update.actor_user_id,
                    update.request_id,
                    update.request_hash,
                    update.channel_id,
                    update.expected_revision + 1,
                    update.now_ms,
                    update.now_ms + 86_400_000,
                ],
            )?;
            let result = load_channel(&tx, update.actor_user_id, &update.channel_id)?
                .ok_or_else(|| StoreError::Database("updated channel disappeared".to_owned()))?;
            if result.recipe.subject.is_some() {
                let mut job = crate::channel_subjects::SubjectJob::new(
                    result.owner_user_id,
                    result.recipe.clone(),
                    result.seed,
                    Some((result.id.clone(), result.revision)),
                    update.now_ms,
                );
                job.activate_next_programme = update.subject_next_programme;
                for sql in crate::channel_subjects::JobWrite::Enqueue(job).sql() {
                    tx.execute(&sql, [])?;
                }
            }
            tx.commit()?;
            Ok(ChannelMutation::Applied(result))
        })
        .await
    }

    async fn delete_library_channel(
        &self,
        deletion: &LibraryChannelDelete,
    ) -> Result<ChannelMutation<()>, StoreError> {
        let deletion = deletion.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let replay: Option<(String, String)> = tx
                .query_row(
                    "SELECT channel_id, operation_hash FROM library_channel_requests \
                     WHERE user_id = ?1 AND request_id = ?2 AND expires_at_ms > ?3",
                    params![deletion.actor_user_id, deletion.request_id, deletion.now_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((channel_id, operation_hash)) = replay {
                if channel_id != deletion.channel_id || operation_hash != deletion.request_hash {
                    return Ok(ChannelMutation::RequestConflict);
                }
                return Ok(ChannelMutation::Replay(()));
            }
            if !request_slot_available(&tx, deletion.actor_user_id, deletion.now_ms)? {
                return Ok(ChannelMutation::RequestLedgerFull);
            }
            let current = load_channel(&tx, deletion.actor_user_id, &deletion.channel_id)?;
            let Some(current) = current else {
                return Ok(ChannelMutation::NotFound);
            };
            if current.owner_user_id != deletion.actor_user_id && !deletion.actor_is_admin {
                return Ok(ChannelMutation::Forbidden);
            }
            if current.revision != deletion.expected_revision {
                return Ok(ChannelMutation::Stale);
            }
            let changed = tx.execute(
                "DELETE FROM library_channels WHERE id = ?1 AND definition_revision = ?2 \
                 AND (owner_user_id = ?3 OR ?4)",
                params![
                    deletion.channel_id,
                    deletion.expected_revision,
                    deletion.actor_user_id,
                    deletion.actor_is_admin
                ],
            )?;
            if changed != 1 {
                return Ok(ChannelMutation::Stale);
            }
            tx.execute(
                "INSERT INTO library_channel_requests \
                 (user_id, request_id, operation_hash, channel_id, result_revision, \
                  created_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
                params![
                    deletion.actor_user_id,
                    deletion.request_id,
                    deletion.request_hash,
                    deletion.channel_id,
                    deletion.now_ms,
                    deletion.now_ms + 86_400_000,
                ],
            )?;
            tx.commit()?;
            Ok(ChannelMutation::Applied(()))
        })
        .await
    }

    async fn set_library_channel_favourite(
        &self,
        user_id: i64,
        channel_id: &str,
        favourite: bool,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let channel_id = channel_id.to_owned();
        self.with_conn(move |conn| {
            let visible: i64 = conn.query_row(
                "SELECT COUNT(*) FROM library_channels WHERE id = ?1 \
                 AND (owner_user_id = ?2 OR (visibility = 'shared' AND enabled = 1))",
                params![channel_id, user_id],
                |row| row.get(0),
            )?;
            if visible != 1 {
                return Ok(false);
            }
            if favourite {
                conn.execute(
                    "INSERT INTO library_channel_favourites (user_id, channel_id, created_at_ms) \
                     VALUES (?1, ?2, ?3) ON CONFLICT(user_id, channel_id) DO NOTHING",
                    params![user_id, channel_id, now_ms],
                )?;
            } else {
                conn.execute(
                    "DELETE FROM library_channel_favourites WHERE user_id = ?1 AND channel_id = ?2",
                    params![user_id, channel_id],
                )?;
            }
            Ok(true)
        })
        .await
    }

    async fn library_channel_catalog_snapshot(
        &self,
        limit: i64,
    ) -> Result<Vec<ChannelCandidate>, StoreError> {
        let limit = limit.clamp(1, 100_001);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT i.id, f.id, f.size, f.mtime, f.duration_ms, i.library_id, i.kind, \
                        i.title, COALESCE(i.overview, ''), COALESCE(sh.overview, ''), \
                        i.genres, COALESCE((SELECT json_group_array(value) FROM (SELECT value FROM json_each(i.tags) UNION ALL SELECT value FROM media_classifications mc,json_each(mc.terms) WHERE mc.item_id=i.id AND mc.source_json=json_object('id',i.id,'kind',i.kind,'title',i.title,'overview',COALESCE(i.overview,''),'year',i.year,'tmdb_id',i.tmdb_id,'genres',json(i.genres),'tags',json(i.tags)) UNION ALL SELECT 'classification:excluded:' || value FROM media_classifications mc,json_each(mc.overrides,'$.exclude') WHERE mc.item_id=i.id AND mc.source_json=json_object('id',i.id,'kind',i.kind,'title',i.title,'overview',COALESCE(i.overview,''),'year',i.year,'tmdb_id',i.tmdb_id,'genres',json(i.genres),'tags',json(i.tags)))),i.tags), \
                        COALESCE(CAST(substr(i.air_date, 1, 4) AS INTEGER), i.year, sh.year), \
                        sh.id, sh.title, s.season_number, i.episode_number, \
                        CASE WHEN s.season_number = 0 THEN 1 ELSE 0 END, \
                        COALESCE(sh.genres, '[]'), COALESCE(sh.tags, '[]') \
                 FROM items i \
                 LEFT JOIN items s ON i.kind = 'episode' AND s.id = i.parent_id AND s.kind = 'season' \
                 LEFT JOIN items sh ON i.kind = 'episode' AND sh.id = s.parent_id AND sh.kind = 'show' \
                 JOIN files f ON f.item_id = i.id \
                    AND f.probe_json IS NOT NULL AND f.video_codec IS NOT NULL \
                    AND f.duration_ms > 0 AND f.duration_ms <= 86400000 \
                 WHERE i.kind IN ('movie','episode') \
                 ORDER BY i.id, f.id LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| {
                let kind_raw: String = row.get(6)?;
                let kind = match kind_raw.as_str() {
                    "movie" => ChannelItemKind::Movie,
                    "episode" => ChannelItemKind::Episode,
                    _ => return Err(conversion_err(6, format!("unsupported channel kind `{kind_raw}`"))),
                };
                let show_genres = parse_string_list(18, row.get(18)?)?;
                let mut genres = parse_string_list(10, row.get(10)?)?;
                genres.extend(show_genres.clone());
                genres.sort_by_key(|value| value.to_lowercase());
                genres.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
                let show_tags = parse_string_list(19, row.get(19)?)?;
                let mut tags = parse_string_list(11, row.get(11)?)?;
                tags.extend(show_tags.clone());
                tags.sort_by_key(|value| value.to_lowercase());
                tags.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
                Ok(ChannelCandidate {
                    item_id: row.get(0)?,
                    file_id: row.get(1)?,
                    file_size: row.get(2)?,
                    file_mtime: row.get(3)?,
                    duration_ms: row.get(4)?,
                    library_id: row.get(5)?,
                    kind,
                    title: row.get(7)?,
                    overview: row.get(8)?,
                    show_overview: row.get(9)?,
                    genres,
                    tags,
                    show_genres,
                    show_tags,
                    year: row.get(12)?,
                    show_id: row.get(13)?,
                    show_title: row.get(14)?,
                    season_number: row.get(15)?,
                    episode_number: row.get(16)?,
                    special: row.get::<_, i64>(17)? != 0,
                    explicitly_included: false,
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn list_library_channel_refresh_candidates(
        &self,
        after_id: Option<&str>,
        limit: i64,
        now_ms: i64,
    ) -> Result<Vec<LibraryChannel>, StoreError> {
        let after_id = after_id.unwrap_or_default().to_owned();
        self.with_read(move |conn| {
            let sql = format!(
                "SELECT {CHANNEL_COLS}, 0 AS favourite FROM library_channels c \
                 WHERE c.id > ?1 AND (c.build_state = 'queued' OR (c.enabled = 1 \
                   AND COALESCE(json_extract(c.recipe_json, '$.auto_refresh'), 1) = 1 \
                   AND (COALESCE(c.last_auto_build_ms, c.build_last_attempt_ms) IS NULL \
                     OR COALESCE(c.last_auto_build_ms, c.build_last_attempt_ms) <= ?2) \
                 )) ORDER BY c.id LIMIT ?3"
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    after_id,
                    now_ms.saturating_sub(15 * 60 * 1_000),
                    limit.clamp(1, 200)
                ],
                channel_from_row,
            )?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn prune_library_channel_state(
        &self,
        now_ms: i64,
        limit: i64,
    ) -> Result<i64, StoreError> {
        let limit = limit.clamp(1, CHANNEL_PRUNE_BATCH_MAX);
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let requests = tx.execute(
                "DELETE FROM library_channel_requests WHERE rowid IN (SELECT rowid \
                 FROM library_channel_requests WHERE expires_at_ms <= ?1 ORDER BY expires_at_ms LIMIT ?2)",
                params![now_ms, limit],
            )? as i64;
            let remaining = limit.saturating_sub(requests);
            let abandoned = if remaining > 0 {
                tx.execute(
                    "DELETE FROM library_channel_generations WHERE id IN (SELECT id \
                     FROM library_channel_generations WHERE state = 'building' \
                       AND build_claim_expires_ms <= ?1 ORDER BY created_at_ms LIMIT ?2)",
                    params![now_ms.saturating_sub(CHANNEL_ABANDONED_BUILD_RETENTION_MS), remaining],
                )? as i64
            } else {
                0
            };
            let remaining = remaining.saturating_sub(abandoned);
            let superseded = if remaining > 0 {
                tx.execute(
                    "DELETE FROM library_channel_generations WHERE id IN (SELECT g.id \
                     FROM library_channel_generations g WHERE g.state = 'ready' \
                       AND g.created_at_ms <= ?1 \
                       AND NOT EXISTS(SELECT 1 FROM library_channels c \
                         WHERE c.active_generation_id = g.id OR c.pending_generation_id = g.id) \
                     ORDER BY g.created_at_ms LIMIT ?2)",
                    params![now_ms.saturating_sub(CHANNEL_SUPERSEDED_RETENTION_MS), remaining],
                )? as i64
            } else {
                0
            };
            tx.commit()?;
            Ok(requests + abandoned + superseded)
        })
        .await
    }

    async fn claim_library_channel_build(
        &self,
        claim: &LibraryChannelBuildClaim,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let claim = claim.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let channel: Option<i64> = tx
                .query_row(
                    "SELECT definition_revision FROM library_channels WHERE id = ?1",
                    [claim.channel_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(revision) = channel else {
                return Ok(ChannelBuildMutation::NotFound);
            };
            if revision != claim.expected_revision {
                return Ok(ChannelBuildMutation::Stale);
            }
            let abandoned: i64 = tx.query_row(
                "SELECT COUNT(*) FROM library_channel_generations \
                 WHERE channel_id = ?1 AND state = 'building' AND build_claim_expires_ms > ?2",
                params![claim.channel_id, claim.now_ms],
                |row| row.get(0),
            )?;
            if abandoned > 0 {
                return Ok(ChannelBuildMutation::Busy);
            }
            tx.execute(
                "INSERT INTO library_channel_generations \
                 (id, channel_id, definition_revision, algorithm_version, content_digest, state, \
                  build_claim_id, build_claim_expires_ms, created_at_ms) \
                 VALUES (?1, ?2, ?3, 1, ?4, 'building', ?5, ?6, ?7)",
                params![
                    claim.generation_id,
                    claim.channel_id,
                    claim.expected_revision,
                    claim.content_digest,
                    claim.claim_id,
                    claim.expires_at_ms,
                    claim.now_ms,
                ],
            )?;
            tx.execute(
                "UPDATE library_channels SET build_state = 'building', \
                 build_error_code = NULL, build_error_message = NULL, \
                 build_last_attempt_ms = ?1 WHERE id = ?2 AND definition_revision = ?3",
                params![claim.now_ms, claim.channel_id, claim.expected_revision],
            )?;
            tx.commit()?;
            Ok(ChannelBuildMutation::Applied)
        })
        .await
    }

    async fn renew_library_channel_build(
        &self,
        channel_id: &str,
        generation_id: &str,
        claim_id: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let channel_id = channel_id.to_owned();
        let generation_id = generation_id.to_owned();
        let claim_id = claim_id.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE library_channel_generations SET build_claim_expires_ms = ?1 \
                 WHERE id = ?2 AND channel_id = ?3 AND state = 'building' \
                   AND build_claim_id = ?4 AND build_claim_expires_ms > ?5",
                params![expires_at_ms, generation_id, channel_id, claim_id, now_ms],
            )?;
            Ok(if changed == 1 {
                ChannelBuildMutation::Applied
            } else {
                ChannelBuildMutation::Stale
            })
        })
        .await
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
        let channel_id = channel_id.to_owned();
        let generation_id = generation_id.to_owned();
        let claim_id = claim_id.to_owned();
        let entries = entries.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let valid: i64 = tx.query_row(
                "SELECT COUNT(*) FROM library_channel_generations \
                 WHERE id = ?1 AND channel_id = ?2 AND state = 'building' \
                   AND build_claim_id = ?3 AND build_claim_expires_ms > ?4",
                params![generation_id, channel_id, claim_id, now_ms],
                |row| row.get(0),
            )?;
            if valid != 1 {
                return Ok(ChannelBuildMutation::Stale);
            }
            for entry in entries {
                tx.execute(
                    "INSERT INTO library_channel_entries \
                     (generation_id, ordinal, item_id, file_id, file_size, file_mtime, \
                      duration_ms, cumulative_start_ms, show_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        generation_id,
                        entry.ordinal,
                        entry.item_id,
                        entry.file_id,
                        entry.file_size,
                        entry.file_mtime,
                        entry.duration_ms,
                        entry.cumulative_start_ms,
                        entry.show_id,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(ChannelBuildMutation::Applied)
        })
        .await
    }

    async fn publish_library_channel_generation(
        &self,
        publication: &LibraryChannelPublication,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let publication = publication.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if let Some((job,claim))=&publication.subject_claim {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM library_channel_subject_jobs WHERE id=?1 AND claim_id=?2 AND channel_id=?3 AND channel_revision=?4 AND state='running' AND claim_expires_ms>?5)",params![job,claim,publication.channel_id,publication.expected_revision,publication.now_ms],|row|row.get(0))?;
                if !valid {return Ok(ChannelBuildMutation::Stale);}
            }
            tx.execute(
                "UPDATE library_channels SET active_generation_id = pending_generation_id, \
                 active_epoch_ms = pending_epoch_ms, pending_generation_id = NULL, \
                 pending_epoch_ms = NULL WHERE id = ?1 AND pending_generation_id IS NOT NULL \
                   AND pending_epoch_ms <= ?2",
                params![publication.channel_id, publication.now_ms],
            )?;
            let facts: Option<(i64, i64, i64, Option<String>)> = tx
                .query_row(
                    "SELECT g.definition_revision, COUNT(e.ordinal), \
                            COALESCE(MAX(e.cumulative_start_ms + e.duration_ms), 0), \
                            c.active_generation_id \
                     FROM library_channel_generations g \
                     JOIN library_channels c ON c.id = g.channel_id \
                     LEFT JOIN library_channel_entries e ON e.generation_id = g.id \
                     WHERE g.id = ?1 AND g.channel_id = ?2 AND g.state = 'building' \
                       AND g.build_claim_id = ?3 AND g.build_claim_expires_ms > ?4 \
                       AND g.content_digest = ?5 AND c.definition_revision = ?6 \
                     GROUP BY g.id",
                    params![
                        publication.generation_id,
                        publication.channel_id,
                        publication.claim_id,
                        publication.now_ms,
                        publication.content_digest,
                        publication.expected_revision,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let Some((_, count, duration, active)) = facts else {
                return Ok(ChannelBuildMutation::Stale);
            };
            if count != publication.entry_count
                || duration != publication.loop_duration_ms
                || count <= 0
                || duration <= 0
            {
                return Ok(ChannelBuildMutation::Invalid);
            }
            let mut statement = tx.prepare(
                "SELECT ordinal, item_id, file_id, file_size, file_mtime, duration_ms, \
                 cumulative_start_ms, show_id FROM library_channel_entries \
                 WHERE generation_id = ?1 ORDER BY ordinal",
            )?;
            let staged = statement
                .query_map([publication.generation_id.as_str()], |row| {
                    Ok(ChannelGenerationEntry {
                        ordinal: row.get(0)?, item_id: row.get(1)?, file_id: row.get(2)?,
                        file_size: row.get(3)?, file_mtime: row.get(4)?, duration_ms: row.get(5)?,
                        cumulative_start_ms: row.get(6)?, show_id: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let continuous = staged.iter().enumerate().scan(0_i64, |offset, (index, entry)| {
                let valid = entry.ordinal as usize == index && entry.cumulative_start_ms == *offset;
                *offset = offset.saturating_add(entry.duration_ms);
                Some(valid)
            }).all(|valid| valid);
            if !continuous || generation_content_digest(publication.seed, &staged) != publication.content_digest {
                return Ok(ChannelBuildMutation::Invalid);
            }
            tx.execute(
                "UPDATE library_channel_generations SET state = 'ready', entry_count = ?1, \
                 loop_duration_ms = ?2, build_claim_id = NULL, build_claim_expires_ms = NULL \
                 WHERE id = ?3 AND build_claim_id = ?4",
                params![
                    publication.entry_count,
                    publication.loop_duration_ms,
                    publication.generation_id,
                    publication.claim_id,
                ],
            )?;
            let pending = publication.pending && active.is_some();
            let changed = if pending {
                tx.execute(
                    "UPDATE library_channels SET pending_generation_id = ?1, pending_epoch_ms = ?2, \
                     build_state = 'ready', build_candidate_count = ?6, build_entry_count = ?6, \
                     build_last_success_ms = ?3, last_auto_build_ms = CASE WHEN ?7 THEN ?3 ELSE last_auto_build_ms END, \
                     updated_at_ms = ?3 WHERE id = ?4 AND definition_revision = ?5",
                    params![
                        publication.generation_id,
                        publication.activation_epoch_ms,
                        publication.now_ms,
                        publication.channel_id,
                        publication.expected_revision,
                        publication.entry_count,
                        publication.automatic,
                    ],
                )?
            } else {
                tx.execute(
                    "UPDATE library_channels SET active_generation_id = ?1, active_epoch_ms = ?2, \
                     pending_generation_id = NULL, pending_epoch_ms = NULL, build_state = 'ready', \
                     build_candidate_count = ?6, build_entry_count = ?6, build_last_success_ms = ?3, \
                     last_auto_build_ms = CASE WHEN ?7 THEN ?3 ELSE last_auto_build_ms END, updated_at_ms = ?3 \
                     WHERE id = ?4 AND definition_revision = ?5",
                    params![
                        publication.generation_id,
                        publication.activation_epoch_ms,
                        publication.now_ms,
                        publication.channel_id,
                        publication.expected_revision,
                        publication.entry_count,
                        publication.automatic,
                    ],
                )?
            };
            if changed != 1 {
                return Ok(ChannelBuildMutation::Stale);
            }
            tx.commit()?;
            Ok(ChannelBuildMutation::Applied)
        })
        .await
    }

    async fn fail_library_channel_build(
        &self,
        failure: &LibraryChannelBuildFailure,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let failure = failure.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if let (Some(generation_id), Some(claim_id)) = (
                failure.generation_id.as_deref(),
                failure.claim_id.as_deref(),
            ) {
                tx.execute(
                    "UPDATE library_channel_generations SET build_claim_expires_ms = ?1 \
                     WHERE id = ?2 AND channel_id = ?3 AND build_claim_id = ?4",
                    params![failure.now_ms, generation_id, failure.channel_id, claim_id],
                )?;
            }
            let changed = tx.execute(
                "UPDATE library_channels SET build_state = 'failed', build_error_code = ?1, \
                 build_error_message = ?2, build_candidate_count = ?3, build_last_attempt_ms = ?4, \
                 updated_at_ms = ?4 WHERE id = ?5 AND definition_revision = ?6",
                params![
                    failure.error_code,
                    failure.error_message,
                    failure.candidate_count,
                    failure.now_ms,
                    failure.channel_id,
                    failure.expected_revision
                ],
            )?;
            tx.commit()?;
            Ok(if changed == 1 {
                ChannelBuildMutation::Applied
            } else {
                ChannelBuildMutation::Stale
            })
        })
        .await
    }

    async fn complete_library_channel_build_without_publication(
        &self,
        channel_id: &str,
        expected_revision: i64,
        candidate_count: i64,
        automatic: bool,
        now_ms: i64,
    ) -> Result<ChannelBuildMutation, StoreError> {
        let channel_id = channel_id.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE library_channels SET build_state = 'ready', build_error_code = NULL, \
                 build_error_message = NULL, build_candidate_count = ?1, \
                 build_entry_count = COALESCE((SELECT entry_count FROM library_channel_generations \
                   WHERE id = COALESCE(pending_generation_id, active_generation_id)), 0), \
                 build_last_attempt_ms = ?2, build_last_success_ms = ?2, \
                 last_auto_build_ms = CASE WHEN ?3 THEN ?2 ELSE last_auto_build_ms END, updated_at_ms = ?2 \
                 WHERE id = ?4 AND definition_revision = ?5",
                params![candidate_count, now_ms, automatic, channel_id, expected_revision],
            )?;
            Ok(if changed == 1 { ChannelBuildMutation::Applied } else { ChannelBuildMutation::Stale })
        }).await
    }

    async fn read_library_channel_generation(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
        generation_id: &str,
    ) -> Result<Option<LibraryChannelGeneration>, StoreError> {
        let channel_id = channel_id.to_owned();
        let generation_id = generation_id.to_owned();
        self.with_read(move |conn| {
            let Some(channel) = load_channel(conn, actor_user_id, &channel_id)? else {
                return Ok(None);
            };
            if !visible(&channel, actor_user_id, actor_is_admin) {
                return Ok(None);
            }
            let header = conn
                .query_row(
                    "SELECT id, channel_id, definition_revision, algorithm_version, content_digest, \
                     state, entry_count, loop_duration_ms, build_claim_id, \
                     build_claim_expires_ms, created_at_ms \
                     FROM library_channel_generations WHERE id = ?1 AND channel_id = ?2",
                    params![generation_id, channel_id],
                    |row| {
                        Ok(LibraryChannelGeneration {
                            id: row.get(0)?,
                            channel_id: row.get(1)?,
                            definition_revision: row.get(2)?,
                            algorithm_version: row.get(3)?,
                            content_digest: row.get(4)?,
                            state: row.get(5)?,
                            entry_count: row.get(6)?,
                            loop_duration_ms: row.get(7)?,
                            build_claim_id: row.get(8)?,
                            build_claim_expires_ms: row.get(9)?,
                            created_at_ms: row.get(10)?,
                            entries: Vec::new(),
                        })
                    },
                )
                .optional()?;
            let Some(mut generation) = header else {
                return Ok(None);
            };
            let mut statement = conn.prepare(
                "SELECT ordinal, item_id, file_id, file_size, file_mtime, duration_ms, \
                 cumulative_start_ms, show_id FROM library_channel_entries \
                 WHERE generation_id = ?1 ORDER BY ordinal",
            )?;
            generation.entries = statement
                .query_map([generation_id], |row| {
                    Ok(ChannelGenerationEntry {
                        ordinal: row.get(0)?,
                        item_id: row.get(1)?,
                        file_id: row.get(2)?,
                        file_size: row.get(3)?,
                        file_mtime: row.get(4)?,
                        duration_ms: row.get(5)?,
                        cumulative_start_ms: row.get(6)?,
                        show_id: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(generation))
        })
        .await
    }
}
