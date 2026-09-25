//! Small, typed SQL sources shared by both durable-store dialects.
//!
//! The parameter declaration order is the binding order. Rendering assigns
//! that same index to every occurrence, so SQLite's numeric `?N` and
//! hiqlite's first-appearance-sensitive `$N` cannot drift apart.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Param {
    UserId,
    Limit,
    LibraryId,
    WindowOffset,
}

impl Param {
    const fn token(self) -> &'static str {
        match self {
            Self::UserId => "@user_id@",
            Self::Limit => "@limit@",
            Self::LibraryId => "@library_id@",
            Self::WindowOffset => "@window_offset@",
        }
    }
}

#[derive(Debug)]
pub(super) struct Stmt {
    template: String,
    params: Vec<Param>,
}

impl Stmt {
    fn new(template: String, params: &[Param]) -> Self {
        debug_assert!(
            params.iter().all(|param| template.contains(param.token())),
            "every declared parameter occurs in the SQL source"
        );
        debug_assert!(
            params
                .iter()
                .enumerate()
                .all(|(index, param)| !params[..index].contains(param)),
            "a parameter is declared once and reused by token"
        );
        Self {
            template,
            params: params.to_vec(),
        }
    }

    fn render(&self, sigil: char) -> String {
        let mut sql = self.template.clone();
        for (index, param) in self.params.iter().enumerate() {
            sql = sql.replace(param.token(), &format!("{sigil}{}", index + 1));
        }
        debug_assert!(!sql.contains('@'), "no parameter token remains unresolved");
        sql
    }

    pub(super) fn sqlite(&self) -> String {
        self.render('?')
    }

    #[cfg(feature = "hiqlite-store")]
    pub(super) fn hiqlite(&self) -> String {
        self.render('$')
    }
}

/// The first window `recently_added` reads, in rows per requested card.
pub(super) const RECENTLY_ADDED_FIRST_WINDOW: i64 = 8;

/// The zero-based offset of the row whose `added_at` bounds the window of
/// `window_rows` newest rows, or `i64::MAX` (no bound: the whole candidate
/// set) for a negative limit, which SQLite reads as "no limit".
pub(super) fn recently_added_first_window_offset(limit: i64) -> i64 {
    if limit < 0 {
        return i64::MAX;
    }
    limit
        .max(1)
        .saturating_mul(RECENTLY_ADDED_FIRST_WINDOW)
        .saturating_sub(1)
}

/// The next window after one that was cut and yielded too few cards.
pub(super) fn recently_added_wider_window_offset(offset: i64) -> i64 {
    offset.saturating_mul(2).saturating_add(1)
}

/// One card per movie, per show (its newest episode represents it) and per
/// home video or folder, newest first, read from a window of the newest rows
/// instead of the whole catalogue (K-05 section 3.5).
///
/// `cut` is the `added_at` of the window's last row (`@window_offset@` rows
/// down the `idx_items_added` order, under the same predicates as the
/// ranking); the ranking then reads every row at or after it. Cutting on
/// `added_at` alone, not on `(added_at, id)`, keeps every row that ties with
/// the boundary inside the window, so for every group with a row in the
/// window, all of the group's rows at its newest `added_at` are in the
/// window too, and its representative (newest by `added_at`, then season,
/// episode and id) is the one the whole-catalogue ranking picks. A group
/// with no row in the window is older than every group in it. So the first
/// `limit` cards are exact whenever the window holds at least `limit`
/// groups, and when `cut` is empty the window is the whole candidate set.
/// `rail_window_cut` tells the caller which case it is in: a cut window that
/// yielded fewer than `limit` cards must be widened and read again.
///
/// The catalogue-wide rail (`@library_id@` null) leaves out Recordings
/// libraries on every backend and read path; a Recordings library's own
/// rail still lists its recordings.
pub(super) fn recently_added(item_columns: &str, ranked_columns: &str) -> Stmt {
    let filter = "i.kind IN ('movie','episode','video','folder','book','audiobook') \
         AND (@library_id@ IS NULL OR i.library_id = @library_id@) \
         AND (@library_id@ IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l \
             WHERE l.id = i.library_id AND l.kind = 'recordings'))";
    Stmt::new(
        format!(
            "WITH cut AS ( \
                 SELECT i.added_at FROM items i WHERE {filter} \
                 ORDER BY i.added_at DESC LIMIT 1 OFFSET @window_offset@ \
             ), ranked AS ( \
                 SELECT {item_columns}, show.title AS rail_show_title, \
                        season.poster_path AS rail_season_poster, \
                        ROW_NUMBER() OVER (PARTITION BY CASE \
                            WHEN i.kind = 'episode' AND show.id IS NOT NULL \
                            THEN 'show:' || show.id ELSE 'item:' || i.id END \
                            ORDER BY i.added_at DESC, COALESCE(season.season_number, -1) DESC, \
                            COALESCE(i.episode_number, -1) DESC, i.id DESC) AS rail_rank \
                 FROM items i \
                 LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' \
                 LEFT JOIN items show ON show.id = season.parent_id \
                 WHERE {filter} \
                   AND i.added_at >= COALESCE((SELECT added_at FROM cut), -9223372036854775808) \
             ) \
             SELECT {ranked_columns}, r.rail_show_title, r.rail_season_poster, \
                    EXISTS (SELECT 1 FROM cut) AS rail_window_cut \
             FROM ranked r WHERE r.rail_rank = 1 \
             ORDER BY r.added_at DESC, r.id DESC LIMIT @limit@"
        ),
        &[Param::LibraryId, Param::WindowOffset, Param::Limit],
    )
}

pub(super) fn next_up(item_columns: &str) -> Stmt {
    Stmt::new(
        format!(
            "SELECT {item_columns}, show.title AS rail_show_title, \
                    season.poster_path AS rail_season_poster, \
                    MIN(season.season_number*100000 + e.episode_number) AS ord \
             {NEXT_UP_FROM}"
        ),
        &[Param::UserId, Param::Limit],
    )
}

/// Everything after `next_up`'s select list: shared verbatim by
/// [`progress_rails`] so the two can never pick different episodes.
const NEXT_UP_FROM: &str = "FROM items e JOIN items season ON season.id = e.parent_id \
             JOIN items show ON show.id = season.parent_id \
             WHERE e.kind = 'episode' \
               AND e.id NOT IN (SELECT item_id FROM watch_state \
                                WHERE user_id = @user_id@ AND (watched = 1 OR position_ms > 0)) \
               AND (season.season_number*100000 + e.episode_number) > ( \
                   SELECT COALESCE(MAX(se.season_number*100000 + ep.episode_number), -1) \
                   FROM watch_state w JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' \
                   JOIN items se ON se.id = ep.parent_id \
                   WHERE w.user_id = @user_id@ AND w.watched = 1 AND se.parent_id = show.id) \
               AND show.id IN (SELECT sh.id FROM watch_state w \
                   JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' \
                   JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id \
                   WHERE w.user_id = @user_id@ AND w.watched = 1) \
               AND show.id NOT IN (SELECT sh.id FROM watch_state w \
                   JOIN items ep ON ep.id = w.item_id AND ep.kind = 'episode' \
                   JOIN items se ON se.id = ep.parent_id JOIN items sh ON sh.id = se.parent_id \
                   WHERE w.user_id = @user_id@ AND w.watched = 0 AND w.position_ms > 0) \
             GROUP BY show.id ORDER BY show.sort_title LIMIT @limit@";

/// Continue-watching and next-up as one statement (K-04 M3).
///
/// Each rail keeps its own filter, order and limit inside its own
/// subquery, and the outer order restores each rail's order from the key it
/// was ranked by: `rail_key_int` (most recent progress first) for
/// continue-watching, `rail_key_text` (show sort title) for next-up. Rows
/// that tie on a rail's key were in no defined order before this statement
/// and are in none now. Columns: `rail`, `rail_key_int`, `rail_key_text`,
/// then the item columns, `rail_show_title`, `rail_season_poster`, the four
/// `watch_*` columns (NULL on next-up rows) and `ord` (NULL on
/// continue-watching rows).
pub(super) fn progress_rails(in_progress_columns: &str, next_up_columns: &str) -> Stmt {
    Stmt::new(
        format!(
            "SELECT * FROM ( \
                 SELECT 0 AS rail, w.updated_at AS rail_key_int, NULL AS rail_key_text, \
                        {in_progress_columns}, show.title AS rail_show_title, \
                        season.poster_path AS rail_season_poster, \
                        w.position_ms AS watch_position_ms, \
                        w.duration_ms AS watch_duration_ms, \
                        w.watched AS watch_watched, w.updated_at AS watch_updated_at, \
                        NULL AS ord \
                 FROM watch_state w JOIN items i ON i.id = w.item_id \
                 LEFT JOIN items season ON season.id = i.parent_id AND i.kind = 'episode' \
                 LEFT JOIN items show ON show.id = season.parent_id \
                 WHERE w.user_id = @user_id@ AND w.watched = 0 AND w.position_ms > 0 \
                   AND i.kind IN ('movie','episode','video','audiobook') \
                 ORDER BY w.updated_at DESC LIMIT @limit@ \
             ) \
             UNION ALL \
             SELECT * FROM ( \
                 SELECT 1 AS rail, NULL AS rail_key_int, show.sort_title AS rail_key_text, \
                        {next_up_columns}, show.title AS rail_show_title, \
                        season.poster_path AS rail_season_poster, \
                        NULL AS watch_position_ms, NULL AS watch_duration_ms, \
                        NULL AS watch_watched, NULL AS watch_updated_at, \
                        MIN(season.season_number*100000 + e.episode_number) AS ord \
                 {NEXT_UP_FROM} \
             ) \
             ORDER BY rail, rail_key_int DESC, rail_key_text"
        ),
        &[Param::UserId, Param::Limit],
    )
}

#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    use super::*;

    #[test]
    fn next_up_renders_equivalent_valid_dialects_from_one_parameter_order() {
        let statement = next_up("e.id, e.title");
        assert_eq!(statement.params, [Param::UserId, Param::Limit]);
        let sqlite = statement.sqlite();
        let hiqlite = statement.hiqlite();
        assert_eq!(sqlite.replace('?', "$"), hiqlite);
        super::super::placeholder_census::validate_sqlite_placeholders(&sqlite)
            .expect("the SQLite dialect has contiguous numeric placeholders");
        super::super::hiqlite::validate_sql(&hiqlite)
            .expect("the replicated dialect introduces placeholders in binding order");
    }

    #[test]
    fn progress_rails_renders_equivalent_valid_dialects_and_embeds_next_up_verbatim() {
        let statement = progress_rails("i.id, i.title", "e.id, e.title");
        assert_eq!(statement.params, [Param::UserId, Param::Limit]);
        let sqlite = statement.sqlite();
        let hiqlite = statement.hiqlite();
        assert_eq!(sqlite.replace('?', "$"), hiqlite);
        super::super::placeholder_census::validate_sqlite_placeholders(&sqlite)
            .expect("the SQLite dialect has contiguous numeric placeholders");
        super::super::hiqlite::validate_sql(&hiqlite)
            .expect("the replicated dialect introduces placeholders in binding order");
        let next_up_tail = next_up("e.id, e.title").hiqlite();
        let tail = &next_up_tail[next_up_tail.find("FROM items e").expect("next-up FROM")..];
        assert!(
            hiqlite.contains(tail),
            "next-up rail must run next_up's exact FROM"
        );
    }

    #[test]
    fn recently_added_renders_equivalent_valid_dialects_from_one_parameter_order() {
        let statement = recently_added("i.id, i.added_at", "r.id, r.added_at");
        assert_eq!(
            statement.params,
            [Param::LibraryId, Param::WindowOffset, Param::Limit]
        );
        let sqlite = statement.sqlite();
        let hiqlite = statement.hiqlite();
        assert_eq!(sqlite.replace('?', "$"), hiqlite);
        super::super::placeholder_census::validate_sqlite_placeholders(&sqlite)
            .expect("the SQLite dialect has contiguous numeric placeholders");
        super::super::hiqlite::validate_sql(&hiqlite)
            .expect("the replicated dialect introduces placeholders in binding order");
    }

    #[test]
    fn recently_added_windows_start_at_eight_rows_a_card_and_double() {
        assert_eq!(recently_added_first_window_offset(24), 191);
        assert_eq!(recently_added_first_window_offset(0), 7);
        assert_eq!(recently_added_first_window_offset(-1), i64::MAX);
        assert_eq!(recently_added_wider_window_offset(191), 383);
        assert_eq!(recently_added_wider_window_offset(i64::MAX), i64::MAX);
    }
}
