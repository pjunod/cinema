//! Custom tags, saved rows and the Home layout — the person-curated side of a
//! library (docs/features/CUSTOM-TAGS-PLAN.md).
//!
//! Nothing here is catalogue metadata. Provider genres, NFO `items.tags` and the
//! classifier's labels belong to the catalogue and are rewritten on their own
//! clocks; a tag someone put on a title must survive every one of those
//! rewrites, so it lives in its own tables keyed by item id and is only ever
//! written by a person (or by a person accepting a suggestion).
use serde::{Deserialize, Serialize};

/// A right an admin can grant a non-admin. Admins hold every grant implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    /// Create and edit household tags, apply them to titles, review
    /// suggestions, and publish household rows.
    ManageTags,
}

impl Grant {
    pub const ALL: [Grant; 1] = [Grant::ManageTags];
    pub fn as_str(self) -> &'static str {
        match self {
            Grant::ManageTags => "manage_tags",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|grant| grant.as_str() == value)
    }
}

/// Who sees a tag or a row: the whole household, or only its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Household,
    Private,
}

/// How a membership came to exist. Kept so "recently tagged" and a future
/// audit can tell a person's click from an accepted suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagSource {
    Manual,
    Suggestion,
}

impl TagSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TagSource::Manual => "manual",
            TagSource::Suggestion => "suggestion",
        }
    }
}

/// The fixed tag palette. A closed set so every client can render it and no
/// client has to sanitise a colour string it was handed.
pub const TAG_COLORS: [&str; 10] = [
    "violet", "blue", "teal", "green", "lime", "amber", "orange", "red", "pink", "slate",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// `None` = household; `Some(user)` = private to that user.
    pub owner_user_id: Option<i64>,
    pub color: String,
    /// Classifier terms (TMDB keywords, classifier labels) whose matches are
    /// offered for review. Never applied without a person accepting them.
    pub suggest_terms: Vec<String>,
    pub archived_at_ms: Option<i64>,
    pub created_by: i64,
    pub created_at_ms: i64,
    /// Titles carrying the tag.
    pub item_count: i64,
}

impl Tag {
    pub fn scope(&self) -> Scope {
        if self.owner_user_id.is_some() {
            Scope::Private
        } else {
            Scope::Household
        }
    }
    pub fn visible_to(&self, user_id: i64) -> bool {
        self.owner_user_id.is_none_or(|owner| owner == user_id)
    }
}

#[derive(Debug, Clone)]
pub struct NewTag {
    pub name: String,
    pub owner_user_id: Option<i64>,
    pub color: String,
    pub created_by: i64,
}

#[derive(Debug, Clone, Default)]
pub struct TagEdit {
    pub name: Option<String>,
    pub color: Option<String>,
    pub suggest_terms: Option<Vec<String>>,
    /// `Some(Some(t))` archives at `t`, `Some(None)` unarchives.
    pub archived_at_ms: Option<Option<i64>>,
}

pub const MAX_TAG_NAME: usize = 64;
pub const MAX_SUGGEST_TERMS: usize = 32;
pub const MAX_BULK_ITEMS: usize = 5000;

/// The comparison key that makes "Gay-Themed", " gay‑themed " and
/// "gay-themed" one tag: Unicode lowercase, trimmed, internal whitespace
/// collapsed to one space. The display name keeps what was typed.
pub fn tag_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A display name as stored: trimmed, whitespace collapsed, bounded, no
/// control characters.
pub fn clean_tag_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("A tag needs a name.".into());
    }
    if name.chars().count() > MAX_TAG_NAME {
        return Err(format!("Tag names are at most {MAX_TAG_NAME} characters."));
    }
    if name.chars().any(char::is_control) {
        return Err("Tag names cannot contain control characters.".into());
    }
    Ok(name)
}

pub fn clean_color(color: Option<&str>) -> Result<String, String> {
    match color.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(TAG_COLORS[0].to_owned()),
        Some(c) if TAG_COLORS.contains(&c) => Ok(c.to_owned()),
        Some(_) => Err(format!("Colour must be one of {}.", TAG_COLORS.join(", "))),
    }
}

/// Suggestion terms are matched against the classifier's stored terms, which
/// are provider keywords as TMDB spells them plus `dimension:value` labels;
/// both compare case-insensitively, so they are stored lowercase.
pub fn clean_suggest_terms(terms: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for term in terms {
        let term = tag_key(term);
        if term.is_empty() {
            continue;
        }
        if term.chars().count() > 100 || term.chars().any(char::is_control) {
            return Err("Suggestion terms are at most 100 characters.".into());
        }
        if !out.contains(&term) {
            out.push(term);
        }
    }
    if out.len() > MAX_SUGGEST_TERMS {
        return Err(format!("At most {MAX_SUGGEST_TERMS} suggestion terms."));
    }
    Ok(out)
}

/// One suggested title: why it matched, so a person can judge a broad keyword
/// such as `lgbt` before accepting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub item_id: i64,
    pub matched: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuggestionPage {
    pub suggestions: Vec<Suggestion>,
    pub total: i64,
}

/// A page of item ids and the total the same predicate matches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IdPage {
    pub item_ids: Vec<i64>,
    pub total: i64,
}

// ---------------------------------------------------------------------------
// Rules: what a saved row, a filtered grid and a `tag:` search all evaluate.
// ---------------------------------------------------------------------------

/// One condition on a title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Term {
    /// Carries this tag (only tags visible to the viewer ever match).
    Tag(i64),
    /// Has this provider genre (case-insensitive).
    Genre(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RowSort {
    #[default]
    RecentlyTagged,
    Added,
    Released,
    Title,
    ShuffleDaily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Unwatched {
    #[default]
    Any,
    First,
    Only,
}

pub const MAX_TERMS_PER_GROUP: usize = 16;
pub const MAX_ROW_LIMIT: i64 = 50;
pub const DEFAULT_ROW_LIMIT: i64 = 20;

/// A saved row's definition, and equally the shape of a filtered grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RowSpec {
    pub all_of: Vec<Term>,
    pub any_of: Vec<Term>,
    pub none_of: Vec<Term>,
    /// Empty = every library.
    pub libraries: Vec<i64>,
    /// Empty = every top-level kind; otherwise item kinds such as `movie`, `show`.
    pub kinds: Vec<String>,
    pub sort: RowSort,
    pub unwatched: Unwatched,
    pub limit: Option<i64>,
}

impl RowSpec {
    pub fn validate(&mut self) -> Result<(), String> {
        for group in [&self.all_of, &self.any_of, &self.none_of] {
            if group.len() > MAX_TERMS_PER_GROUP {
                return Err(format!(
                    "At most {MAX_TERMS_PER_GROUP} conditions per group."
                ));
            }
        }
        for term in self
            .all_of
            .iter_mut()
            .chain(&mut self.any_of)
            .chain(&mut self.none_of)
        {
            if let Term::Genre(genre) = term {
                *genre = genre.trim().to_owned();
                if genre.is_empty() || genre.len() > 100 {
                    return Err("Genre conditions need a genre name.".into());
                }
            }
        }
        for kind in &self.kinds {
            if ![
                "movie",
                "show",
                "book",
                "audiobook",
                "folder",
                "video",
                "photo",
            ]
            .contains(&kind.as_str())
            {
                return Err(format!("Unknown kind {kind}."));
            }
        }
        if self.libraries.len() > 64 || self.kinds.len() > 8 {
            return Err("Too many libraries or kinds.".into());
        }
        if let Some(limit) = self.limit {
            if !(1..=MAX_ROW_LIMIT).contains(&limit) {
                return Err(format!("Rows show 1 to {MAX_ROW_LIMIT} titles."));
            }
        }
        if self.all_of.is_empty()
            && self.any_of.is_empty()
            && self.libraries.is_empty()
            && self.kinds.is_empty()
        {
            return Err("A row needs at least one condition.".into());
        }
        Ok(())
    }
    pub fn limit(&self) -> i64 {
        self.limit
            .unwrap_or(DEFAULT_ROW_LIMIT)
            .clamp(1, MAX_ROW_LIMIT)
    }
    /// Every tag the rule names, in any group.
    pub fn tag_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .all_of
            .iter()
            .chain(&self.any_of)
            .chain(&self.none_of)
            .filter_map(|t| match t {
                Term::Tag(id) => Some(*id),
                Term::Genre(_) => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
    /// The tags whose membership time "recently tagged" orders by: the ones a
    /// title must or may carry, not the ones it must not.
    pub fn positive_tag_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .all_of
            .iter()
            .chain(&self.any_of)
            .filter_map(|t| match t {
                Term::Tag(id) => Some(*id),
                Term::Genre(_) => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// A rule evaluated for one person on one day.
#[derive(Debug, Clone)]
pub struct RuleQuery<'a> {
    pub viewer: i64,
    pub spec: &'a RowSpec,
    /// Restrict to these ids (a `tag:` search narrows a text search's hits).
    pub only_items: Option<&'a [i64]>,
    /// Seed for `ShuffleDaily`: the same order all day, a new one tomorrow.
    pub day: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedRow {
    pub id: i64,
    pub title: String,
    pub owner_user_id: Option<i64>,
    pub spec: RowSpec,
    pub created_by: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl SavedRow {
    pub fn visible_to(&self, user_id: i64) -> bool {
        self.owner_user_id.is_none_or(|owner| owner == user_id)
    }
}

pub const MAX_ROW_TITLE: usize = 80;

pub fn clean_row_title(title: &str) -> Result<String, String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty()
        || title.chars().count() > MAX_ROW_TITLE
        || title.chars().any(char::is_control)
    {
        return Err(format!("Row titles are 1 to {MAX_ROW_TITLE} characters."));
    }
    Ok(title)
}

// ---------------------------------------------------------------------------
// Home layout
// ---------------------------------------------------------------------------

/// The built-in Home rows, in today's order. A layout nobody stored resolves
/// to exactly this, so Home does not change for anyone until they change it.
pub const BUILTIN_HOME: [&str; 7] = [
    "continue_watching",
    "next_up",
    "watchlist",
    "favorites",
    "recently_added",
    "coming_soon",
    "libraries",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeEntry {
    /// A built-in name from [`BUILTIN_HOME`], or `row:<id>`.
    pub key: String,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HomeLayout {
    pub entries: Vec<HomeEntry>,
}

pub fn row_key(id: i64) -> String {
    format!("row:{id}")
}

pub fn parse_row_key(key: &str) -> Option<i64> {
    key.strip_prefix("row:")?.parse().ok()
}

impl HomeLayout {
    pub fn builtin() -> Self {
        Self {
            entries: BUILTIN_HOME
                .iter()
                .map(|key| HomeEntry {
                    key: (*key).to_owned(),
                    hidden: false,
                })
                .collect(),
        }
    }

    /// Structural validation of a layout someone is saving: known keys only,
    /// each once, bounded.
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 200 {
            return Err("Too many Home entries.".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let known =
                BUILTIN_HOME.contains(&entry.key.as_str()) || parse_row_key(&entry.key).is_some();
            if !known {
                return Err(format!("Unknown Home entry {}.", entry.key));
            }
            if !seen.insert(entry.key.clone()) {
                return Err(format!("{} appears twice.", entry.key));
            }
        }
        Ok(())
    }

    /// What a person actually sees: the stored order with entries that no
    /// longer exist dropped, then every built-in and every visible row the
    /// layout never mentioned appended — so a household row published after
    /// someone arranged their Home still reaches them, and a built-in added
    /// in a later release is not invisible to everyone with a saved layout.
    pub fn resolve(&self, visible_rows: &[i64]) -> HomeLayout {
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let live = BUILTIN_HOME.contains(&entry.key.as_str())
                || parse_row_key(&entry.key).is_some_and(|id| visible_rows.contains(&id));
            if live && seen.insert(entry.key.clone()) {
                out.push(entry.clone());
            }
        }
        for key in BUILTIN_HOME
            .iter()
            .map(|k| (*k).to_owned())
            .chain(visible_rows.iter().map(|id| row_key(*id)))
        {
            if seen.insert(key.clone()) {
                out.push(HomeEntry { key, hidden: false });
            }
        }
        HomeLayout { entries: out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_key_folds_case_and_whitespace_but_keeps_words() {
        assert_eq!(tag_key("  Gay-Themed \t Films "), "gay-themed films");
        assert_eq!(tag_key("ÉTÉ"), "été");
        assert_ne!(tag_key("gay themed"), tag_key("gay-themed"));
    }

    #[test]
    fn an_unstored_layout_is_todays_home_and_rows_are_appended() {
        let layout = HomeLayout::default().resolve(&[7]);
        let keys: Vec<_> = layout.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(&keys[..7], &BUILTIN_HOME[..]);
        assert_eq!(keys[7], "row:7");
    }

    #[test]
    fn resolve_keeps_order_drops_dead_rows_and_appends_missing_builtins() {
        let stored = HomeLayout {
            entries: vec![
                HomeEntry {
                    key: "row:3".into(),
                    hidden: false,
                },
                HomeEntry {
                    key: "recently_added".into(),
                    hidden: true,
                },
                HomeEntry {
                    key: "row:9".into(),
                    hidden: false,
                },
            ],
        };
        let resolved = stored.resolve(&[3, 4]);
        let keys: Vec<_> = resolved.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys[0], "row:3");
        assert_eq!(keys[1], "recently_added");
        assert!(resolved.entries[1].hidden);
        assert!(
            !keys.contains(&"row:9"),
            "a row the viewer cannot see is dropped"
        );
        assert!(keys.contains(&"row:4"), "a newly visible row is appended");
        assert!(keys.contains(&"continue_watching"));
    }

    #[test]
    fn a_row_needs_a_condition_and_bounded_groups() {
        let mut empty = RowSpec::default();
        assert!(empty.validate().is_err());
        let mut ok = RowSpec {
            all_of: vec![Term::Tag(1)],
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let mut big = RowSpec {
            any_of: (0..17).map(Term::Tag).collect(),
            ..Default::default()
        };
        assert!(big.validate().is_err());
    }

    #[test]
    fn spec_json_shape_is_stable() {
        let spec: RowSpec = serde_json::from_str(
            r#"{"all_of":[{"tag":4}],"none_of":[{"genre":"Horror"}],"sort":"shuffle_daily","unwatched":"first"}"#,
        )
        .expect("parse");
        assert_eq!(spec.all_of, vec![Term::Tag(4)]);
        assert_eq!(spec.none_of, vec![Term::Genre("Horror".into())]);
        assert_eq!(spec.sort, RowSort::ShuffleDaily);
        assert!(serde_json::from_str::<RowSpec>(r#"{"bogus":1}"#).is_err());
    }
}
