//! Item annotations that are not catalogue metadata: review-site ratings an
//! item carries, and the per-person lists (watchlist, favorites) it is on.

use serde::{Deserialize, Serialize};

/// Where a score comes from. The order of the variants is the order clients
/// show them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RatingSource {
    Imdb,
    RottenTomatoes,
    Metacritic,
    Tmdb,
}

impl RatingSource {
    pub const ALL: [Self; 4] = [
        Self::Imdb,
        Self::RottenTomatoes,
        Self::Metacritic,
        Self::Tmdb,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Imdb => "imdb",
            Self::RottenTomatoes => "rotten_tomatoes",
            Self::Metacritic => "metacritic",
            Self::Tmdb => "tmdb",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == value)
    }
}

/// One score, on the scale its source publishes: IMDb out of 10, the others
/// as a percentage or out of 100.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rating {
    pub source: RatingSource,
    pub value: f64,
    /// 10 or 100.
    pub scale: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<i64>,
}

/// Everything known about an item's scores, and when it was last asked for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemRatings {
    pub fetched_at_ms: i64,
    /// Whether OMDb (IMDb, Rotten Tomatoes, Metacritic) answered this fetch.
    /// A fetch made with no OMDb key, or while OMDb refused, says `false`, so
    /// configuring a key later is reason enough to ask again.
    pub omdb_consulted: bool,
    /// At most one per source, in [`RatingSource`] order.
    pub ratings: Vec<Rating>,
}

/// A per-person list an item can be put on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemList {
    Watchlist,
    Favorite,
}

impl ItemList {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watchlist => "watchlist",
            Self::Favorite => "favorite",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "watchlist" => Some(Self::Watchlist),
            "favorite" | "favorites" => Some(Self::Favorite),
            _ => None,
        }
    }
}

/// Which of a person's lists one item is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ItemListFlags {
    pub watchlist: bool,
    pub favorite: bool,
    /// When the item went on the watchlist, if it is on it.
    pub watchlist_added_at_ms: Option<i64>,
}

/// One entry of a person's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListedItem {
    pub item_id: i64,
    pub added_at_ms: i64,
}
