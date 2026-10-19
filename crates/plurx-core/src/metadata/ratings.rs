//! Review-site scores for an item: TMDB's own user score, and IMDb, Rotten
//! Tomatoes and Metacritic through OMDb. A cache the item page fills on
//! demand, not catalogue metadata: nothing here touches the item's row.

use super::omdb::OmdbClient;
use super::tmdb::TmdbClient;
use crate::annotations::{ItemRatings, Rating, RatingSource};
use crate::domain::{Item, ItemKind};

/// What a fetch needs to know about the title it scores.
#[derive(Debug, Clone, Copy)]
pub struct RatedTitle<'a> {
    pub kind: ItemKind,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<&'a str>,
}

impl<'a> From<&'a Item> for RatedTitle<'a> {
    fn from(item: &'a Item) -> Self {
        Self {
            kind: item.kind,
            tmdb_id: item.tmdb_id,
            imdb_id: item.imdb_id.as_deref(),
        }
    }
}
use crate::error::MetadataError;

/// How long a fetch stands before an item page asks again. Scores move
/// slowly, and OMDb's free key allows 1,000 requests a day for a whole
/// household's browsing.
pub const RATINGS_TTL_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// How soon to retry OMDb after it refused (exhausted allowance, bad key) or
/// was not configured when a key now is.
pub const OMDB_RETRY_MS: i64 = 60 * 60 * 1000;

/// Which providers a fetch may ask.
#[derive(Clone, Copy, Default)]
pub struct RatingProviders<'a> {
    pub tmdb: Option<&'a TmdbClient>,
    pub omdb: Option<&'a OmdbClient>,
}

/// Only movies and shows carry review-site scores.
#[must_use]
pub fn rated_kind(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Movie | ItemKind::Show)
}

/// Whether a stored answer should be asked for again.
#[must_use]
pub fn needs_refresh(cached: Option<&ItemRatings>, omdb_configured: bool, now_ms: i64) -> bool {
    let Some(cached) = cached else {
        return true;
    };
    let age = now_ms.saturating_sub(cached.fetched_at_ms);
    age >= RATINGS_TTL_MS || (omdb_configured && !cached.omdb_consulted && age >= OMDB_RETRY_MS)
}

/// Ask every configured provider once. A provider that fails keeps what the
/// previous fetch had from it, so one bad answer never erases a score; when
/// every provider asked fails, nothing is returned to store.
pub async fn fetch_item_ratings(
    item: RatedTitle<'_>,
    providers: RatingProviders<'_>,
    previous: Option<&ItemRatings>,
    now_ms: i64,
) -> Result<ItemRatings, MetadataError> {
    let carried = |sources: &[RatingSource]| -> Vec<Rating> {
        previous
            .map(|p| {
                p.ratings
                    .iter()
                    .filter(|r| sources.contains(&r.source))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let omdb_sources = [
        RatingSource::Imdb,
        RatingSource::RottenTomatoes,
        RatingSource::Metacritic,
    ];
    let mut asked = 0_u8;
    let mut failures = 0_u8;
    let mut failed: Option<MetadataError> = None;
    let mut ratings = Vec::new();
    let mut imdb_id = item.imdb_id.map(ToOwned::to_owned);

    match (providers.tmdb, item.tmdb_id) {
        (Some(tmdb), Some(tmdb_id)) => {
            asked += 1;
            match tmdb.score(item.kind, tmdb_id).await {
                Ok(score) => {
                    imdb_id = imdb_id.or(score.imdb_id.clone());
                    ratings.extend(score.rating());
                }
                Err(error) => {
                    ratings.extend(carried(&[RatingSource::Tmdb]));
                    failures += 1;
                    failed = Some(error);
                }
            }
        }
        _ => ratings.extend(carried(&[RatingSource::Tmdb])),
    }

    let mut omdb_consulted = previous.is_some_and(|p| p.omdb_consulted);
    match (providers.omdb, imdb_id.as_deref()) {
        (Some(omdb), Some(imdb_id)) => {
            asked += 1;
            match omdb.scores(imdb_id).await {
                Ok(scores) => {
                    omdb_consulted = true;
                    ratings.extend(scores);
                }
                Err(error) => {
                    ratings.extend(carried(&omdb_sources));
                    failures += 1;
                    failed = Some(error);
                }
            }
        }
        // Configured, but no IMDb id to ask with: OMDb has been answered as
        // well as it can be.
        (Some(_), None) => omdb_consulted = true,
        (None, _) => ratings.extend(carried(&omdb_sources)),
    }

    if asked == 0 {
        return Err(MetadataError::NotConfigured);
    }
    if let Some(error) = failed {
        if failures == asked {
            return Err(error);
        }
    }
    ratings.sort_by_key(|rating| rating.source);
    ratings.dedup_by_key(|rating| rating.source);
    Ok(ItemRatings {
        fetched_at_ms: now_ms,
        omdb_consulted,
        ratings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating(source: RatingSource, value: f64) -> Rating {
        Rating {
            source,
            value,
            scale: if source == RatingSource::Imdb {
                10
            } else {
                100
            },
            votes: None,
        }
    }

    #[test]
    fn a_stored_answer_is_kept_until_it_ages_or_omdb_becomes_askable() {
        let fresh = ItemRatings {
            fetched_at_ms: 1_000,
            omdb_consulted: true,
            ratings: vec![rating(RatingSource::Tmdb, 78.0)],
        };
        assert!(needs_refresh(None, false, 0));
        assert!(!needs_refresh(
            Some(&fresh),
            true,
            1_000 + RATINGS_TTL_MS - 1
        ));
        assert!(needs_refresh(Some(&fresh), true, 1_000 + RATINGS_TTL_MS));
        let without_omdb = ItemRatings {
            omdb_consulted: false,
            ..fresh
        };
        assert!(!needs_refresh(
            Some(&without_omdb),
            false,
            1_000 + OMDB_RETRY_MS
        ));
        assert!(!needs_refresh(
            Some(&without_omdb),
            true,
            1_000 + OMDB_RETRY_MS - 1
        ));
        assert!(needs_refresh(
            Some(&without_omdb),
            true,
            1_000 + OMDB_RETRY_MS
        ));
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    fn show(tmdb_id: i64) -> RatedTitle<'static> {
        RatedTitle {
            kind: ItemKind::Show,
            tmdb_id: Some(tmdb_id),
            imdb_id: None,
        }
    }

    /// A show matched by search has no IMDb id of its own; TMDB's external
    /// ids supply it, and OMDb is asked with that. When OMDb later refuses
    /// (its daily allowance), the scores it gave before are kept.
    #[tokio::test]
    async fn a_fetch_asks_tmdb_then_omdb_and_keeps_omdb_scores_through_a_refusal() {
        use axum::routing::get;
        use serde_json::json;
        let tmdb = serve(axum::Router::new().route(
            "/tv/{id}",
            get(|| async {
                axum::Json(json!({"vote_average": 8.4, "vote_count": 1200, "external_ids": {"imdb_id": "tt7654321"}}))
            }),
        ))
        .await;
        let refuse = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let omdb = {
            let refuse = std::sync::Arc::clone(&refuse);
            serve(axum::Router::new().route(
                "/",
                get(move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| {
                    let refuse = refuse.load(std::sync::atomic::Ordering::SeqCst);
                    async move {
                        if refuse {
                            return axum::Json(json!({"Response": "False", "Error": "Request limit reached!"}));
                        }
                        assert_eq!(q.get("i").map(String::as_str), Some("tt7654321"));
                        axum::Json(json!({"Response": "True", "imdbRating": "8.6", "imdbVotes": "1,234", "Metascore": "N/A",
                            "Ratings": [{"Source": "Rotten Tomatoes", "Value": "91%"}]}))
                    }
                }),
            ))
            .await
        };
        let tmdb = TmdbClient::new("key").with_base(&tmdb, &tmdb);
        let omdb = OmdbClient::new("key").with_base(format!("{omdb}/"));
        let providers = RatingProviders {
            tmdb: Some(&tmdb),
            omdb: Some(&omdb),
        };
        let first = fetch_item_ratings(show(42), providers, None, 5_000)
            .await
            .expect("fetch");
        assert!(first.omdb_consulted);
        assert_eq!(first.fetched_at_ms, 5_000);
        assert_eq!(
            first
                .ratings
                .iter()
                .map(|r| (r.source, r.value))
                .collect::<Vec<_>>(),
            vec![
                (RatingSource::Imdb, 8.6),
                (RatingSource::RottenTomatoes, 91.0),
                (RatingSource::Tmdb, 84.0)
            ]
        );

        refuse.store(true, std::sync::atomic::Ordering::SeqCst);
        let second = fetch_item_ratings(show(42), providers, Some(&first), 9_000)
            .await
            .expect("TMDB still answered");
        assert_eq!(
            second.ratings, first.ratings,
            "OMDb's scores survive its refusal"
        );
        assert!(second.omdb_consulted);
        assert_eq!(second.fetched_at_ms, 9_000);

        let only_omdb = RatingProviders {
            tmdb: None,
            omdb: Some(&omdb),
        };
        let movie = RatedTitle {
            kind: ItemKind::Movie,
            tmdb_id: Some(42),
            imdb_id: Some("tt7654321"),
        };
        assert!(
            fetch_item_ratings(movie, only_omdb, None, 9_000)
                .await
                .is_err(),
            "every asked provider failed: nothing to store"
        );
        assert!(matches!(
            fetch_item_ratings(movie, RatingProviders::default(), None, 9_000).await,
            Err(MetadataError::NotConfigured)
        ));
    }

    #[test]
    fn only_movies_and_shows_are_rated() {
        assert!(rated_kind(ItemKind::Movie));
        assert!(rated_kind(ItemKind::Show));
        assert!(!rated_kind(ItemKind::Episode));
        assert!(!rated_kind(ItemKind::Season));
    }
}
