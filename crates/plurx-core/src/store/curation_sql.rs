//! The SQL both curation backends run, written once.
//!
//! Every statement numbers its placeholders `$1, $2, …` in order of first
//! appearance, which is the one spelling both rusqlite (positional binding of
//! named `$N` parameters by first appearance) and hiqlite's `validate_sql`
//! accept. Keeping the text in one place is what stops the SQLite and hiqlite
//! twins from drifting into two different meanings of "visible" or "matches".

use crate::curation::{RowSort, RuleQuery, Term, Unwatched};
use crate::domain::ItemPresence;

/// The visibility predicate for a tag aliased `t`, viewer bound at `$N`.
fn visible(alias: &str, viewer: &str) -> String {
    format!("({alias}.owner_user_id IS NULL OR {alias}.owner_user_id = {viewer})")
}

/// `TOP_LEVEL_ITEM_PREDICATE` as a literal, so constant statements can embed
/// it (a test pins the two equal): tags go on titles, never on seasons or
/// episodes, and suggestions offer only what a grid could show.
macro_rules! top_level {
    () => {
        "(kind IN ('movie','show','book','audiobook') OR \
     (kind IN ('folder','video','photo') AND parent_id IS NULL))"
    };
}

macro_rules! tag_cols {
    () => {
        "t.id, t.name, t.owner_user_id, t.color, t.suggest_terms, t.archived_at_ms, \
         t.created_by, t.created_at_ms, \
         (SELECT count(*) FROM library_curation_item_tags c WHERE c.tag_id = t.id) AS item_count"
    };
}

pub(crate) const USER_GRANTS: &str =
    "SELECT grant_name FROM library_curation_grants WHERE user_id = $1 ORDER BY grant_name";
pub(crate) const GRANT: &str =
    "INSERT INTO library_curation_grants(user_id, grant_name, granted_by, granted_at_ms) \
     SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT 1 FROM users WHERE id = $1) \
     ON CONFLICT(user_id, grant_name) DO NOTHING";
pub(crate) const REVOKE: &str =
    "DELETE FROM library_curation_grants WHERE user_id = $1 AND grant_name = $2";

pub(crate) const VISIBLE_TAGS: &str = concat!(
    "SELECT ",
    tag_cols!(),
    " FROM library_curation_tags t WHERE (t.owner_user_id IS NULL OR t.owner_user_id = $1) \
     ORDER BY t.name_key, t.id"
);
pub(crate) const TAG_BY_ID: &str = concat!(
    "SELECT ",
    tag_cols!(),
    " FROM library_curation_tags t WHERE t.id = $1"
);
/// `ON CONFLICT DO NOTHING` + `RETURNING`: no row back means a tag with that
/// key already exists in that scope, which the caller reports as such.
pub(crate) const INSERT_TAG: &str = "INSERT INTO library_curation_tags(name, name_key, owner_user_id, color, suggest_terms, created_by, created_at_ms) \
     VALUES($1, $2, $3, $4, '[]', $5, $6) ON CONFLICT DO NOTHING RETURNING id";
/// Each optional field is `NULL` to leave it alone; `$5` says whether the
/// archive stamp is being set (to `$6`, which may itself be `NULL`).
pub(crate) const UPDATE_TAG: &str = "UPDATE library_curation_tags SET \
     name = COALESCE($1, name), name_key = COALESCE($2, name_key), color = COALESCE($3, color), \
     suggest_terms = COALESCE($4, suggest_terms), \
     archived_at_ms = CASE WHEN $5 THEN $6 ELSE archived_at_ms END \
     WHERE id = $7";
pub(crate) const DELETE_TAG: &str = "DELETE FROM library_curation_tags WHERE id = $1";
/// Merge `$2` into `$1`: memberships and rejections move (the target's own
/// rows win a collision), then the source goes, cascading what is left.
pub(crate) const MERGE_MEMBERS: &str = "INSERT INTO library_curation_item_tags(tag_id, item_id, added_by, added_at_ms, source) \
     SELECT $1, item_id, added_by, added_at_ms, source FROM library_curation_item_tags WHERE tag_id = $2 \
     ON CONFLICT(tag_id, item_id) DO NOTHING";
pub(crate) const MERGE_REJECTIONS: &str = "INSERT INTO library_curation_rejections(tag_id, item_id, rejected_by, rejected_at_ms) \
     SELECT $1, item_id, rejected_by, rejected_at_ms FROM library_curation_rejections WHERE tag_id = $2 \
     ON CONFLICT(tag_id, item_id) DO NOTHING";

/// Add the items in JSON array `$5` to tag `$1`. Items that no longer exist
/// are skipped rather than failing the batch; existing memberships keep
/// their original time, so "recently tagged" means when it was first tagged.
pub(crate) const ADD_MEMBERS: &str = concat!(
    "INSERT INTO library_curation_item_tags(tag_id, item_id, added_by, added_at_ms, source) \
     SELECT $1, j.value, $2, $3, $4 FROM json_each($5) j WHERE EXISTS (SELECT 1 FROM items WHERE id = j.value AND ",
    top_level!(),
    ") ON CONFLICT(tag_id, item_id) DO NOTHING"
);
pub(crate) const REMOVE_MEMBERS: &str = "DELETE FROM library_curation_item_tags \
     WHERE tag_id = $1 AND item_id IN (SELECT value FROM json_each($2))";
pub(crate) const REJECT: &str = concat!(
    "INSERT INTO library_curation_rejections(tag_id, item_id, rejected_by, rejected_at_ms) \
     SELECT $1, j.value, $2, $3 FROM json_each($4) j WHERE EXISTS (SELECT 1 FROM items WHERE id = j.value AND ",
    top_level!(),
    ") ON CONFLICT(tag_id, item_id) DO NOTHING"
);
pub(crate) const UNREJECT: &str = "DELETE FROM library_curation_rejections \
     WHERE tag_id = $1 AND item_id IN (SELECT value FROM json_each($2))";
pub(crate) const REJECTED_ITEMS: &str = "SELECT r.item_id FROM library_curation_rejections r \
     JOIN items i ON i.id = r.item_id WHERE r.tag_id = $1 \
     ORDER BY r.rejected_at_ms DESC, r.item_id DESC LIMIT $2 OFFSET $3";

/// The tags on one item that the viewer may see.
pub(crate) const ITEM_TAG_IDS: &str = "SELECT it.tag_id FROM library_curation_item_tags it \
     JOIN library_curation_tags t ON t.id = it.tag_id \
     WHERE it.item_id = $1 AND (t.owner_user_id IS NULL OR t.owner_user_id = $2) ORDER BY it.tag_id";

/// A classifier term matches a suggestion term case-insensitively. Terms are
/// provider keywords as TMDB spells them and `dimension:value` labels.
const SUGGEST_MATCH: &str = "EXISTS (SELECT 1 FROM json_each(c.terms) k \
     WHERE lower(k.value) IN (SELECT value FROM json_each(t.suggest_terms)))";
const SUGGEST_FROM: &str = concat!(
    "FROM media_classifications c \
     JOIN library_curation_tags t ON t.id = $1 \
     JOIN items i ON i.id = c.item_id \
     AND EXISTS (SELECT 1 FROM items WHERE id = c.item_id AND ",
    top_level!(),
    ")"
);
const SUGGEST_OPEN: &str = "NOT EXISTS (SELECT 1 FROM library_curation_item_tags m WHERE m.tag_id = t.id AND m.item_id = c.item_id) \
     AND NOT EXISTS (SELECT 1 FROM library_curation_rejections r WHERE r.tag_id = t.id AND r.item_id = c.item_id)";

pub(crate) fn suggestion_count_sql() -> String {
    format!("SELECT count(*) AS value {SUGGEST_FROM} WHERE {SUGGEST_MATCH} AND {SUGGEST_OPEN}")
}

pub(crate) fn suggestion_page_sql() -> String {
    format!(
        "SELECT c.item_id AS item_id, \
           (SELECT json_group_array(k.value) FROM json_each(c.terms) k \
             WHERE lower(k.value) IN (SELECT value FROM json_each(t.suggest_terms))) AS matched \
         {SUGGEST_FROM} WHERE {SUGGEST_MATCH} AND {SUGGEST_OPEN} \
         ORDER BY i.sort_title COLLATE NOCASE, i.id LIMIT $2 OFFSET $3"
    )
}

pub(crate) const VISIBLE_ROWS: &str =
    "SELECT id, owner_user_id, title, spec, created_by, created_at_ms, updated_at_ms \
     FROM library_curation_rows WHERE (owner_user_id IS NULL OR owner_user_id = $1) \
     ORDER BY owner_user_id IS NOT NULL, created_at_ms, id";
pub(crate) const ROW_BY_ID: &str =
    "SELECT id, owner_user_id, title, spec, created_by, created_at_ms, updated_at_ms \
     FROM library_curation_rows WHERE id = $1";
pub(crate) const ROWS_NAMING_TAG: &str = "SELECT id, owner_user_id, title, spec, created_by, created_at_ms, updated_at_ms \
     FROM library_curation_rows r WHERE EXISTS ( \
       SELECT 1 FROM json_each(r.spec, '$.all_of') g WHERE json_extract(g.value, '$.tag') = $1 \
       UNION ALL SELECT 1 FROM json_each(r.spec, '$.any_of') g WHERE json_extract(g.value, '$.tag') = $1 \
       UNION ALL SELECT 1 FROM json_each(r.spec, '$.none_of') g WHERE json_extract(g.value, '$.tag') = $1) \
     ORDER BY id";
pub(crate) const INSERT_ROW: &str = "INSERT INTO library_curation_rows(owner_user_id, title, spec, created_by, created_at_ms, updated_at_ms) \
     VALUES($1, $2, $3, $4, $5, $5) RETURNING id";
pub(crate) const UPDATE_ROW: &str =
    "UPDATE library_curation_rows SET title = $1, spec = $2, updated_at_ms = $3 WHERE id = $4";
pub(crate) const DELETE_ROW: &str = "DELETE FROM library_curation_rows WHERE id = $1";

pub(crate) const HOME_LAYOUT: &str = "SELECT layout FROM library_curation_home WHERE user_id = $1";
pub(crate) const PUT_HOME_LAYOUT: &str = "INSERT INTO library_curation_home(user_id, layout, updated_at_ms) \
     SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM users WHERE id = $1) \
     ON CONFLICT(user_id) DO UPDATE SET layout = excluded.layout, updated_at_ms = excluded.updated_at_ms";
pub(crate) const CLEAR_HOME_LAYOUT: &str = "DELETE FROM library_curation_home WHERE user_id = $1";

/// A bound value, in placeholder order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Arg {
    Int(i64),
    Text(String),
}

/// Builds a statement whose placeholders are numbered in the order they are
/// written, which is the only order both backends accept.
struct Builder {
    args: Vec<Arg>,
    viewer_id: i64,
    viewer: Option<String>,
}

impl Builder {
    fn bind(&mut self, arg: Arg) -> String {
        self.args.push(arg);
        format!("${}", self.args.len())
    }
    /// The viewer's placeholder, bound where it is first written: callers
    /// build the statement left to right, so first use is first appearance.
    fn viewer(&mut self) -> String {
        if let Some(v) = &self.viewer {
            return v.clone();
        }
        let v = self.bind(Arg::Int(self.viewer_id));
        self.viewer = Some(v.clone());
        v
    }
    /// A movie is unwatched without a watched mark; a show while any of its
    /// episodes lacks one. Seasons sit between show and episode.
    fn unwatched(&mut self) -> String {
        let v = self.viewer();
        format!(
            "(CASE WHEN items.kind = 'show' THEN EXISTS (SELECT 1 FROM items s JOIN items e ON e.parent_id = s.id \
                WHERE s.parent_id = items.id AND e.kind = 'episode' AND NOT EXISTS ( \
                  SELECT 1 FROM watch_state w WHERE w.user_id = {v} AND w.item_id = e.id AND w.watched = 1)) \
              ELSE NOT EXISTS (SELECT 1 FROM watch_state w WHERE w.user_id = {v} AND w.item_id = items.id AND w.watched = 1) END)"
        )
    }
}

/// The top-level titles a rule matches, in the rule's order: one statement
/// for the page (`Some((offset, limit))`) or the count (`None`). Built per
/// call, so a rule's shape is never interpolated — only bound.
///
/// Tags are matched through the viewer's visibility in SQL, so a private tag
/// id smuggled into someone else's rule matches nothing.
pub(crate) fn rule_sql(query: &RuleQuery<'_>, page: Option<(i64, i64)>) -> (String, Vec<Arg>) {
    let mut b = Builder {
        args: Vec::new(),
        viewer_id: query.viewer,
        viewer: None,
    };
    let spec = query.spec;
    // As written, so SQLite can serve the scan from the partial top-level
    // index exactly as it does for a library grid.
    let mut wheres = vec![super::TOP_LEVEL_ITEM_PREDICATE.to_owned()];
    if query.presence != ItemPresence::Everything {
        wheres.push(query.presence.sql().to_owned());
    }

    let term_sql = |b: &mut Builder, term: &Term| -> String {
        match term {
            Term::Tag(id) => {
                let id = b.bind(Arg::Int(*id));
                let viewer = b.viewer();
                format!(
                    "EXISTS (SELECT 1 FROM library_curation_item_tags it JOIN library_curation_tags t ON t.id = it.tag_id \
                     WHERE it.item_id = items.id AND it.tag_id = {id} AND {})",
                    visible("t", &viewer)
                )
            }
            Term::Genre(genre) => {
                let g = b.bind(Arg::Text(genre.clone()));
                format!("EXISTS (SELECT 1 FROM json_each(items.genres) WHERE value = {g} COLLATE NOCASE)")
            }
        }
    };
    if !spec.libraries.is_empty() {
        let ids = b.bind(Arg::Text(
            serde_json::to_string(&spec.libraries).expect("ids serialize"),
        ));
        wheres.push(format!(
            "items.library_id IN (SELECT value FROM json_each({ids}))"
        ));
    }
    if !spec.kinds.is_empty() {
        let kinds = b.bind(Arg::Text(
            serde_json::to_string(&spec.kinds).expect("kinds serialize"),
        ));
        wheres.push(format!(
            "items.kind IN (SELECT value FROM json_each({kinds}))"
        ));
    }
    if let Some(only) = query.only_items {
        let ids = b.bind(Arg::Text(
            serde_json::to_string(only).expect("ids serialize"),
        ));
        wheres.push(format!("items.id IN (SELECT value FROM json_each({ids}))"));
    }
    for term in &spec.all_of {
        wheres.push(term_sql(&mut b, term));
    }
    if !spec.any_of.is_empty() {
        let any: Vec<String> = spec.any_of.iter().map(|t| term_sql(&mut b, t)).collect();
        wheres.push(format!("({})", any.join(" OR ")));
    }
    for term in &spec.none_of {
        wheres.push(format!("NOT {}", term_sql(&mut b, term)));
    }
    if spec.unwatched == Unwatched::Only {
        let expr = b.unwatched();
        wheres.push(expr);
    }
    let filter = wheres.join(" AND ");
    let Some((offset, limit)) = page else {
        return (
            format!("SELECT count(*) AS value FROM items WHERE {filter}"),
            b.args,
        );
    };
    let mut order = Vec::new();
    if spec.unwatched == Unwatched::First {
        let expr = b.unwatched();
        order.push(format!("{expr} DESC"));
    }
    match spec.sort {
        RowSort::RecentlyTagged => {
            let tags = spec.positive_tag_ids();
            if !tags.is_empty() {
                let ids = b.bind(Arg::Text(
                    serde_json::to_string(&tags).expect("ids serialize"),
                ));
                // Only memberships of tags the viewer can see may order the
                // page; otherwise a private tag id would leak its titles
                // through the sort.
                let viewer = b.viewer();
                order.push(format!(
                    "(SELECT max(it.added_at_ms) FROM library_curation_item_tags it \
                      JOIN library_curation_tags t ON t.id = it.tag_id \
                      WHERE it.item_id = items.id AND it.tag_id IN (SELECT value FROM json_each({ids})) \
                      AND {}) DESC",
                    visible("t", &viewer)
                ));
            }
            order.push("items.added_at DESC".into());
        }
        RowSort::Added => order.push("items.added_at DESC".into()),
        RowSort::Released => order.push("items.year IS NULL, items.year DESC".into()),
        RowSort::Title => order.push("items.sort_title COLLATE NOCASE".into()),
        RowSort::ShuffleDaily => {
            // A cheap, stable-for-the-day permutation: no RANDOM(), so every
            // node and every page of "See all" agree on the order.
            let seed = b.bind(Arg::Int(query.day.rem_euclid(1_000_003) + 1));
            order.push(format!("((items.id * {seed}) % 1000003)"));
        }
    }
    order.push("items.id DESC".into());
    let limit = b.bind(Arg::Int(limit));
    let offset = b.bind(Arg::Int(offset));
    (
        format!(
            "SELECT items.id AS item_id FROM items WHERE {filter} ORDER BY {} LIMIT {limit} OFFSET {offset}",
            order.join(", ")
        ),
        b.args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::RowSpec;

    fn numbered_in_order(sql: &str, count: usize) {
        let mut next = 1;
        let mut rest = sql;
        while let Some(at) = rest.find('$') {
            let digits: String = rest[at + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.is_empty() {
                rest = &rest[at + 1..];
                continue;
            }
            let n: usize = digits.parse().expect("placeholder number");
            assert!(n <= next, "${n} appears before ${next} in {sql}");
            if n == next {
                next += 1;
            }
            rest = &rest[at + 1 + digits.len()..];
        }
        assert_eq!(next - 1, count, "every bound value is used: {sql}");
    }

    #[test]
    fn every_rule_statement_numbers_placeholders_in_first_appearance_order() {
        let spec = RowSpec {
            all_of: vec![Term::Tag(3), Term::Genre("Drama".into())],
            any_of: vec![Term::Tag(4), Term::Tag(5)],
            none_of: vec![Term::Genre("Horror".into())],
            libraries: vec![1, 2],
            kinds: vec!["movie".into()],
            unwatched: Unwatched::First,
            ..Default::default()
        };
        for sort in [
            RowSort::RecentlyTagged,
            RowSort::Added,
            RowSort::ShuffleDaily,
            RowSort::Title,
        ] {
            let spec = RowSpec {
                sort,
                ..spec.clone()
            };
            let ids = [9, 10];
            let q = RuleQuery {
                viewer: 7,
                spec: &spec,
                only_items: Some(&ids),
                day: 20_000,
                presence: ItemPresence::OnDisk,
            };
            let (sql, args) = rule_sql(&q, Some((0, 20)));
            numbered_in_order(&sql, args.len());
            let (sql, args) = rule_sql(&q, None);
            numbered_in_order(&sql, args.len());
        }
    }

    #[test]
    fn fixed_statements_number_placeholders_in_first_appearance_order() {
        for (sql, n) in [
            (GRANT, 4),
            (INSERT_TAG, 6),
            (UPDATE_TAG, 7),
            (MERGE_MEMBERS, 2),
            (ADD_MEMBERS, 5),
            (REJECT, 4),
            (ITEM_TAG_IDS, 2),
            (ROWS_NAMING_TAG, 1),
            (INSERT_ROW, 5),
            (UPDATE_ROW, 4),
            (PUT_HOME_LAYOUT, 3),
        ] {
            numbered_in_order(sql, n);
        }
        numbered_in_order(&suggestion_page_sql(), 3);
        numbered_in_order(&suggestion_count_sql(), 1);
    }

    #[test]
    fn recently_tagged_orders_only_by_tags_the_viewer_can_see() {
        let spec = RowSpec {
            any_of: vec![Term::Tag(3), Term::Tag(4)],
            sort: RowSort::RecentlyTagged,
            ..Default::default()
        };
        let q = RuleQuery {
            viewer: 7,
            spec: &spec,
            only_items: None,
            day: 0,
            presence: ItemPresence::Everything,
        };
        let (sql, _) = rule_sql(&q, Some((0, 20)));
        let order = &sql[sql.find("ORDER BY").expect("order")..];
        assert!(
            order.contains("t.owner_user_id IS NULL OR t.owner_user_id = $"),
            "{order}"
        );
    }

    #[test]
    fn the_embedded_top_level_predicate_is_the_grids() {
        assert_eq!(
            top_level!().split_whitespace().collect::<Vec<_>>(),
            super::super::TOP_LEVEL_ITEM_PREDICATE
                .split_whitespace()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_tag_term_is_matched_only_through_the_viewers_visibility() {
        let spec = RowSpec {
            all_of: vec![Term::Tag(3)],
            ..Default::default()
        };
        let q = RuleQuery {
            viewer: 7,
            spec: &spec,
            only_items: None,
            day: 0,
            presence: ItemPresence::Everything,
        };
        let (sql, args) = rule_sql(&q, None);
        assert!(
            sql.contains("t.owner_user_id IS NULL OR t.owner_user_id = $2"),
            "{sql}"
        );
        assert_eq!(args[0], Arg::Int(3));
        assert_eq!(args[1], Arg::Int(7));
    }
}
