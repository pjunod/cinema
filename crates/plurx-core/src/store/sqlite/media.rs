//! Items (movie/show/season/episode), media files, search.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use super::{
    file_from_row, item_cols, item_from_row, SqliteStore, FILE_COLS, ITEM_COLS, ITEM_COL_COUNT,
};
use crate::domain::{
    sort_title_for, ArtworkAttempt, BookMetadataPatch, HomePreviewPage, Item, ItemEdit, ItemKind,
    ItemPage, ItemSort, MediaFile, MediaShape, MetadataPatch, NewItem, ProbeResult, RecentItem,
};
use crate::error::StoreError;
use crate::mediafacts::{FactsRow, MediaFacts};
use crate::store::{
    directory_matches_movie_path, directory_matches_show_path, directory_path_bounds,
    item_sort_order_by, normalized_directory, ArtworkInventoryItem, ArtworkRepairFence,
    IdentityRepairBlocker, IdentityRepairFile, IdentityRepairItem, IdentityRepairSnapshot,
    IdentityRepairWatch, MediaStore, MissingFieldOrder, MissingVideoCodecTag, ReconcileOutcome,
    RootFingerprintStatus, RootRemovalOutcome, SeriesHintOutcome, IDENTITY_REPAIR_EPISODES_MAX,
    IDENTITY_REPAIR_FILES_MAX, IDENTITY_REPAIR_SEASONS_MAX, IDENTITY_REPAIR_SHOWS_MAX,
    IDENTITY_REPAIR_SHOWS_MIN, IDENTITY_REPAIR_WATCHES_MAX, TOP_LEVEL_ITEM_PREDICATE,
};

pub(super) fn identity_repair_snapshot(
    conn: &Connection,
    library_id: i64,
    show_ids: &[i64],
) -> Result<IdentityRepairSnapshot, StoreError> {
    const REPAIR_ITEM_COLS: &str = "id, library_id, kind, parent_id, title, sort_title, year, overview, tmdb_id, imdb_id, season_number, episode_number, air_date, runtime_ms, poster_path, backdrop_path, added_at, updated_at, recorded_at, tags, nfo_seeded_at, metadata_at, artwork_attempted_at, artwork_error, genres, author, book_work_id, book_edition_id, book_metadata_source";
    const REPAIR_FILE_COLS: &str = "id, item_id, path, size, mtime, duration_ms, container, video_codec, video_profile, width, height, bit_depth, hdr, bitrate, audio_streams, subtitle_streams, probe_json, scanned_at, hdr_format, audio_offset_ms, dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present, video_codec_tag, field_order, max_cll, max_fall, mastering_max_luminance, luminance_source, downloaded_subtitles";
    if !(IDENTITY_REPAIR_SHOWS_MIN..=IDENTITY_REPAIR_SHOWS_MAX).contains(&show_ids.len())
        || show_ids.iter().any(|id| *id <= 0)
    {
        return Err(StoreError::Task(
            "invalid identity repair show IDs".to_owned(),
        ));
    }
    let mut sorted_ids = show_ids.to_vec();
    sorted_ids.sort_unstable();
    if sorted_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(StoreError::Task(
            "duplicate identity repair show ID".to_owned(),
        ));
    }
    let ids_json = serde_json::to_string(&sorted_ids)
        .map_err(|error| StoreError::Task(format!("encode repair show IDs: {error}")))?;
    let library = conn
        .query_row(
            "SELECT kind, paths FROM libraries WHERE id = ?1",
            params![library_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::Task("identity repair library not found".to_owned()))?;

    let mut item_statement = conn.prepare(&format!(
        "WITH requested(id) AS (SELECT value FROM json_each(?1)),
              selected(id) AS (
                SELECT id FROM requested
                UNION SELECT i.id FROM items i JOIN requested r ON i.parent_id = r.id
                UNION SELECT e.id FROM items e JOIN items s ON s.id = e.parent_id
                     JOIN requested r ON s.parent_id = r.id)
         SELECT id, library_id, kind, parent_id, tmdb_id, season_number, episode_number,
                added_at, json_array({REPAIR_ITEM_COLS})
           FROM items WHERE id IN (SELECT id FROM selected) ORDER BY id"
    ))?;
    let items = item_statement
        .query_map(params![ids_json], |row| {
            Ok(IdentityRepairItem {
                id: row.get(0)?,
                library_id: row.get(1)?,
                kind: row.get(2)?,
                parent_id: row.get(3)?,
                tmdb_id: row.get(4)?,
                season_number: row.get(5)?,
                episode_number: row.get(6)?,
                added_at: row.get(7)?,
                row: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let season_count = items.iter().filter(|item| item.kind == "season").count();
    let episode_count = items.iter().filter(|item| item.kind == "episode").count();
    if season_count > IDENTITY_REPAIR_SEASONS_MAX || episode_count > IDENTITY_REPAIR_EPISODES_MAX {
        return Err(StoreError::Task("repair_too_large".to_owned()));
    }
    let item_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
    let item_ids_json = serde_json::to_string(&item_ids)
        .map_err(|error| StoreError::Task(format!("encode repair item IDs: {error}")))?;

    let mut file_statement = conn.prepare(&format!(
        "SELECT id, item_id, path, size, mtime, json_array({REPAIR_FILE_COLS})
           FROM files WHERE item_id IN (SELECT value FROM json_each(?1)) ORDER BY id"
    ))?;
    let files = file_statement
        .query_map(params![item_ids_json], |row| {
            Ok(IdentityRepairFile {
                id: row.get(0)?,
                item_id: row.get(1)?,
                path: row.get(2)?,
                size: row.get(3)?,
                mtime: row.get(4)?,
                row: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if files.len() > IDENTITY_REPAIR_FILES_MAX {
        return Err(StoreError::Task("repair_too_large".to_owned()));
    }

    let mut watch_statement = conn.prepare(
        "SELECT user_id, item_id, position_ms, duration_ms, watched, updated_at
           FROM watch_state WHERE item_id IN (SELECT value FROM json_each(?1))
          ORDER BY item_id, user_id",
    )?;
    let watches = watch_statement
        .query_map(params![item_ids_json], |row| {
            Ok(IdentityRepairWatch {
                user_id: row.get(0)?,
                item_id: row.get(1)?,
                position_ms: row.get(2)?,
                duration_ms: row.get(3)?,
                watched: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if watches.len() > IDENTITY_REPAIR_WATCHES_MAX {
        return Err(StoreError::Task("repair_too_large".to_owned()));
    }

    let mut directories = files
        .iter()
        .filter_map(|file| {
            crate::scan::parse::parse_episode(std::path::Path::new(&file.path))
                .or_else(|_| {
                    crate::scan::parse::parse_anime_episode(std::path::Path::new(&file.path))
                })
                .ok()
                .and_then(|parsed| parsed.source_directory)
                .map(|path| path.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    directories.sort();
    directories.dedup();
    let mut directory_owner_ids = Vec::new();
    for directory in &directories {
        let (lower, upper) = directory_path_bounds(directory)?;
        let mut statement = conn.prepare(
            "SELECT f.path, show.id
               FROM files f INDEXED BY sqlite_autoindex_files_1
               JOIN items episode ON episode.id = f.item_id AND episode.kind = 'episode'
               JOIN items season ON season.id = episode.parent_id AND season.kind = 'season'
               JOIN items show ON show.id = season.parent_id AND show.kind = 'show'
              WHERE f.path >= ?1 AND f.path < ?2
                AND show.library_id = ?3 AND season.library_id = ?3 AND episode.library_id = ?3",
        )?;
        for row in statement.query_map(params![lower, upper, library_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (path, show_id) = row?;
            if directory_matches_show_path(&path, directory) {
                directory_owner_ids.push(show_id);
            }
        }
    }
    directory_owner_ids.sort_unstable();
    directory_owner_ids.dedup();

    let retiring_ids = crate::store::plan_identity_repair(IdentityRepairSnapshot {
        library_id,
        library_kind: library.0.clone(),
        library_paths: library.1.clone(),
        input_show_ids: sorted_ids.clone(),
        items: items.clone(),
        files: files.clone(),
        watches: watches.clone(),
        directory_owner_ids: directory_owner_ids.clone(),
        dependency_rows: Vec::new(),
        blockers: Vec::new(),
    })?
    .retired_item_ids;
    let dependency_ids_json = serde_json::to_string(&retiring_ids)
        .map_err(|error| StoreError::Task(format!("encode retiring item IDs: {error}")))?;

    let mut blockers = Vec::new();
    let reading_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reading_state WHERE item_id IN (SELECT value FROM json_each(?1))",
        params![dependency_ids_json],
        |row| row.get(0),
    )?;
    if reading_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "reading_state".to_owned(),
            detail: format!("{reading_count} reading-state rows reference selected items"),
        });
    }
    let reconcile_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM scan_reconcile_items
          WHERE library_id = ?1 AND item_id IN (SELECT value FROM json_each(?2))",
        params![library_id, dependency_ids_json],
        |row| row.get(0),
    )?;
    if reconcile_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "scan_reconcile_items".to_owned(),
            detail: format!("{reconcile_count} reconciliation rows reference selected items"),
        });
    }
    let override_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_classifications
          WHERE item_id IN (SELECT value FROM json_each(?1))
            AND (json_array_length(overrides, '$.include') > 0
              OR json_array_length(overrides, '$.exclude') > 0)",
        params![dependency_ids_json],
        |row| row.get(0),
    )?;
    if override_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "classification_overrides".to_owned(),
            detail: format!("{override_count} selected items have manual classification overrides"),
        });
    }
    let artwork_table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
                        WHERE type='table' AND name='cluster_artwork_repairs')",
        [],
        |row| row.get(0),
    )?;
    let artwork_count: i64 = if artwork_table_exists {
        conn.query_row(
            "SELECT COUNT(*) FROM cluster_artwork_repairs
              WHERE item_id IN (SELECT value FROM json_each(?1))",
            params![dependency_ids_json],
            |row| row.get(0),
        )?
    } else {
        0
    };
    if artwork_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "artwork_repair".to_owned(),
            detail: format!("{artwork_count} artwork repairs reference selected items"),
        });
    }
    let channel_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_channel_entries
          WHERE item_id IN (SELECT value FROM json_each(?1))
             OR show_id IN (SELECT value FROM json_each(?1))",
        params![dependency_ids_json],
        |row| row.get(0),
    )?;
    if channel_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "library_channel_entries".to_owned(),
            detail: format!("{channel_count} channel entries reference selected items"),
        });
    }
    let recipe_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_channels c
          WHERE EXISTS (
            SELECT 1 FROM json_each(?1) ids
             WHERE EXISTS (SELECT 1 FROM json_each(c.recipe_json, '$.include_item_ids') WHERE value = ids.value)
                OR EXISTS (SELECT 1 FROM json_each(c.recipe_json, '$.exclude_item_ids') WHERE value = ids.value)
                OR EXISTS (SELECT 1 FROM json_each(c.recipe_json, '$.include_show_ids') WHERE value = ids.value)
                OR EXISTS (SELECT 1 FROM json_each(c.recipe_json, '$.exclude_show_ids') WHERE value = ids.value))",
        params![dependency_ids_json],
        |row| row.get(0),
    )?;
    if recipe_count != 0 {
        blockers.push(IdentityRepairBlocker {
            code: "library_channel_recipe".to_owned(),
            detail: format!("{recipe_count} channel recipes reference selected items"),
        });
    }
    for (code, sql, detail) in [
        (
            "active_media_session",
            "SELECT COUNT(*) FROM media_sessions s WHERE s.state IN ('starting','active') AND (EXISTS (SELECT 1 FROM json_tree(s.recipe_json) j JOIN json_each(?1) ids ON j.type='integer' AND j.value=ids.value WHERE j.key IN ('item_id','show_id')) OR EXISTS (SELECT 1 FROM json_tree(s.response_json) j JOIN json_each(?1) ids ON j.type='integer' AND j.value=ids.value WHERE j.key IN ('item_id','show_id')))",
            "active media sessions reference selected items",
        ),
        (
            "channel_session_recipe",
            "SELECT COUNT(*) FROM library_channel_session_recipes r WHERE EXISTS (SELECT 1 FROM json_tree(r.recipe_json) j JOIN json_each(?1) ids ON j.type='integer' AND j.value=ids.value WHERE j.key IN ('item_id','show_id'))",
            "channel session recipes reference selected items",
        ),
        (
            "watched_outbox",
            "SELECT COUNT(*) FROM watched_outbox o WHERE EXISTS (SELECT 1 FROM json_tree(o.payload) j JOIN json_each(?1) ids ON j.type='integer' AND j.value=ids.value WHERE j.key IN ('item_id','show_id'))",
            "watched outbox payloads reference selected items",
        ),
        (
            "dvr_recording",
            "SELECT COUNT(*) FROM dvr_recordings WHERE item_id IN (SELECT value FROM json_each(?1))",
            "DVR recordings reference selected items",
        ),
    ] {
        let count: i64 = conn.query_row(sql, params![dependency_ids_json], |row| row.get(0))?;
        if count != 0 {
            blockers.push(IdentityRepairBlocker {
                code: code.to_owned(),
                detail: format!("{count} {detail}"),
            });
        }
    }

    Ok(IdentityRepairSnapshot {
        library_id,
        library_kind: library.0,
        library_paths: library.1,
        input_show_ids: sorted_ids,
        items,
        files,
        watches,
        directory_owner_ids,
        dependency_rows: Vec::new(),
        blockers,
    })
}

pub(super) fn apply_series_tmdb_hint(
    conn: &rusqlite::Connection,
    library_id: i64,
    show_id: i64,
    tmdb_id: i64,
) -> Result<SeriesHintOutcome, StoreError> {
    if tmdb_id <= 0 {
        return Err(StoreError::Task(
            "series TMDB hint must be a positive integer".to_owned(),
        ));
    }
    let changed = conn.execute(
        "UPDATE items SET tmdb_id = ?1, updated_at = unixepoch()
         WHERE id = ?2 AND library_id = ?3 AND kind = 'show' AND tmdb_id IS NULL",
        params![tmdb_id, show_id, library_id],
    )?;
    if changed == 1 {
        return Ok(SeriesHintOutcome::Applied);
    }
    let current = conn
        .query_row(
            "SELECT tmdb_id FROM items
             WHERE id = ?1 AND library_id = ?2 AND kind = 'show'",
            params![show_id, library_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?;
    Ok(match current {
        Some(Some(current_tmdb_id)) if current_tmdb_id == tmdb_id => {
            SeriesHintOutcome::AlreadyEqual
        }
        Some(Some(current_tmdb_id)) => SeriesHintOutcome::Conflict { current_tmdb_id },
        Some(None) => {
            return Err(StoreError::Database(
                "series TMDB hint matched an unset show but changed no row".to_owned(),
            ))
        }
        None => SeriesHintOutcome::MissingOrWrongKind,
    })
}

/// Build an FTS5 MATCH expression from free text: quoted tokens, prefix
/// matching on the last one. Returns `None` for queries with no tokens.
fn fts_query(input: &str) -> Option<String> {
    crate::metadata::classification::fts_query(input)
}

fn find_by(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Option<Item>> {
    conn.query_row(sql, params, |row| item_from_row(row, 0))
        .optional()
}

/// An ` AND <column> IN (…)` fragment for an optional id narrowing.
///
/// `None` in gives an empty string out, so an unnarrowed query is the
/// original SQL character for character — the whole-library path must not
/// change shape because a targeted path wanted a filter. `Some(&[])` gives
/// `None` out, meaning "the caller asked for zero items": the caller returns
/// early rather than running a query that would match everything.
///
/// Inline rather than bound, like `child_counts`: these are our own row ids,
/// i64s that came out of this database, and a bound IN-list needs a
/// statement per arity.
fn id_filter(column: &str, only: Option<&[i64]>) -> Option<String> {
    match only {
        None => Some(String::new()),
        Some([]) => None,
        Some(ids) => {
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            Some(format!(" AND {column} IN ({list})"))
        }
    }
}

/// The count and page statements `list_top_items_in_genre` runs for the
/// `ORDER BY` `order`, in that order.
///
/// Genres are a JSON array (migration v13), so membership is a `json_each`
/// scan rather than an index probe. Written as "no filter asked, OR the array
/// contains it" in ONE clause so the count and the page cannot drift: a total
/// computed without the filter and a page computed with it is a grid that
/// paginates into empty screens.
///
/// NOCASE because the value arrives from a URL a human or a client typed, and
/// "science fiction" meaning nothing while "Science Fiction" works is not a
/// distinction anybody asked for. ASCII-only folding, which is all TMDB's
/// genre vocabulary needs.
///
/// Both statements carry `TOP_LEVEL_ITEM_PREDICATE` as written, which is what
/// lets SQLite read them from the partial `idx_items_top_level_title`
/// (K-05 M5): a partial index serves only a query whose `WHERE` contains the
/// index's own `WHERE` term.
fn library_page_statements(order: &str) -> (String, String) {
    const GENRE_COUNT: &str = "(?2 IS NULL OR EXISTS ( \
         SELECT 1 FROM json_each(items.genres) WHERE value = ?2 COLLATE NOCASE))";
    const GENRE_PAGE: &str = "(?4 IS NULL OR EXISTS ( \
         SELECT 1 FROM json_each(items.genres) WHERE value = ?4 COLLATE NOCASE))";
    // Keep the count and page predicates structurally identical while giving
    // each statement a gap-free binding sequence. The census rejects either
    // clause if a future edit breaks that rule.
    debug_assert_eq!(
        GENRE_COUNT.replace("?2", "?"),
        GENRE_PAGE.replace("?4", "?")
    );
    (
        format!(
            "SELECT COUNT(*) FROM items WHERE library_id = ?1 AND \
             {TOP_LEVEL_ITEM_PREDICATE} AND {GENRE_COUNT}"
        ),
        format!(
            "SELECT {ITEM_COLS} FROM items
             WHERE library_id = ?1 AND {TOP_LEVEL_ITEM_PREDICATE} AND {GENRE_PAGE}
             ORDER BY {order} LIMIT ?3 OFFSET ?2"
        ),
    )
}

#[async_trait]
impl MediaStore for SqliteStore {
    async fn identity_repair_snapshot(
        &self,
        library_id: i64,
        show_ids: &[i64],
    ) -> Result<IdentityRepairSnapshot, StoreError> {
        let show_ids = show_ids.to_vec();
        self.with_conn(move |conn| identity_repair_snapshot(conn, library_id, &show_ids))
            .await
    }

    async fn item_by_external_id(
        &self,
        kind: ItemKind,
        tmdb_id: Option<i64>,
        imdb_id: Option<&str>,
    ) -> Result<Option<Item>, StoreError> {
        // Neither id means nothing to match on. Falling through would compare
        // NULL against NULL and return the first row of that kind — a wrong
        // answer delivered confidently, which is the failure this whole
        // ids-only rule exists to prevent.
        let imdb_id = imdb_id.filter(|s| !s.is_empty()).map(str::to_owned);
        if tmdb_id.is_none() && imdb_id.is_none() {
            return Ok(None);
        }
        let kind = kind.as_str().to_owned();
        self.with_conn(move |conn| {
            // Each arm is guarded by its own `IS NOT NULL`, so an id we do not
            // have can never match a row whose id is also NULL. TMDB wins ties
            // because it is what plurx stores for everything it enriched; the
            // IMDb arm is the fallback for items adopted from an NFO. Each arm
            // is one partial-index search (K-05 M5; `sql_source` says why the
            // statement is spelled as a union).
            let sql = super::super::sql_source::item_by_external_id(ITEM_COLS).sqlite();
            super::trace_statement("item_by_external_id", &sql);
            Ok(find_by(conn, &sql, params![kind, tmdb_id, imdb_id])?)
        })
        .await
    }

    async fn find_movie(
        &self,
        library_id: i64,
        title: &str,
        year: Option<i32>,
    ) -> Result<Option<Item>, StoreError> {
        let title = title.to_owned();
        self.with_conn(move |conn| {
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE library_id = ?1 AND kind = 'movie'
                       AND title = ?2 COLLATE NOCASE AND year IS ?3"
                ),
                params![library_id, title, year],
            )?)
        })
        .await
    }

    async fn find_movies_by_directory(
        &self,
        library_id: i64,
        directory: &str,
    ) -> Result<Vec<Item>, StoreError> {
        let directory = normalized_directory(directory)?.to_owned();
        let (lower, upper) = directory_path_bounds(&directory)?;
        self.with_conn(move |conn| {
            let item_columns = item_cols("movie");
            let mut statement = conn.prepare(&format!(
                "SELECT f.path, {item_columns}
                 FROM files f INDEXED BY sqlite_autoindex_files_1
                 JOIN items movie ON movie.id = f.item_id AND movie.kind = 'movie'
                 WHERE f.path >= ?1 AND f.path < ?2
                   AND movie.library_id = ?3"
            ))?;
            let mut candidates = HashMap::<i64, Item>::new();
            for row in statement.query_map(params![lower, upper, library_id], |row| {
                Ok((row.get::<_, String>(0)?, item_from_row(row, 1)?))
            })? {
                let (path, item) = row?;
                if directory_matches_movie_path(&path, &directory) {
                    candidates.entry(item.id).or_insert(item);
                }
            }
            let mut candidates = candidates.into_values().collect::<Vec<_>>();
            candidates.sort_by_key(|item| (item.added_at, item.id));
            Ok(candidates)
        })
        .await
    }

    async fn find_book(
        &self,
        library_id: i64,
        kind: ItemKind,
        title: &str,
        year: Option<i32>,
        identity_path: Option<&str>,
    ) -> Result<Option<Item>, StoreError> {
        debug_assert!(matches!(kind, ItemKind::Book | ItemKind::Audiobook));
        let kind = kind.as_str().to_owned();
        let title = title.to_owned();
        let identity_path = identity_path.map(str::to_owned);
        self.with_conn(move |conn| {
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE library_id = ?1 AND kind = ?2
                       AND ((?5 IS NOT NULL AND EXISTS (
                           SELECT 1 FROM files f WHERE f.item_id = items.id
                             AND (f.path = ?5 OR (
                               substr(f.path, 1, length(?5)) = ?5
                               AND substr(f.path, length(?5) + 1, 1) = '/'))))
                         OR (?5 IS NULL AND title = ?3 COLLATE NOCASE AND year IS ?4))"
                ),
                params![library_id, kind, title, year, identity_path],
            )?)
        })
        .await
    }

    async fn find_show(
        &self,
        library_id: i64,
        title: &str,
        year: Option<i32>,
    ) -> Result<Option<Item>, StoreError> {
        let title = title.to_owned();
        self.with_conn(move |conn| {
            // A show matches by title; year disambiguates only when both sides
            // have one (scanners often lack the year the second time around).
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE library_id = ?1 AND kind = 'show'
                       AND title = ?2 COLLATE NOCASE
                       AND (?3 IS NULL OR year IS NULL OR year = ?3)
                     ORDER BY (year = ?3) DESC"
                ),
                params![library_id, title, year],
            )?)
        })
        .await
    }

    async fn find_shows_by_directory(
        &self,
        library_id: i64,
        directory: &str,
        season_number: i32,
    ) -> Result<Vec<Item>, StoreError> {
        let directory = normalized_directory(directory)?.to_owned();
        let (lower, upper) = directory_path_bounds(&directory)?;
        self.with_conn(move |conn| {
            let item_columns = item_cols("show");
            let mut statement = conn.prepare(&format!(
                "SELECT f.path,
                        EXISTS(SELECT 1 FROM items owned
                               WHERE owned.parent_id = show.id
                                 AND owned.kind = 'season'
                                 AND owned.library_id = ?3
                                 AND owned.season_number = ?4),
                        {item_columns}
                 FROM files f INDEXED BY sqlite_autoindex_files_1
                 JOIN items episode ON episode.id = f.item_id AND episode.kind = 'episode'
                 JOIN items season ON season.id = episode.parent_id AND season.kind = 'season'
                 JOIN items show ON show.id = season.parent_id AND show.kind = 'show'
                 WHERE f.path >= ?1 AND f.path < ?2
                   AND episode.library_id = ?3
                   AND season.library_id = ?3
                   AND show.library_id = ?3"
            ))?;
            let mut candidates = HashMap::<i64, (Item, bool)>::new();
            for row in
                statement.query_map(params![lower, upper, library_id, season_number], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        item_from_row(row, 2)?,
                    ))
                })?
            {
                let (path, owns_season, item) = row?;
                if directory_matches_show_path(&path, &directory) {
                    candidates
                        .entry(item.id)
                        .and_modify(|(_, owns)| *owns |= owns_season)
                        .or_insert((item, owns_season));
                }
            }
            let mut candidates = candidates.into_values().collect::<Vec<_>>();
            candidates.sort_by_key(|(item, owns)| (!*owns, item.added_at, item.id));
            Ok(candidates.into_iter().map(|(item, _)| item).collect())
        })
        .await
    }

    async fn find_season(
        &self,
        show_id: i64,
        season_number: i32,
    ) -> Result<Option<Item>, StoreError> {
        self.with_conn(move |conn| {
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE parent_id = ?1 AND kind = 'season' AND season_number = ?2"
                ),
                params![show_id, season_number],
            )?)
        })
        .await
    }

    async fn find_episode(
        &self,
        season_id: i64,
        episode_number: i32,
    ) -> Result<Option<Item>, StoreError> {
        self.with_conn(move |conn| {
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE parent_id = ?1 AND kind = 'episode' AND episode_number = ?2"
                ),
                params![season_id, episode_number],
            )?)
        })
        .await
    }

    async fn find_child_item(
        &self,
        library_id: i64,
        parent_id: Option<i64>,
        kind: ItemKind,
        title: &str,
    ) -> Result<Option<Item>, StoreError> {
        let title = title.to_owned();
        let kind = kind.as_str();
        self.with_conn(move |conn| {
            Ok(find_by(
                conn,
                &format!(
                    "SELECT {ITEM_COLS} FROM items
                     WHERE library_id = ?1 AND kind = ?3 AND title = ?4 COLLATE NOCASE
                       AND parent_id IS ?2"
                ),
                params![library_id, parent_id, kind, title],
            )?)
        })
        .await
    }

    async fn insert_item(&self, item: &NewItem) -> Result<i64, StoreError> {
        let item = item.clone();
        self.with_conn(move |conn| {
            // Folder titles are directory names, not work titles: "The Lake
            // House 2021" is a place. Stripping the article would file it
            // under L, so folders sort on the raw name.
            let sort_title = if item.kind == ItemKind::Folder {
                item.title.to_lowercase()
            } else {
                sort_title_for(&item.title)
            };
            let changed = conn.execute(
                if conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='item_identity_watermark'", [], |r| r.get::<_,i64>(0))? == 1 {
                    "INSERT INTO items(id,library_id,kind,parent_id,title,sort_title,year,season_number,episode_number) SELECT high_water+1,?1,?2,?3,?4,?5,?6,?7,?8 FROM item_identity_watermark WHERE singleton=1 AND importing=0 AND high_water<9223372036854775807"
                } else {
                    "INSERT INTO items(library_id,kind,parent_id,title,sort_title,year,season_number,episode_number) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)"
                },
                params![
                    item.library_id,
                    item.kind.as_str(),
                    item.parent_id,
                    item.title,
                    sort_title,
                    item.year,
                    item.season_number,
                    item.episode_number,
                ],
            )?;
            if changed != 1 { return Err(StoreError::Database("item identity allocation unavailable".into())); }
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    async fn get_item(&self, id: i64) -> Result<Option<Item>, StoreError> {
        // Same reasoning as `get_file`: hot, read-only, off the writer.
        self.with_read(move |conn| {
            Ok(find_by(
                conn,
                &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
                params![id],
            )?)
        })
        .await
    }

    async fn item_titles(&self, ids: &[i64]) -> Result<BTreeMap<i64, String>, StoreError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let ids = serde_json::to_string(ids)
            .map_err(|error| StoreError::Task(format!("encode item title ids: {error}")))?;
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, title FROM items \
                 WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![ids], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn get_item_children(&self, parent_id: i64) -> Result<Vec<Item>, StoreError> {
        self.with_read(move |conn| {
            // Shows order by season/episode; home folders want subfolders
            // first, then their media chronologically. The extra keys are
            // inert for movie/show libraries (recorded_at is NULL there).
            let mut stmt = conn.prepare(&format!(
                "SELECT {ITEM_COLS} FROM items WHERE parent_id = ?1
                 ORDER BY (kind = 'folder') DESC, season_number, episode_number,
                          (recorded_at IS NULL), recorded_at, sort_title"
            ))?;
            let items = stmt
                .query_map(params![parent_id], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn items_with_artwork(&self) -> Result<Vec<ArtworkInventoryItem>, StoreError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, poster_path, backdrop_path FROM items \
                 WHERE poster_path IS NOT NULL OR backdrop_path IS NOT NULL \
                 ORDER BY id",
            )?;
            let items = stmt
                .query_map([], |row| {
                    Ok(ArtworkInventoryItem {
                        id: row.get(0)?,
                        poster_path: row.get(1)?,
                        backdrop_path: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn items_with_artwork_page(
        &self,
        after_item_id: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError> {
        if after_item_id < 0 || !(1..=256).contains(&limit) {
            return Err(StoreError::Task(
                "invalid artwork inventory page".to_owned(),
            ));
        }
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {i} FROM items i
                 WHERE i.id > ?1
                   AND (i.poster_path IS NOT NULL OR i.backdrop_path IS NOT NULL)
                 ORDER BY i.id LIMIT ?2",
                i = item_cols("i")
            ))?;
            let items = stmt
                .query_map(params![after_item_id, limit], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn artwork_filename_is_referenced(&self, filename: &str) -> Result<bool, StoreError> {
        let filename = filename.to_owned();
        self.with_read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM items
                  WHERE poster_path = ?1 OR backdrop_path = ?1)",
                [filename],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .await
    }

    async fn referenced_artwork_filenames(
        &self,
        filenames: &[String],
    ) -> Result<Vec<String>, StoreError> {
        if filenames.len() > 256
            || filenames
                .iter()
                .any(|name| name.is_empty() || name.len() > 512)
        {
            return Err(StoreError::Task(
                "invalid artwork reference batch".to_owned(),
            ));
        }
        if filenames.is_empty() {
            return Ok(Vec::new());
        }
        let encoded = serde_json::to_string(filenames)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT poster_path AS filename FROM items
                  WHERE poster_path IN (SELECT value FROM json_each(?1))
                 UNION
                 SELECT backdrop_path AS filename FROM items
                  WHERE backdrop_path IN (SELECT value FROM json_each(?1))",
            )?;
            let referenced = stmt
                .query_map([encoded], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(referenced)
        })
        .await
    }

    async fn list_top_items_in_genre(
        &self,
        library_id: i64,
        sort: ItemSort,
        offset: i64,
        limit: i64,
        genre: Option<&str>,
    ) -> Result<ItemPage, StoreError> {
        let genre = genre.map(str::to_owned);
        self.with_read_txn(move |conn| {
            let (count_sql, page_sql) = library_page_statements(item_sort_order_by(sort));
            super::trace_statement("list_top_items_in_genre.count", &count_sql);
            let total: i64 =
                conn.query_row(&count_sql, params![library_id, genre], |row| row.get(0))?;
            super::trace_statement("list_top_items_in_genre.page", &page_sql);
            let mut stmt = conn.prepare(&page_sql)?;
            let items = stmt
                .query_map(params![library_id, offset, limit, genre], |row| {
                    item_from_row(row, 0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ItemPage { items, total })
        })
        .await
    }

    async fn home_preview_pages(
        &self,
        limit_per_library: i64,
    ) -> Result<Vec<HomePreviewPage>, StoreError> {
        let limit_per_library = limit_per_library.clamp(1, 24);
        self.with_read(move |conn| {
            let sql = format!(
                "WITH ranked AS (
                     SELECT id, library_id,
                            COUNT(*) OVER (PARTITION BY library_id) AS library_total,
                            ROW_NUMBER() OVER (
                                PARTITION BY library_id
                                ORDER BY added_at DESC, id DESC
                            ) AS preview_rank
                       FROM items
                      WHERE {TOP_LEVEL_ITEM_PREDICATE}
                 ), selected AS (
                     SELECT id, library_id, library_total, preview_rank
                       FROM ranked
                      WHERE preview_rank <= ?1
                 )
                 SELECT {}, selected.library_total
                   FROM selected
                   JOIN items i ON i.id = selected.id
                  ORDER BY selected.library_id, selected.preview_rank",
                item_cols("i")
            );
            super::trace_statement("home_preview_pages", &sql);
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map([limit_per_library], |row| {
                    Ok((item_from_row(row, 0)?, row.get::<_, i64>(ITEM_COL_COUNT)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let mut pages: Vec<HomePreviewPage> = Vec::new();
            for (item, total) in rows {
                match pages.last_mut() {
                    Some(page) if page.library_id == item.library_id => page.items.push(item),
                    _ => pages.push(HomePreviewPage {
                        library_id: item.library_id,
                        items: vec![item],
                        total,
                    }),
                }
            }
            Ok(pages)
        })
        .await
    }

    async fn recently_added(
        &self,
        library_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<RecentItem>, StoreError> {
        self.with_read(move |conn| {
            // One card per movie, per show (latest episode represents the
            // show), and — in home libraries — per video or folder. A scan can
            // insert several seasons inside the same second, so added_at alone
            // cannot choose the representative episode: break ties by season,
            // episode, then id so the card also gets the newest season poster.
            // Photos are excluded on purpose: a 2,000-photo import would
            // otherwise flood the home screen, and its videos and folders
            // still surface it. Read from a widening window of the newest
            // rows; `sql_source::recently_added` states why that is exact.
            let sql =
                super::super::sql_source::recently_added(&item_cols("i"), &item_cols("r")).sqlite();
            let mut stmt = conn.prepare(&sql)?;
            let mut window_offset =
                super::super::sql_source::recently_added_first_window_offset(limit);
            loop {
                // Once per pass: the query-plan protocol times every pass
                // the read actually ran.
                super::trace_statement("recently_added", &sql);
                let mut cut = false;
                let items = stmt
                    .query_map(params![library_id, window_offset, limit], |row| {
                        cut = row.get::<_, bool>(ITEM_COL_COUNT + 2)?;
                        Ok(RecentItem {
                            item: item_from_row(row, 0)?,
                            show_title: row.get(ITEM_COL_COUNT)?,
                            season_poster: row.get(ITEM_COL_COUNT + 1)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if !cut || i64::try_from(items.len()).unwrap_or(i64::MAX) >= limit {
                    return Ok(items);
                }
                window_offset =
                    super::super::sql_source::recently_added_wider_window_offset(window_offset);
            }
        })
        .await
    }

    async fn search_items(&self, query: &str, limit: i64) -> Result<Vec<RecentItem>, StoreError> {
        let Some(match_expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        self.with_read(move |conn| {
            // An `items_fts` hit is dropped only while the item has a *current*
            // classification index entry: membership of `classification_fts`,
            // looked up per hit by rowid (K-05 section 3.6). A rename deletes
            // that row (`classification_source_changed`) and keeps the
            // `media_classifications` row for regeneration, so the renamed
            // title is found through `items_fts`; testing the classification
            // table instead would hide it from both branches.
            let sql = format!(
                "WITH hits AS MATERIALIZED (SELECT rowid,rank AS score FROM items_fts WHERE items_fts MATCH ?1 AND NOT EXISTS (SELECT 1 FROM classification_fts c WHERE c.rowid = items_fts.rowid) UNION ALL SELECT rowid,rank AS score FROM classification_fts WHERE classification_fts MATCH ?1) SELECT {i}, show.title, season.poster_path
                 FROM (SELECT rowid,min(score) AS score FROM hits GROUP BY rowid) f
                 JOIN items i ON i.id = f.rowid
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE i.kind IN ('movie','show','episode','folder','video','photo','book','audiobook')
                 ORDER BY f.score, i.id LIMIT ?2",
                i = item_cols("i")
            );
            super::trace_statement("search_items", &sql);
            let mut stmt = conn.prepare(&sql)?;
            let items = stmt
                .query_map(params![match_expr, limit], |row| {
                    Ok(RecentItem {
                        item: item_from_row(row, 0)?,
                        show_title: row.get(ITEM_COL_COUNT)?,
                        season_poster: row.get(ITEM_COL_COUNT + 1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn apply_series_tmdb_hint(
        &self,
        library_id: i64,
        show_id: i64,
        tmdb_id: i64,
    ) -> Result<SeriesHintOutcome, StoreError> {
        self.with_conn(move |conn| apply_series_tmdb_hint(conn, library_id, show_id, tmdb_id))
            .await
    }

    async fn apply_metadata(&self, item_id: i64, patch: &MetadataPatch) -> Result<(), StoreError> {
        let patch = patch.clone();
        self.with_conn(move |conn| {
            let sort_title = patch.title.as_deref().map(sort_title_for);
            let tags = patch
                .tags
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::Database(e.to_string()))?;
            let genres = patch
                .genres
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::Database(e.to_string()))?;
            conn.execute(
                "UPDATE items SET
                     title = COALESCE(?2, title),
                     sort_title = COALESCE(?3, sort_title),
                     year = COALESCE(?4, year),
                     overview = COALESCE(?5, overview),
                     tmdb_id = COALESCE(?6, tmdb_id),
                     imdb_id = COALESCE(?7, imdb_id),
                     air_date = COALESCE(?8, air_date),
                     runtime_ms = COALESCE(?9, runtime_ms),
                     poster_path = COALESCE(?10, poster_path),
                     backdrop_path = COALESCE(?11, backdrop_path),
                     recorded_at = COALESCE(?12, recorded_at),
                     tags = COALESCE(?13, tags),
                     -- Add-or-replace like the rest, which is why the patch
                     -- sends NULL rather than '[]' when a provider named no
                     -- genres: an empty list here would let a Trakt backfill
                     -- or a caller-supplied id erase a good one.
                     genres = COALESCE(?17, genres),
                     -- Sticky: once a provider has answered, a later patch
                     -- that does not claim to be enrichment (caller-supplied
                     -- ids, a Trakt backfill) must not un-mark the item.
                     metadata_at = CASE WHEN ?14 = 1 THEN unixepoch() ELSE metadata_at END,
                     -- Only a patch that actually tried to fetch artwork
                     -- speaks for these two. Everything else — a Trakt
                     -- backfill, a caller-supplied id — leaves the record of
                     -- the last attempt intact, so the retry sweep keeps its
                     -- backoff instead of being reset to zero by traffic that
                     -- has nothing to do with images.
                     artwork_attempted_at =
                         CASE WHEN ?15 = 1 THEN unixepoch() ELSE artwork_attempted_at END,
                     artwork_error = CASE WHEN ?15 = 1 THEN ?16 ELSE artwork_error END,
                     updated_at = unixepoch()
                 WHERE id = ?1",
                params![
                    item_id,
                    patch.title,
                    sort_title,
                    patch.year,
                    patch.overview,
                    patch.tmdb_id,
                    patch.imdb_id,
                    patch.air_date,
                    patch.runtime_ms,
                    patch.poster_path,
                    patch.backdrop_path,
                    patch.recorded_at,
                    tags,
                    patch.enriched as i64,
                    patch.artwork.is_some() as i64,
                    match &patch.artwork {
                        Some(ArtworkAttempt::Failed(why)) => Some(why.as_str()),
                        _ => None,
                    },
                    genres,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn apply_metadata_if_artwork_repair_current(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, StoreError> {
        let patch = patch.clone();
        let fence = fence.clone();
        self.with_conn(move |conn| {
            let sort_title = patch.title.as_deref().map(sort_title_for);
            let tags = patch
                .tags
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| StoreError::Database(error.to_string()))?;
            let genres = patch
                .genres
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| StoreError::Database(error.to_string()))?;
            let changed = conn.execute(
                "UPDATE items SET
                     title = COALESCE(?2, title),
                     sort_title = COALESCE(?3, sort_title),
                     year = COALESCE(?4, year),
                     overview = COALESCE(?5, overview),
                     tmdb_id = COALESCE(?6, tmdb_id),
                     imdb_id = COALESCE(?7, imdb_id),
                     air_date = COALESCE(?8, air_date),
                     runtime_ms = COALESCE(?9, runtime_ms),
                     poster_path = COALESCE(?10, poster_path),
                     backdrop_path = COALESCE(?11, backdrop_path),
                     recorded_at = COALESCE(?12, recorded_at),
                     tags = COALESCE(?13, tags),
                     genres = COALESCE(?17, genres),
                     metadata_at = CASE WHEN ?14 = 1 THEN unixepoch() ELSE metadata_at END,
                     artwork_attempted_at =
                         CASE WHEN ?15 = 1 THEN unixepoch() ELSE artwork_attempted_at END,
                     artwork_error = CASE WHEN ?15 = 1 THEN ?16 ELSE artwork_error END,
                     updated_at = unixepoch()
                 WHERE id = ?1 AND ?18 = ?1 AND EXISTS (
                   SELECT 1 FROM cluster_artwork_repairs
                   WHERE item_id = ?18 AND owner_node_id = ?19 AND leader_term = ?20
                     AND generation = ?21)",
                params![
                    item_id,
                    patch.title,
                    sort_title,
                    patch.year,
                    patch.overview,
                    patch.tmdb_id,
                    patch.imdb_id,
                    patch.air_date,
                    patch.runtime_ms,
                    patch.poster_path,
                    patch.backdrop_path,
                    patch.recorded_at,
                    tags,
                    patch.enriched as i64,
                    patch.artwork.is_some() as i64,
                    match &patch.artwork {
                        Some(ArtworkAttempt::Failed(reason)) => Some(reason.as_str()),
                        _ => None,
                    },
                    genres,
                    fence.item_id,
                    fence.owner_node_id,
                    fence.leader_term,
                    fence.generation,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn apply_book_metadata(
        &self,
        item_id: i64,
        patch: &BookMetadataPatch,
    ) -> Result<(), StoreError> {
        let patch = patch.clone();
        self.with_conn(move |conn| {
            let sort_title = patch.title.as_deref().map(sort_title_for);
            let source = patch.source.as_str();
            conn.execute(
                "UPDATE items SET
                     title = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                              OR book_metadata_source = 'epub')
                                       THEN COALESCE(?2, title) ELSE title END,
                     sort_title = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                                   OR book_metadata_source = 'epub')
                                            THEN COALESCE(?3, sort_title) ELSE sort_title END,
                     author = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                               OR book_metadata_source = 'epub')
                                        THEN COALESCE(?4, author) ELSE author END,
                     book_work_id = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                                     OR book_metadata_source = 'epub')
                                              THEN COALESCE(?5, book_work_id) ELSE book_work_id END,
                     book_edition_id = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                                        OR book_metadata_source = 'epub')
                                                 THEN COALESCE(?6, book_edition_id) ELSE book_edition_id END,
                     poster_path = CASE WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                                    OR book_metadata_source = 'epub')
                                             THEN COALESCE(?7, poster_path) ELSE poster_path END,
                     book_metadata_source = CASE
                         WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                               OR book_metadata_source = 'epub') THEN ?8
                         ELSE book_metadata_source END,
                     updated_at = CASE
                         WHEN (?8 = 'curator' OR book_metadata_source IS NULL
                                               OR book_metadata_source = 'epub') THEN unixepoch()
                         ELSE updated_at END
                 WHERE id = ?1 AND kind IN ('book', 'audiobook')",
                params![
                    item_id,
                    patch.title,
                    sort_title,
                    patch.author,
                    patch.work_id,
                    patch.edition_id,
                    patch.poster_path,
                    source,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn apply_book_metadata_if_current(
        &self,
        expected: &Item,
        patch: &BookMetadataPatch,
        repair_fence: Option<&ArtworkRepairFence>,
    ) -> Result<bool, StoreError> {
        let patch = patch.clone();
        let expected = expected.clone();
        let repair_fence = repair_fence.cloned();
        self.with_conn(move |conn| {
            let sort_title = patch.title.as_deref().map(sort_title_for);
            let source = patch.source.as_str();
            let base_sql = "UPDATE items SET
                     title = COALESCE(?2, title),
                     sort_title = COALESCE(?3, sort_title),
                     author = COALESCE(?4, author),
                     book_work_id = COALESCE(?5, book_work_id),
                     book_edition_id = COALESCE(?6, book_edition_id),
                     poster_path = COALESCE(?7, poster_path),
                     book_metadata_source = ?8,
                     updated_at = unixepoch()
                 WHERE id = ?1 AND kind IN ('book', 'audiobook')
                   AND title = ?9
                   AND author IS ?10
                   AND book_work_id IS ?11
                   AND book_metadata_source IS ?12
                   AND book_edition_id IS ?13
                   AND poster_path IS ?14
                   AND (?15 IS NULL OR EXISTS (
                       SELECT 1 FROM settings WHERE key = ?15 AND value = ?16))";
            let (origin_key, origin_value) = patch
                .required_origin
                .as_ref()
                .map_or((None, None), |(key, value)| {
                    (Some(key.as_str()), Some(value.as_str()))
                });
            let changed = if let Some(fence) = repair_fence.as_ref() {
                conn.execute(
                    &format!(
                        "{base_sql} AND ?17 = ?1 AND EXISTS (
                           SELECT 1 FROM cluster_artwork_repairs
                           WHERE item_id = ?17 AND owner_node_id = ?18 AND leader_term = ?19
                             AND generation = ?20)"
                    ),
                    params![
                        expected.id,
                        patch.title,
                        sort_title,
                        patch.author,
                        patch.work_id,
                        patch.edition_id,
                        patch.poster_path,
                        source,
                        expected.title,
                        expected.author,
                        expected.book_work_id,
                        expected.book_metadata_source,
                        expected.book_edition_id,
                        expected.poster_path,
                        origin_key,
                        origin_value,
                        fence.item_id,
                        fence.owner_node_id,
                        fence.leader_term,
                        fence.generation,
                    ],
                )?
            } else {
                conn.execute(
                    base_sql,
                    params![
                        expected.id,
                        patch.title,
                        sort_title,
                        patch.author,
                        patch.work_id,
                        patch.edition_id,
                        patch.poster_path,
                        source,
                        expected.title,
                        expected.author,
                        expected.book_work_id,
                        expected.book_metadata_source,
                        expected.book_edition_id,
                        expected.poster_path,
                        origin_key,
                        origin_value,
                    ],
                )?
            };
            Ok(changed == 1)
        })
        .await
    }

    async fn book_items(
        &self,
        library_id: i64,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError> {
        let Some(narrow) = id_filter("id", only) else {
            return Ok(Vec::new());
        };
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {ITEM_COLS} FROM items
                 WHERE library_id = ?1 AND kind IN ('book', 'audiobook'){narrow}
                 ORDER BY id"
            ))?;
            let rows = statement
                .query_map(params![library_id], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn related_book_editions(
        &self,
        item_id: i64,
        work_id: &str,
    ) -> Result<Vec<Item>, StoreError> {
        let work_id = work_id.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {ITEM_COLS} FROM items
                 WHERE id != ?1 AND book_work_id = ?2
                   AND kind IN ('book', 'audiobook')
                 ORDER BY kind, id"
            ))?;
            let rows = statement
                .query_map(params![item_id, work_id], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn update_item_fields(
        &self,
        item_id: i64,
        edit: &ItemEdit,
    ) -> Result<Option<Item>, StoreError> {
        let edit = edit.clone();
        self.with_conn(move |conn| {
            // Distinct from apply_metadata: an edit must be able to CLEAR a
            // field, so each column is guarded by a "was it in the request?"
            // flag rather than by the value being non-NULL.
            let sort_title = edit.title.as_deref().map(sort_title_for);
            let tags = edit
                .tags
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::Database(e.to_string()))?;
            conn.execute(
                "UPDATE items SET
                     title      = CASE WHEN ?2 THEN ?3 ELSE title END,
                     sort_title = CASE WHEN ?2 THEN ?4 ELSE sort_title END,
                     overview   = CASE WHEN ?5 THEN ?6 ELSE overview END,
                     recorded_at = CASE WHEN ?7 THEN ?8 ELSE recorded_at END,
                     year       = CASE WHEN ?9 THEN ?10 ELSE year END,
                     tags       = CASE WHEN ?11 THEN ?12 ELSE tags END,
                     updated_at = unixepoch()
                 WHERE id = ?1",
                params![
                    item_id,
                    edit.title.is_some(),
                    edit.title,
                    sort_title,
                    edit.overview.is_some(),
                    edit.overview.flatten(),
                    edit.recorded_at.is_some(),
                    edit.recorded_at.flatten(),
                    edit.year.is_some(),
                    edit.year.flatten(),
                    tags.is_some(),
                    tags,
                ],
            )?;
            Ok(find_by(
                conn,
                &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
                params![item_id],
            )?)
        })
        .await
    }

    async fn set_nfo_seeded(&self, item_id: i64) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE items SET nfo_seeded_at = unixepoch() WHERE id = ?1",
                params![item_id],
            )?;
            Ok(())
        })
        .await
    }

    async fn items_needing_artwork(
        &self,
        library_id: i64,
        force: bool,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError> {
        let Some(narrow) = id_filter("id", only) else {
            return Ok(Vec::new());
        };
        self.with_conn(move |conn| {
            // Folders last: a folder inherits its first child's poster, so the
            // children have to be thumbed first.
            let mut stmt = conn.prepare(&format!(
                "SELECT {ITEM_COLS} FROM items
                 WHERE library_id = ?1 AND kind IN ('folder','video','photo')
                   AND (?2 = 1 OR poster_path IS NULL){narrow}
                 ORDER BY (kind = 'folder'), id"
            ))?;
            let items = stmt
                .query_map(params![library_id, force as i64], |row| {
                    item_from_row(row, 0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn items_needing_metadata(
        &self,
        library_id: Option<i64>,
        force: bool,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError> {
        let Some(narrow) = id_filter("i.id", only) else {
            return Ok(Vec::new());
        };
        self.with_conn(move |conn| {
            // Home libraries never see a provider: there is nothing to match
            // "Christmas 2019.mp4" against, and a false match would be worse
            // than nothing. Guarded here so no enrichment loop can reach them.
            let mut stmt = conn.prepare(&format!(
                "SELECT {i} FROM items i
                 JOIN libraries l ON l.id = i.library_id AND l.kind != 'home'
                 WHERE i.kind IN ('movie','show') AND (?2 = 1 OR i.metadata_at IS NULL)
                   AND (?1 IS NULL OR i.library_id = ?1){narrow}
                 ORDER BY i.id",
                i = item_cols("i")
            ))?;
            let items = stmt
                .query_map(params![library_id, force as i64], |row| {
                    item_from_row(row, 0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn items_missing_artwork(
        &self,
        library_id: Option<i64>,
        retry_after_secs: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.with_conn(move |conn| {
            // A movie/show is eligible only after its own provider pass. A TV
            // season/episode never receives `metadata_at` -- TMDB reaches it
            // through the show -- so its enriched show is the corresponding
            // gate. Without those two child cases the sweep can repair the
            // show poster but silently skips the season/episode cards that
            // motivated the job.
            //
            // Anime children are excluded: AniList enriches show artwork only,
            // so selecting their episodes would hand the sweep work it cannot
            // perform or stamp. Home libraries are likewise handled by their
            // local-art query rather than this provider retry path.
            let mut stmt = conn.prepare(&format!(
                "SELECT {i} FROM items i
                 JOIN libraries l ON l.id = i.library_id AND l.kind != 'home'
                 LEFT JOIN items parent ON parent.id = i.parent_id
                 LEFT JOIN items grandparent ON grandparent.id = parent.parent_id
                 WHERE (i.poster_path IS NULL
                        OR (i.kind IN ('movie','show') AND i.backdrop_path IS NULL))
                   AND (
                        (i.kind IN ('movie','show') AND i.metadata_at IS NOT NULL)
                        OR (i.kind = 'season' AND l.anime = 0
                            AND i.season_number IS NOT NULL
                            AND parent.kind = 'show' AND parent.metadata_at IS NOT NULL)
                        OR (i.kind = 'episode' AND l.anime = 0
                            AND i.season_number IS NOT NULL
                            AND parent.kind = 'season'
                            AND grandparent.kind = 'show'
                            AND grandparent.metadata_at IS NOT NULL)
                   )
                   AND (i.artwork_attempted_at IS NULL
                        OR i.artwork_attempted_at <= unixepoch() - ?2)
                   AND (?1 IS NULL OR i.library_id = ?1)
                 ORDER BY i.artwork_attempted_at, i.id
                 LIMIT ?3",
                i = item_cols("i")
            ))?;
            let items = stmt
                .query_map(params![library_id, retry_after_secs.max(0), limit], |row| {
                    item_from_row(row, 0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn items_missing_genres(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError> {
        self.with_conn(move |conn| {
            // Home libraries are excluded for the same reason
            // `items_needing_metadata` excludes them: there is no provider to
            // ask about "Christmas 2019.mp4", and their genre-shaped facts
            // already live in `tags` where the NFO put them.
            //
            // `metadata_at IS NOT NULL` — this is deliberately only the
            // *already enriched*. An item nobody has enriched yet is the
            // ordinary enrichment queue's work and will get genres there for
            // free; taking it here too would race that pass and spend a
            // second request on it.
            let mut stmt = conn.prepare(&format!(
                "SELECT {i} FROM items i
                 JOIN libraries l ON l.id = i.library_id AND l.kind != 'home'
                 WHERE i.kind IN ('movie','show')
                   AND i.genres = '[]'
                   AND i.metadata_at IS NOT NULL
                   AND i.id > ?1
                 ORDER BY i.id LIMIT ?2",
                i = item_cols("i")
            ))?;
            let items = stmt
                .query_map(params![after_id, limit.max(0)], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn episodes_for_show(&self, show_id: i64) -> Result<Vec<Item>, StoreError> {
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {e} FROM items e
                 JOIN items season ON e.parent_id = season.id
                 WHERE season.parent_id = ?1 AND e.kind = 'episode'
                 ORDER BY season.season_number, e.episode_number",
                e = item_cols("e")
            ))?;
            let items = stmt
                .query_map(params![show_id], |row| item_from_row(row, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
    }

    async fn get_file_by_path(&self, path: &str) -> Result<Option<MediaFile>, StoreError> {
        let path = path.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {FILE_COLS} FROM files WHERE path = ?1"),
                    params![path],
                    file_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn upsert_file(
        &self,
        item_id: i64,
        path: &str,
        size: i64,
        mtime: i64,
        probe: &ProbeResult,
    ) -> Result<i64, StoreError> {
        let path = path.to_owned();
        let probe = probe.clone();
        self.with_conn(move |conn| {
            let audio = serde_json::to_string(&probe.audio_streams)
                .map_err(|e| StoreError::Database(e.to_string()))?;
            let subs = serde_json::to_string(&probe.subtitle_streams)
                .map_err(|e| StoreError::Database(e.to_string()))?;
            let id: i64 = conn.query_row(
                "INSERT INTO files
                   (item_id, path, size, mtime, duration_ms, container, video_codec,
                    video_profile, width, height, bit_depth, hdr, bitrate,
                    audio_streams, subtitle_streams, probe_json, hdr_format, scanned_at,
                    dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present,
                    video_codec_tag, field_order, max_cll, max_fall,
                    mastering_max_luminance, luminance_source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                         ?14, ?15, ?16, ?17, unixepoch(), ?18, ?19, ?20, ?21, ?22, ?23,
                         ?24, ?25, ?26, ?27, ?28)
                 ON CONFLICT(path) DO UPDATE SET
                     item_id = excluded.item_id,
                     size = excluded.size,
                     mtime = excluded.mtime,
                     duration_ms = excluded.duration_ms,
                     container = excluded.container,
                     video_codec = excluded.video_codec,
                     video_profile = excluded.video_profile,
                     width = excluded.width,
                     height = excluded.height,
                     bit_depth = excluded.bit_depth,
                     hdr = excluded.hdr,
                     bitrate = excluded.bitrate,
                     audio_streams = excluded.audio_streams,
                     subtitle_streams = excluded.subtitle_streams,
                     probe_json = excluded.probe_json,
                     hdr_format = excluded.hdr_format,
                     dv_profile = excluded.dv_profile,
                     dv_level = excluded.dv_level,
                     dv_bl_compat_id = excluded.dv_bl_compat_id,
                     dv_el_present = excluded.dv_el_present,
                     dv_rpu_present = excluded.dv_rpu_present,
                     video_codec_tag = excluded.video_codec_tag,
                     field_order = excluded.field_order,
                     max_cll = excluded.max_cll,
                     max_fall = excluded.max_fall,
                     mastering_max_luminance = excluded.mastering_max_luminance,
                     luminance_source = excluded.luminance_source,
                     scanned_at = unixepoch()
                 RETURNING id",
                params![
                    item_id,
                    path,
                    size,
                    mtime,
                    probe.duration_ms,
                    probe.container,
                    probe.video_codec,
                    probe.video_profile,
                    probe.width,
                    probe.height,
                    probe.bit_depth,
                    probe.hdr,
                    probe.bitrate,
                    audio,
                    subs,
                    probe.raw_json,
                    probe.hdr_format,
                    probe.dolby_vision.profile,
                    probe.dolby_vision.level,
                    probe.dolby_vision.bl_compat_id,
                    probe.dolby_vision.el_present.map(i64::from),
                    probe.dolby_vision.rpu_present.map(i64::from),
                    probe.video_codec_tag,
                    probe.stored_field_order(),
                    probe.max_cll,
                    probe.max_fall,
                    probe.mastering_max_luminance,
                    probe.luminance_source,
                ],
                |row| row.get(0),
            )?;
            Ok(id)
        })
        .await
    }

    async fn media_shape(&self) -> Result<MediaShape, StoreError> {
        self.with_conn(|conn| {
            // "Probed" is `video_codec IS NOT NULL`, which is what every other
            // read in the app treats as "we have looked at this file". A row
            // that was never read has NULL in every column below, and letting
            // those into a denominator would report a library as 60% SDR when
            // 40% of it has simply never been opened.
            let probed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM files WHERE video_codec IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            let unprobed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM files WHERE video_codec IS NULL",
                [],
                |r| r.get(0),
            )?;

            let pairs = |sql: &str| -> Result<Vec<(String, i64)>, StoreError> {
                let mut st = conn.prepare(sql)?;
                let rows =
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                let mut out: Vec<(String, i64)> = rows.collect::<Result<_, _>>()?;
                // Biggest first: the shape of a library is the first two lines
                // of this, and alphabetical order buries them.
                out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                Ok(out)
            };

            let hdr = pairs(
                "SELECT COALESCE(NULLIF(hdr,''),'sdr'), COUNT(*) FROM files \
                 WHERE video_codec IS NOT NULL GROUP BY 1",
            )?;
            // 4K by height rather than by width: 3840x1600 scope-cropped
            // releases are 4K and would be missed by a width test, and nothing
            // that is not 4K is 1600 lines tall.
            let hdr_4k = pairs(
                "SELECT COALESCE(NULLIF(hdr,''),'sdr'), COUNT(*) FROM files \
                 WHERE video_codec IS NOT NULL AND height >= 1600 GROUP BY 1",
            )?;
            let codecs = pairs(
                "SELECT LOWER(video_codec), COUNT(*) FROM files \
                 WHERE video_codec IS NOT NULL GROUP BY 1",
            )?;

            let over_segmented_floor: i64 = conn.query_row(
                "SELECT COUNT(*) FROM files WHERE bitrate >= 40000000",
                [],
                |r| r.get(0),
            )?;
            let max_bitrate: Option<i64> =
                conn.query_row("SELECT MAX(bitrate) FROM files", [], |r| r.get(0))?;

            Ok(MediaShape {
                probed,
                unprobed,
                hdr,
                hdr_4k,
                codecs,
                over_segmented_floor,
                max_bitrate,
            })
        })
        .await
    }

    async fn add_downloaded_subtitle(
        &self,
        file_id: i64,
        track: &crate::domain::DownloadedSubtitle,
    ) -> Result<bool, StoreError> {
        let raw = crate::store::downloaded_subtitles::encode(track)?;
        let track = track.clone();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                crate::store::downloaded_subtitles::ADD_DOWNLOADED_SUBTITLE,
                params![
                    file_id,
                    track.source_size,
                    track.source_mtime,
                    raw,
                    track.provider_file_id
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn subtitle_candidate_file_ids(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<i64>, StoreError> {
        self.with_read(move |conn| {
            Ok(conn
                .prepare(crate::store::downloaded_subtitles::CANDIDATES)?
                .query_map(params![after_id, limit.clamp(1, 16)], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn playback_planning_snapshot(
        &self,
        file_id: i64,
        keys: &[&str],
    ) -> Result<Option<crate::store::PlaybackPlanningSnapshot>, StoreError> {
        self.note_settings_read(keys);
        let keys = crate::store::selected_settings_json(keys)?;
        self.with_read(move |conn| {
            conn.query_row(&format!("SELECT {FILE_COLS}, probe_json AS planning_probe, \
                (SELECT generation FROM playback_input_generation WHERE singleton = 1) AS planning_generation, \
                (SELECT json_group_object(key, value) FROM settings WHERE key IN (SELECT value FROM json_each(?2))) AS planning_settings, \
                (SELECT title FROM items WHERE items.id = files.item_id) AS planning_item_title \
                FROM files WHERE id = ?1"), params![file_id, keys], |row| {
                let encoded: String = row.get("planning_settings")?;
                let settings = serde_json::from_str(&encoded).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?;
                Ok(crate::store::PlaybackPlanningSnapshot { file: file_from_row(row)?,
                    probe_json: row.get("planning_probe")?, settings,
                    generation: row.get("planning_generation")?,
                    item_title: row.get("planning_item_title")? })
            }).optional().map_err(StoreError::from)
        }).await
    }

    async fn get_file(&self, id: i64) -> Result<Option<MediaFile>, StoreError> {
        // The per-request metadata lookup: session starts, decisions, VTT.
        // Read-only, so it takes a read connection instead of queuing behind
        // whatever the writer is doing (review §3.2).
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {FILE_COLS} FROM files WHERE id = ?1"),
                    params![id],
                    file_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn set_file_audio_offset(&self, file_id: i64, offset_ms: i64) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE files SET audio_offset_ms = ?2 WHERE id = ?1",
                params![file_id, offset_ms],
            )?;
            Ok(())
        })
        .await
    }

    async fn merge_file_probe_chapters(
        &self,
        file_id: i64,
        chapters_json: &str,
    ) -> Result<(), StoreError> {
        let chapters = chapters_json.to_owned();
        self.with_conn(move |conn| {
            // `json_set` rather than read-modify-write in Rust: the whole
            // document is rewritten either way, but doing it in SQL keeps the
            // update atomic against a concurrent scan writing the same row.
            // Guarded on NOT NULL because json_set(NULL, …) is NULL, which
            // would erase the probe-failed marker `probe_json IS NULL` — the
            // one fingerprint the repair job keys on.
            conn.execute(
                "UPDATE files SET probe_json = json_set(probe_json, '$.chapters', json(?2))
                 WHERE id = ?1 AND probe_json IS NOT NULL",
                params![file_id, chapters],
            )?;
            Ok(())
        })
        .await
    }

    async fn merge_file_probe_hevc_parameter_sets(
        &self,
        file_id: i64,
        size: i64,
        mtime: i64,
        census_json: &str,
    ) -> Result<bool, StoreError> {
        let census = census_json.to_owned();
        self.with_conn(move |conn| {
            // The same in-SQL graft as the chapters above, fenced to the
            // measured revision.
            Ok(conn.execute(
                "UPDATE files
                    SET probe_json = json_set(probe_json, '$.plurx_hevc_parameter_sets', json(?1))
                  WHERE id = ?2 AND size = ?3 AND mtime = ?4 AND probe_json IS NOT NULL",
                params![census, file_id, size, mtime],
            )? == 1)
        })
        .await
    }

    async fn merge_file_probe_content_encoding(
        &self,
        file_id: i64,
        size: i64,
        mtime: i64,
        report_json: &str,
    ) -> Result<bool, StoreError> {
        let report = report_json.to_owned();
        self.with_conn(move |conn| {
            // The same in-SQL graft as the chapters above, fenced to the
            // measured revision.
            Ok(conn.execute(
                "UPDATE files
                    SET probe_json = json_set(probe_json, '$.plurx_content_encoding', json(?1))
                  WHERE id = ?2 AND size = ?3 AND mtime = ?4 AND probe_json IS NOT NULL",
                params![report, file_id, size, mtime],
            )? == 1)
        })
        .await
    }

    async fn files_missing_dolby_vision(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, Option<String>)>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, probe_json, hdr_format FROM files
                  WHERE hdr = 'dolby_vision'
                    AND dv_profile IS NULL
                    AND probe_json IS NOT NULL
                    AND id > ?1
                  ORDER BY id
                  LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_id, limit.max(0)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn files_missing_video_codec_tag(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, path, size, mtime, probe_json FROM files
                  WHERE video_codec_tag IS NULL
                    AND probe_json IS NOT NULL
                    AND id > ?1
                  ORDER BY id
                  LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_id, limit.max(0)], |row| {
                Ok(MissingVideoCodecTag {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                    probe_json: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn set_file_video_codec_tag(
        &self,
        candidate: &MissingVideoCodecTag,
        video_codec_tag: &str,
    ) -> Result<bool, StoreError> {
        let candidate = candidate.clone();
        let video_codec_tag = video_codec_tag.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE files SET video_codec_tag = ?1
                  WHERE id = ?2 AND path = ?3 AND size = ?4 AND mtime = ?5
                    AND probe_json = ?6 AND video_codec_tag IS NULL",
                params![
                    video_codec_tag,
                    candidate.id,
                    candidate.path,
                    candidate.size,
                    candidate.mtime,
                    candidate.probe_json,
                ],
            )? == 1)
        })
        .await
    }

    async fn files_missing_field_order(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingFieldOrder>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, path, size, mtime, probe_json FROM files
                  WHERE field_order IS NULL
                    AND probe_json IS NOT NULL
                    AND id > ?1
                  ORDER BY id
                  LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_id, limit.max(0)], |row| {
                Ok(MissingFieldOrder {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                    probe_json: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn files_missing_luminance(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, path, size, mtime, probe_json FROM files
                  WHERE hdr IS NOT NULL AND luminance_source IS NULL
                    AND probe_json IS NOT NULL AND id > ?1
                  ORDER BY id LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_id, limit.max(0)], |row| {
                Ok(MissingVideoCodecTag {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                    probe_json: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn set_file_field_order(
        &self,
        candidate: &MissingFieldOrder,
        field_order: &str,
    ) -> Result<bool, StoreError> {
        let candidate = candidate.clone();
        let field_order = field_order.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE files SET field_order = ?1
                  WHERE id = ?2 AND path = ?3 AND size = ?4 AND mtime = ?5
                    AND probe_json = ?6 AND field_order IS NULL",
                params![
                    field_order,
                    candidate.id,
                    candidate.path,
                    candidate.size,
                    candidate.mtime,
                    candidate.probe_json,
                ],
            )? == 1)
        })
        .await
    }

    async fn set_file_luminance(
        &self,
        candidate: &MissingVideoCodecTag,
        max_cll: Option<i64>,
        max_fall: Option<i64>,
        mastering_max_luminance: Option<i64>,
        source: &str,
    ) -> Result<bool, StoreError> {
        let candidate = candidate.clone();
        let source = source.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE files SET max_cll = ?1, max_fall = ?2,
                                  mastering_max_luminance = ?3, luminance_source = ?4
                  WHERE id = ?5 AND path = ?6 AND size = ?7 AND mtime = ?8
                    AND probe_json = ?9 AND luminance_source IS NULL",
                params![
                    max_cll,
                    max_fall,
                    mastering_max_luminance,
                    source,
                    candidate.id,
                    candidate.path,
                    candidate.size,
                    candidate.mtime,
                    candidate.probe_json
                ],
            )? == 1)
        })
        .await
    }

    async fn files_without_luminance_facts(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError> {
        self.with_conn(move |conn| {
            let rows = conn
                .prepare(
                    "SELECT id, path, size, mtime, probe_json FROM files
                      WHERE hdr IS NOT NULL AND luminance_source = 'none'
                        AND probe_json IS NOT NULL AND id > ?1
                      ORDER BY id LIMIT ?2",
                )?
                .query_map(params![after_id, limit.max(0)], |row| {
                    Ok(MissingVideoCodecTag {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        size: row.get(2)?,
                        mtime: row.get(3)?,
                        probe_json: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn files_with_unsignalled_transfer(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError> {
        self.with_conn(move |conn| {
            let rows = conn
                .prepare(
                    "SELECT id, path, size, mtime, probe_json FROM files
                      WHERE hdr IS NULL AND probe_json IS NOT NULL AND id > ?1
                        AND (bit_depth >= 10 OR instr(probe_json, '\"bt2020\"') > 0)
                      ORDER BY id LIMIT ?2",
                )?
                .query_map(params![after_id, limit.max(0)], |row| {
                    Ok(MissingVideoCodecTag {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        size: row.get(2)?,
                        mtime: row.get(3)?,
                        probe_json: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn set_file_frame_dynamic_range(
        &self,
        candidate: &MissingVideoCodecTag,
        observed: &crate::scan::probe::FrameDynamicRange,
    ) -> Result<bool, StoreError> {
        let candidate = candidate.clone();
        let observed = observed.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE files SET hdr = ?1, hdr_format = ?2, max_cll = ?3, max_fall = ?4,
                                  mastering_max_luminance = ?5, luminance_source = ?6
                  WHERE id = ?7 AND path = ?8 AND size = ?9 AND mtime = ?10
                    AND probe_json = ?11 AND hdr IS NULL",
                params![
                    observed.hdr,
                    observed.hdr_format,
                    observed.max_cll,
                    observed.max_fall,
                    observed.mastering_max_luminance,
                    observed.luminance_source,
                    candidate.id,
                    candidate.path,
                    candidate.size,
                    candidate.mtime,
                    candidate.probe_json
                ],
            )? == 1)
        })
        .await
    }

    async fn set_file_frame_luminance(
        &self,
        candidate: &MissingVideoCodecTag,
        max_cll: Option<i64>,
        max_fall: Option<i64>,
        mastering_max_luminance: Option<i64>,
    ) -> Result<bool, StoreError> {
        let candidate = candidate.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE files SET max_cll = ?1, max_fall = ?2,
                                  mastering_max_luminance = ?3, luminance_source = 'frame'
                  WHERE id = ?4 AND path = ?5 AND size = ?6 AND mtime = ?7
                    AND probe_json = ?8 AND luminance_source = 'none'",
                params![
                    max_cll,
                    max_fall,
                    mastering_max_luminance,
                    candidate.id,
                    candidate.path,
                    candidate.size,
                    candidate.mtime,
                    candidate.probe_json
                ],
            )? == 1)
        })
        .await
    }

    async fn set_file_dolby_vision(
        &self,
        file_id: i64,
        facts: crate::domain::DolbyVisionFacts,
        hdr_format: Option<&str>,
    ) -> Result<(), StoreError> {
        let hdr_format = hdr_format.map(str::to_owned);
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE files SET dv_profile = ?2, dv_level = ?3, dv_bl_compat_id = ?4,
                                  dv_el_present = ?5, dv_rpu_present = ?6,
                                  hdr_format = COALESCE(?7, hdr_format)
                  WHERE id = ?1",
                params![
                    file_id,
                    facts.profile,
                    facts.level,
                    facts.bl_compat_id,
                    facts.el_present.map(i64::from),
                    facts.rpu_present.map(i64::from),
                    hdr_format,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_file_probe_json(&self, file_id: i64) -> Result<Option<String>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT probe_json FROM files WHERE id = ?1",
                    params![file_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }

    async fn get_file_probe_chapters_json(
        &self,
        file_id: i64,
    ) -> Result<Option<String>, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT CASE
                       WHEN probe_json IS NOT NULL AND json_valid(probe_json)
                        AND json_type(probe_json, '$.chapters') = 'array'
                       THEN json_extract(probe_json, '$.chapters') END
                       FROM files WHERE id = ?1",
                    params![file_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }

    async fn files_for_item(&self, item_id: i64) -> Result<Vec<MediaFile>, StoreError> {
        self.with_read(move |conn| {
            // Best version first: an item can have several source files (a 4K
            // and a 1080p rip of the same movie). Order by resolution, then
            // bitrate, so clients default to the highest quality; SQLite
            // sorts NULLs last under DESC.
            let sql = format!(
                "SELECT {FILE_COLS} FROM files WHERE item_id = ?1
                 ORDER BY height DESC, bitrate DESC, path"
            );
            super::trace_statement("files_for_item", &sql);
            let mut stmt = conn.prepare(&sql)?;
            let files = stmt
                .query_map(params![item_id], file_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(files)
        })
        .await
    }

    async fn files_missing_probe(
        &self,
        library_id: Option<i64>,
    ) -> Result<Vec<MediaFile>, StoreError> {
        self.with_conn(move |conn| {
            // Narrowed through a subquery rather than a join: `FILE_COLS` is
            // unqualified, and joining `items` would make `id` ambiguous.
            // Oldest scan first, so a capped retry pass works through the
            // backlog instead of re-trying the same few files forever.
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLS} FROM files
                 WHERE probe_json IS NULL
                   AND (?1 IS NULL
                        OR item_id IN (SELECT id FROM items WHERE library_id = ?1))
                 ORDER BY scanned_at ASC, id ASC"
            ))?;
            let files = stmt
                .query_map(params![library_id], file_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(files)
        })
        .await
    }

    async fn child_counts(&self, ids: &[i64]) -> Result<HashMap<i64, i64>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // ids are our own row ids (trusted i64s), so an inline IN-list is safe.
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT parent_id, COUNT(*) FROM items
                 WHERE parent_id IN ({list}) GROUP BY parent_id"
            ))?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().collect())
        })
        .await
    }

    async fn item_max_heights(&self, ids: &[i64]) -> Result<HashMap<i64, i64>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // ids come from our own item rows (trusted i64s), so an inline IN-list
        // is safe and avoids a variadic-params dance.
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT item_id, MAX(height) FROM files
                 WHERE height IS NOT NULL AND item_id IN ({list})
                 GROUP BY item_id"
            ))?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().collect())
        })
        .await
    }

    async fn item_media_facts(&self, ids: &[i64]) -> Result<HashMap<i64, MediaFacts>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // Same inline IN-list as `item_max_heights`, for the same reason: our
        // own row ids, and a bound list needs a statement per arity.
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.with_read(move |conn| {
            // ONE statement for the whole page. The window functions do both
            // halves of the aggregation rule documented on `MediaFacts`:
            // COUNT/SUM describe every file of the item, ROW_NUMBER picks the
            // single best file whose columns describe it. Ordering: height
            // first, so this can never disagree with the `resolution` badge's
            // MAX(height); then bitrate (a remux beats a re-encode at equal
            // height); then size and id purely so equal versions resolve the
            // same way on every render instead of flickering.
            //
            // NULL heights sort last via COALESCE rather than being filtered
            // out — an unprobed file is still a file, and dropping it here
            // would make `files`/`bytes` understate what is on the volume.
            let sql = format!(
                "WITH ranked AS (
                     SELECT item_id,
                            COUNT(*)  OVER (PARTITION BY item_id) AS n_files,
                            SUM(size) OVER (PARTITION BY item_id) AS total_bytes,
                            ROW_NUMBER() OVER (
                                PARTITION BY item_id
                                ORDER BY COALESCE(height, 0) DESC,
                                         COALESCE(bitrate, 0) DESC,
                                         size DESC,
                                         id ASC) AS pick,
                            container, video_codec, height, hdr, hdr_format,
                            audio_streams
                     FROM files
                     WHERE item_id IN ({list})
                 )
                 SELECT item_id, n_files, total_bytes, container, video_codec,
                        height, hdr, hdr_format, audio_streams
                 FROM ranked WHERE pick = 1"
            );
            super::trace_statement("item_media_facts", &sql);
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map([], |row| {
                    let audio_json: String = row.get(8)?;
                    Ok((
                        row.get::<_, i64>(0)?,
                        FactsRow {
                            files: row.get(1)?,
                            bytes: row.get(2)?,
                            container: row.get(3)?,
                            video_codec: row.get(4)?,
                            height: row.get(5)?,
                            hdr: row.get(6)?,
                            hdr_format: row.get(7)?,
                            // The one thing this SQL cannot aggregate:
                            // `audio_streams` is a JSON TEXT column, so the
                            // summary is a pass in Rust over rows already
                            // fetched — still one query, still no second trip.
                            //
                            // Tolerant on purpose, unlike `file_from_row`:
                            // this decorates a grid, so unreadable JSON in one
                            // row costs that row its audio badge, never the
                            // whole page its 200.
                            audio: serde_json::from_str(&audio_json).unwrap_or_default(),
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|(id, row)| (id, MediaFacts::from(row)))
                .collect())
        })
        .await
    }

    async fn library_file_paths(&self, library_id: i64) -> Result<Vec<(i64, PathBuf)>, StoreError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT f.id, f.path FROM files f
                 JOIN items i ON i.id = f.item_id
                 WHERE i.library_id = ?1",
            )?;
            let rows = stmt
                .query_map(params![library_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        PathBuf::from(row.get::<_, String>(1)?),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn ensure_library_root_fingerprint(
        &self,
        library_id: i64,
        configured_paths: &[PathBuf],
        fingerprint: &str,
        allow_establish: bool,
    ) -> Result<RootFingerprintStatus, StoreError> {
        let configured_paths = crate::store::library_paths_json(configured_paths)?;
        let fingerprint = fingerprint.to_owned();
        self.with_conn(move |conn| {
            super::publication::ensure_library_root_fingerprint(
                conn,
                library_id,
                &configured_paths,
                &fingerprint,
                allow_establish,
            )
        })
        .await
    }

    async fn reconcile_library(
        &self,
        library_id: i64,
        root_fingerprint: &str,
        gone_file_ids: &[i64],
        prune_limit: u64,
    ) -> Result<ReconcileOutcome, StoreError> {
        if let Some(refusal) = crate::store::reconcile_payload_refusal(gone_file_ids, prune_limit) {
            return Ok(refusal);
        }
        let root_fingerprint = root_fingerprint.to_owned();
        let ids = gone_file_ids.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let expected: Option<String> = tx
                .query_row(
                    "SELECT fingerprint FROM library_roots WHERE library_id = ?1",
                    params![library_id],
                    |row| row.get(0),
                )
                .optional()?;
            if expected.as_deref() != Some(root_fingerprint.as_str()) {
                return Ok(ReconcileOutcome::RefusedRoot {
                    expected: expected.unwrap_or_else(|| "<unregistered>".to_owned()),
                });
            }

            // `gone_file_ids` is the subset of `library_file_paths` absent
            // from the completed walk. Its cardinality is already the exact
            // requested prune count; rejoining every id through one enormous
            // IN list made reconciliation quadratic for large libraries.
            let requested = ids.len() as u64;
            if requested > prune_limit {
                return Ok(ReconcileOutcome::RefusedPrune {
                    requested,
                    limit: prune_limit,
                });
            }

            let mut deleted_files = 0_u64;
            {
                let mut delete = tx.prepare_cached(
                    "DELETE FROM files WHERE id = ?1 AND item_id IN \
                     (SELECT id FROM items WHERE library_id = ?2) \
                     AND NOT EXISTS (SELECT 1 FROM dv_conversions d \
                                     WHERE d.file_id = files.id \
                                       AND d.state IN ('running', 'verified'))",
                )?;
                for id in ids {
                    deleted_files += delete.execute(params![id, library_id])? as u64;
                }
            }
            let mut pruned_items = 0_u64;
            pruned_items += tx.execute(
                "DELETE FROM items WHERE library_id = ?1 \
                 AND kind IN ('movie','episode','video','photo','book','audiobook') \
                 AND id NOT IN (SELECT item_id FROM files)",
                params![library_id],
            )? as u64;
            pruned_items += tx.execute(
                "DELETE FROM items WHERE library_id = ?1 AND kind = 'season' \
                 AND id NOT IN (SELECT parent_id FROM items \
                                WHERE kind = 'episode' AND parent_id IS NOT NULL)",
                params![library_id],
            )? as u64;
            pruned_items += tx.execute(
                "DELETE FROM items WHERE library_id = ?1 AND kind = 'show' \
                 AND id NOT IN (SELECT parent_id FROM items \
                                WHERE kind = 'season' AND parent_id IS NOT NULL)",
                params![library_id],
            )? as u64;
            let empty_folders: i64 = tx.query_row(
                "WITH RECURSIVE descendants(root_id, id, kind) AS ( \
                     SELECT root.id, child.id, child.kind FROM items root \
                     LEFT JOIN items child ON child.parent_id = root.id \
                     WHERE root.library_id = ?1 AND root.kind = 'folder' \
                     UNION \
                     SELECT descendants.root_id, child.id, child.kind \
                     FROM descendants JOIN items child ON child.parent_id = descendants.id \
                 ) \
                 SELECT COUNT(*) FROM items WHERE library_id = ?1 AND kind = 'folder' \
                   AND NOT EXISTS (SELECT 1 FROM descendants \
                                   WHERE root_id = items.id AND kind != 'folder')",
                params![library_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "WITH RECURSIVE descendants(root_id, id, kind) AS ( \
                     SELECT root.id, child.id, child.kind FROM items root \
                     LEFT JOIN items child ON child.parent_id = root.id \
                     WHERE root.library_id = ?1 AND root.kind = 'folder' \
                     UNION \
                     SELECT descendants.root_id, child.id, child.kind \
                     FROM descendants JOIN items child ON child.parent_id = descendants.id \
                 ) \
                 DELETE FROM items WHERE library_id = ?1 AND kind = 'folder' \
                   AND NOT EXISTS (SELECT 1 FROM descendants \
                                   WHERE root_id = items.id AND kind != 'folder')",
                params![library_id],
            )?;
            pruned_items += empty_folders.max(0) as u64;
            tx.commit()?;
            Ok(ReconcileOutcome::Applied {
                deleted_files,
                pruned_items,
            })
        })
        .await
    }

    async fn remove_files_outside_library_roots(
        &self,
        library_id: i64,
        configured_paths: &[PathBuf],
        file_ids: &[i64],
    ) -> Result<RootRemovalOutcome, StoreError> {
        crate::store::root_removal_payload(file_ids)?;
        let configured_paths = crate::store::library_paths_json(configured_paths)?;
        let ids = file_ids.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let outcome =
                super::publication::remove_library_files(&tx, library_id, &configured_paths, &ids)?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
    }

    async fn reset_library_root_fingerprint(&self, library_id: i64) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM library_roots WHERE library_id = ?1",
                params![library_id],
            )? == 1)
        })
        .await
    }

    async fn rebuild_search_index(&self) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
            conn.execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", [])?;
            conn.execute("DELETE FROM classification_fts", [])?;
            conn.execute(&crate::store::classification::rebuild_sql(), [])?;
            Ok(count.max(0) as u64)
        })
        .await
    }

    async fn delete_files(&self, ids: &[i64]) -> Result<u64, StoreError> {
        let ids = ids.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut deleted = 0u64;
            {
                let mut stmt = tx.prepare("DELETE FROM files WHERE id = ?1")?;
                for id in &ids {
                    deleted += stmt.execute(params![id])? as u64;
                }
            }
            tx.commit()?;
            Ok(deleted)
        })
        .await
    }

    async fn prune_empty_items(&self, library_id: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let removed = prune_empty_items_in(&tx, library_id)?;
            tx.commit()?;
            Ok(removed)
        })
        .await
    }
}

/// Remove a library's items left without files, bottom-up, inside the
/// caller's transaction. Returns rows removed.
pub(super) fn prune_empty_items_in(tx: &Connection, library_id: i64) -> Result<u64, StoreError> {
    let mut removed = 0u64;
    // Bottom-up: file-less leaves, then empty seasons, then empty shows.
    removed += tx.execute(
        "DELETE FROM items
         WHERE library_id = ?1 AND kind IN ('movie','episode','video','photo','book','audiobook')
           AND id NOT IN (SELECT item_id FROM files)",
        params![library_id],
    )? as u64;
    removed += tx.execute(
        "DELETE FROM items WHERE library_id = ?1 AND kind = 'season'
         AND id NOT IN (SELECT parent_id FROM items
                        WHERE kind = 'episode' AND parent_id IS NOT NULL)",
        params![library_id],
    )? as u64;
    removed += tx.execute(
        "DELETE FROM items WHERE library_id = ?1 AND kind = 'show'
         AND id NOT IN (SELECT parent_id FROM items
                        WHERE kind = 'season' AND parent_id IS NOT NULL)",
        params![library_id],
    )? as u64;
    // Folders nest arbitrarily deep, so one pass only strips the
    // innermost layer: delete a whole subtree of files and the empty
    // chain above it has to go too. Loop until a pass removes nothing.
    loop {
        let pass = tx.execute(
            "DELETE FROM items WHERE library_id = ?1 AND kind = 'folder'
             AND id NOT IN (SELECT parent_id FROM items
                            WHERE parent_id IS NOT NULL)",
            params![library_id],
        )? as u64;
        removed += pass;
        if pass == 0 {
            break;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
