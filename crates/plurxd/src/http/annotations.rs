//! Item annotations: review-site scores and a person's watchlist and
//! favorites. Kept off `GET /items/{id}` and `GET /hubs` on purpose: those
//! pages have a read budget, and a score may need a provider round trip. A
//! client asks for these beside the page and paints them when they land.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::annotations::{ItemList, Rating};
use plurx_core::domain::{Item, ItemKind, WatchRollup, WatchState};
use plurx_core::metadata::ratings::{
    fetch_item_ratings, needs_refresh, rated_kind, RatingProviders,
};
use plurx_core::metadata::{OmdbClient, TmdbClient};
use plurx_core::store::keys;
use serde::{Deserialize, Serialize};

use super::dto::ItemDto;
use super::error::ApiError;
use super::extract::AuthUser;
use crate::state::AppState;

/// The longest an item page waits on providers before it is answered with
/// what is stored. The fetch itself is not abandoned early by this; its own
/// provider deadlines bound it.
const FETCH_WAIT: Duration = Duration::from_secs(8);
/// The longest list a client is handed in one read.
const LIST_MAX: i64 = 500;

/// Items whose scores are being fetched on this node right now, so a page
/// opened twice asks the providers once.
#[derive(Clone, Default)]
pub struct RatingFetches(Arc<Mutex<HashSet<i64>>>);

impl RatingFetches {
    fn claim(&self, item_id: i64) -> Option<RatingFetchClaim> {
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(item_id).then(|| RatingFetchClaim {
            fetches: self.clone(),
            item_id,
        })
    }
}

struct RatingFetchClaim {
    fetches: RatingFetches,
    item_id: i64,
}

impl Drop for RatingFetchClaim {
    fn drop(&mut self) {
        self.fetches
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.item_id);
    }
}

#[derive(Serialize, Debug)]
pub struct ItemAnnotations {
    /// At most one per source, in display order: imdb, rotten_tomatoes,
    /// metacritic, tmdb. Only sources with a score appear.
    pub ratings: Vec<Rating>,
    /// On the reader's watchlist and not finished since it was put there.
    pub watchlist: bool,
    pub favorite: bool,
}

/// Where the score providers are reached: their public APIs, or a mock
/// server in tests.
#[derive(Clone, Default)]
pub struct ProviderEndpoints {
    tmdb: Option<String>,
    omdb: Option<String>,
}

impl ProviderEndpoints {
    #[cfg(test)]
    pub fn at(tmdb: &str, omdb: &str) -> Self {
        Self {
            tmdb: Some(tmdb.to_owned()),
            omdb: Some(omdb.to_owned()),
        }
    }

    fn tmdb(&self, key: String) -> TmdbClient {
        match &self.tmdb {
            Some(base) => TmdbClient::new(key).with_base(base, base),
            None => TmdbClient::new(key),
        }
    }

    fn omdb(&self, key: String) -> OmdbClient {
        match &self.omdb {
            Some(base) => OmdbClient::new(key).with_base(base),
            None => OmdbClient::new(key),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Only things a person picks to watch go on these lists.
fn listable(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Movie | ItemKind::Show)
}

/// Whether a watchlist entry has been finished since it was added: a movie
/// watched after it went on the list, or a show whose every episode is
/// watched. Derived at read time rather than deleted by the many paths that
/// mark things watched (native, Plex and Jellyfin scrobbles, Trakt, "mark
/// watched"), so none of them can forget to.
fn finished_since(
    kind: ItemKind,
    added_at_ms: i64,
    watch: Option<&WatchState>,
    rollup: Option<&WatchRollup>,
) -> bool {
    match kind {
        ItemKind::Show => rollup.is_some_and(|r| r.complete()),
        _ => watch.is_some_and(|w| w.watched && w.updated_at.saturating_mul(1000) >= added_at_ms),
    }
}

async fn stored_or_fetched_ratings(state: &AppState, item: &Item) -> Result<Vec<Rating>, ApiError> {
    if !rated_kind(item.kind) {
        return Ok(Vec::new());
    }
    let cached = state.store.item_ratings(item.id).await?;
    let settings = state
        .store
        .get_settings(&[keys::TMDB_API_KEY, keys::OMDB_API_KEY])
        .await?;
    let key = |name: &str| {
        settings
            .get(name)
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty())
    };
    let (tmdb_key, omdb_key) = (key(keys::TMDB_API_KEY), key(keys::OMDB_API_KEY));
    let now = now_ms();
    let wanted = (tmdb_key.is_some() || omdb_key.is_some())
        && needs_refresh(cached.as_ref(), omdb_key.is_some(), now);
    let claim = if wanted {
        state.rating_fetches.claim(item.id)
    } else {
        None
    };
    let Some(_claim) = claim else {
        return Ok(cached.map(|c| c.ratings).unwrap_or_default());
    };
    let tmdb = tmdb_key.map(|k| state.provider_endpoints.tmdb(k));
    let omdb = omdb_key.map(|k| state.provider_endpoints.omdb(k));
    let providers = RatingProviders {
        tmdb: tmdb.as_ref(),
        omdb: omdb.as_ref(),
    };
    let fetched = tokio::time::timeout(
        FETCH_WAIT,
        fetch_item_ratings(item.into(), providers, cached.as_ref(), now),
    )
    .await;
    match fetched {
        Ok(Ok(fresh)) => {
            state.store.put_item_ratings(item.id, &fresh).await?;
            Ok(fresh.ratings)
        }
        Ok(Err(error)) => {
            tracing::info!(item_id = item.id, %error, "ratings fetch failed; serving stored scores");
            Ok(cached.map(|c| c.ratings).unwrap_or_default())
        }
        Err(_) => {
            tracing::info!(
                item_id = item.id,
                "ratings fetch outlasted the page; serving stored scores"
            );
            Ok(cached.map(|c| c.ratings).unwrap_or_default())
        }
    }
}

/// GET /api/v1/items/{id}/annotations — scores and the reader's list flags.
pub async fn item_annotations(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ItemAnnotations>, ApiError> {
    let item = state
        .catalogue
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    let flags = state.store.item_list_flags(user.id, id).await?;
    let watchlist = if flags.watchlist {
        let added = state
            .store
            .listed_items(user.id, ItemList::Watchlist, LIST_MAX, 0)
            .await?
            .into_iter()
            .find(|entry| entry.item_id == id)
            .map_or(0, |entry| entry.added_at_ms);
        let (watch, rollup) = progress_of(&state, user.id, std::slice::from_ref(&item)).await?;
        !finished_since(item.kind, added, watch.get(&id), rollup.get(&id))
    } else {
        false
    };
    let ratings = stored_or_fetched_ratings(&state, &item).await?;
    Ok(Json(ItemAnnotations {
        ratings,
        watchlist,
        favorite: flags.favorite,
    }))
}

async fn progress_of(
    state: &AppState,
    user_id: i64,
    items: &[Item],
) -> Result<(HashMap<i64, WatchState>, HashMap<i64, WatchRollup>), ApiError> {
    let leaves: Vec<i64> = items
        .iter()
        .filter(|i| i.kind != ItemKind::Show)
        .map(|i| i.id)
        .collect();
    let shows: Vec<i64> = items
        .iter()
        .filter(|i| i.kind == ItemKind::Show)
        .map(|i| i.id)
        .collect();
    let watch = if leaves.is_empty() {
        HashMap::new()
    } else {
        state
            .store
            .watch_map(user_id, &leaves)
            .await?
            .into_iter()
            .collect()
    };
    let rollups = if shows.is_empty() {
        HashMap::new()
    } else {
        state.store.watch_rollups(user_id, &shows).await?
    };
    Ok((watch, rollups))
}

fn parse_list(list: &str) -> Result<ItemList, ApiError> {
    ItemList::parse(list).ok_or(ApiError::NotFound("list"))
}

async fn set_listed(
    state: &AppState,
    user_id: i64,
    id: i64,
    list: ItemList,
    listed: bool,
) -> Result<StatusCode, ApiError> {
    let item = state
        .catalogue
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if listed && !listable(item.kind) {
        return Err(ApiError::BadRequest(
            "only movies and shows go on a watchlist or favorites".into(),
        ));
    }
    state
        .store
        .set_item_listed(user_id, id, list, listed, now_ms())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// PUT /api/v1/items/{id}/watchlist — put it on the reader's watchlist.
pub async fn add_to_watchlist(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    set_listed(&state, user.id, id, ItemList::Watchlist, true).await
}

/// DELETE /api/v1/items/{id}/watchlist — take it off.
pub async fn remove_from_watchlist(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    set_listed(&state, user.id, id, ItemList::Watchlist, false).await
}

/// PUT /api/v1/items/{id}/favorite — mark it a favorite of the reader's.
pub async fn add_to_favorites(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    set_listed(&state, user.id, id, ItemList::Favorite, true).await
}

/// DELETE /api/v1/items/{id}/favorite — unmark it.
pub async fn remove_from_favorites(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    set_listed(&state, user.id, id, ItemList::Favorite, false).await
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub items: Vec<ItemDto>,
}

/// GET /api/v1/lists/{list} — the reader's watchlist or favorites, most
/// recently added first. Finished watchlist entries are left out (see
/// [`finished_since`]); they come back if the person adds them again.
pub async fn list_items(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(list): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let list = parse_list(&list)?;
    let limit = q.limit.unwrap_or(LIST_MAX).clamp(1, LIST_MAX);
    let entries = state.store.listed_items(user.id, list, LIST_MAX, 0).await?;
    let mut items = Vec::with_capacity(entries.len());
    for entry in &entries {
        if let Some(item) = state.catalogue.get_item(entry.item_id).await? {
            items.push((item, entry.added_at_ms));
        }
    }
    let plain: Vec<Item> = items.iter().map(|(item, _)| item.clone()).collect();
    let (watch, rollups) = progress_of(&state, user.id, &plain).await?;
    let items = items
        .into_iter()
        .filter(|(item, added)| {
            list != ItemList::Watchlist
                || !finished_since(
                    item.kind,
                    *added,
                    watch.get(&item.id),
                    rollups.get(&item.id),
                )
        })
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|(item, _)| {
            let id = item.id;
            ItemDto::from(item)
                .with_watch(watch.get(&id).copied())
                .with_rollup(rollups.get(&id).copied())
        })
        .collect();
    Ok(Json(ListResponse { items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watched(at_s: i64) -> WatchState {
        WatchState {
            position_ms: 0,
            duration_ms: None,
            watched: true,
            updated_at: at_s,
        }
    }

    #[test]
    fn a_movie_leaves_the_watchlist_when_watched_after_it_was_added() {
        let movie = ItemKind::Movie;
        assert!(finished_since(movie, 10_000, Some(&watched(20)), None));
        // Watched before it was (re)added: it stays.
        assert!(!finished_since(movie, 30_000, Some(&watched(20)), None));
        assert!(!finished_since(movie, 10_000, None, None));
    }

    #[test]
    fn a_show_leaves_the_watchlist_when_every_episode_is_watched() {
        let show = ItemKind::Show;
        assert!(finished_since(
            show,
            0,
            None,
            Some(&WatchRollup {
                leaves: 8,
                watched: 8
            })
        ));
        assert!(!finished_since(
            show,
            0,
            None,
            Some(&WatchRollup {
                leaves: 8,
                watched: 7
            })
        ));
        assert!(!finished_since(
            show,
            0,
            None,
            Some(&WatchRollup {
                leaves: 0,
                watched: 0
            })
        ));
    }
}
