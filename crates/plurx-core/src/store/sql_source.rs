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
}
