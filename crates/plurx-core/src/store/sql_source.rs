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
    offset.saturating_add(1).saturating_mul(2).saturating_sub(1)
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
             FROM items e JOIN items season ON season.id = e.parent_id \
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
             GROUP BY show.id ORDER BY show.sort_title LIMIT @limit@"
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
