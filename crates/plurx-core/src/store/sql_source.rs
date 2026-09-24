//! Small, typed SQL sources shared by both durable-store dialects.
//!
//! The parameter declaration order is the binding order. Rendering assigns
//! that same index to every occurrence, so SQLite's numeric `?N` and
//! hiqlite's first-appearance-sensitive `$N` cannot drift apart.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Param {
    UserId,
    Limit,
}

impl Param {
    const fn token(self) -> &'static str {
        match self {
            Self::UserId => "@user_id@",
            Self::Limit => "@limit@",
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
}
