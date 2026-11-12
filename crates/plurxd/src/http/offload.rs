//! Get it back: asking Curator to download an offloaded title again, and
//! reading how that is going (`docs/INTEGRATION.md`, "Offload").
//!
//! Curator deleted the files to free disk; cinema kept the item, its metadata
//! and its watch history, greyed out. The page's admin presses Get it back and
//! this makes one synchronous server-side call to Curator's webhook (the same
//! `POST /api/v1/webhooks/plurx` and key as the watched notification), with
//! `event: "reacquire"` and the ids the watched event uses: a movie's own
//! TMDB/IMDb ids; for an episode, season or show, the SHOW's TMDB id plus
//! season and episode. Curator matches by ids only and answers
//! `{matched, mediaItemId, state}`.
//!
//! Nothing is stored here: no pending flag, no retry queue. Asking twice is
//! harmless because Curator reports the title's current state, and the status
//! read (`event: "reacquire_status"`) is a pass-through for as long as a page
//! is open. A Curator that answers without `state` predates the feature and
//! the answer says so.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::domain::{Item, ItemKind};
use plurx_core::store::keys;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use crate::state::AppState;

/// How long cinema waits for Curator before saying it cannot be reached.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// What Curator's `state` can say about an offloaded title.
const KNOWN_STATES: &[&str] = &[
    "searching",
    "downloading",
    "importing",
    "on_disk",
    "not_offloaded",
    "not_found",
    "offloading",
    "offloaded",
];

#[derive(Debug, Serialize)]
struct CuratorEvent<'a> {
    event: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmdb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imdb: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode: Option<i32>,
    user: &'a str,
}

#[derive(Debug, Deserialize)]
struct CuratorAnswer {
    #[serde(default)]
    matched: bool,
    #[serde(default, rename = "mediaItemId")]
    media_item_id: Option<serde_json::Value>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// What the page shows.
#[derive(Debug, Serialize, PartialEq)]
pub struct OffloadAnswer {
    /// Curator knows the title.
    pub matched: bool,
    /// `searching` | `downloading` | `importing` | `on_disk` | `not_offloaded`
    /// | `not_found`, as Curator reported it.
    pub state: String,
    /// One sentence for the page.
    pub message: String,
}

fn message_for(state: &str) -> &'static str {
    match state {
        "searching" => "Curator is searching for it.",
        "downloading" => "Curator is downloading it.",
        "importing" => "Curator is importing it.",
        "on_disk" => "Curator has it on disk; it will play here as soon as it is indexed.",
        "not_offloaded" => "Curator does not have this title offloaded.",
        "offloading" => "Curator is still offloading this; try again shortly.",
        "offloaded" => "Curator still has this offloaded; getting it back did not start.",
        "not_found" => "Curator does not know this title.",
        _ => "",
    }
}

/// The ids a title is known by to Curator.
#[derive(Debug, PartialEq)]
struct Ids {
    kind: &'static str,
    tmdb: Option<i64>,
    imdb: Option<String>,
    season: Option<i32>,
    episode: Option<i32>,
}

/// The ids exactly as the watched event spells them.
async fn ids_for(state: &AppState, item: &Item) -> Result<Ids, ApiError> {
    let no_ids = || {
        ApiError::typed(
            StatusCode::CONFLICT,
            "no_external_ids",
            "this title has no TMDB or IMDb id, so Curator cannot be asked for it",
        )
    };
    let show_of = |mut current: Item| async move {
        for _ in 0..4 {
            if current.kind == ItemKind::Show {
                return Ok::<_, ApiError>(Some(current));
            }
            let Some(parent) = current.parent_id else {
                return Ok(None);
            };
            match state.store.get_item(parent).await? {
                Some(parent) => current = parent,
                None => return Ok(None),
            }
        }
        Ok(None)
    };
    match item.kind {
        ItemKind::Movie => {
            if item.tmdb_id.is_none() && item.imdb_id.is_none() {
                return Err(no_ids());
            }
            Ok(Ids {
                kind: "movie",
                tmdb: item.tmdb_id,
                imdb: item.imdb_id.clone(),
                season: None,
                episode: None,
            })
        }
        ItemKind::Show | ItemKind::Season | ItemKind::Episode => {
            let show = show_of(item.clone()).await?.ok_or_else(no_ids)?;
            if show.tmdb_id.is_none() && show.imdb_id.is_none() {
                return Err(no_ids());
            }
            let season = match item.kind {
                ItemKind::Season => item.season_number,
                ItemKind::Episode => match item.parent_id {
                    Some(parent) => state
                        .store
                        .get_item(parent)
                        .await?
                        .and_then(|season| season.season_number),
                    None => None,
                },
                _ => None,
            };
            if item.kind != ItemKind::Show && season.is_none() {
                return Err(no_ids());
            }
            Ok(Ids {
                kind: match item.kind {
                    ItemKind::Show => "show",
                    ItemKind::Season => "season",
                    _ => "episode",
                },
                tmdb: show.tmdb_id,
                imdb: show.imdb_id.clone(),
                season,
                episode: (item.kind == ItemKind::Episode)
                    .then_some(item.episode_number)
                    .flatten(),
            })
        }
        _ => Err(ApiError::typed(
            StatusCode::CONFLICT,
            "not_offloadable",
            "only movies and TV can be offloaded",
        )),
    }
}

/// An item (or, for a container, any leaf under it) must be offloaded for
/// Get it back to mean anything.
async fn require_offloaded(state: &AppState, item: &Item) -> Result<(), ApiError> {
    let offloaded = match item.kind {
        ItemKind::Show | ItemKind::Season => {
            state
                .store
                .offloaded_leaf_counts(&[item.id])
                .await?
                .get(&item.id)
                .copied()
                .unwrap_or(0)
                > 0
        }
        _ => item.offloaded_at.is_some(),
    };
    if offloaded {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::CONFLICT,
            "not_offloaded",
            "this title is not offloaded",
        ))
    }
}

async fn ask_curator(
    state: &AppState,
    event: &str,
    ids: &Ids,
    user: &str,
) -> Result<OffloadAnswer, ApiError> {
    let (url, key) = state
        .store
        .get_setting_pair(keys::MONARR_URL, keys::MONARR_API_KEY)
        .await?;
    let (url, key) = (url.unwrap_or_default(), key.unwrap_or_default());
    if url.is_empty() || key.is_empty() {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "curator_not_configured",
            "Curator is not paired with this server (Settings, Curator)",
        ));
    }
    let url = super::comingsoon::normalize_monarr_url(&url);
    let body = CuratorEvent {
        event,
        kind: ids.kind,
        tmdb: ids.tmdb,
        imdb: ids.imdb.as_deref(),
        season: ids.season,
        episode: ids.episode,
        user,
    };
    let client = reqwest::Client::builder()
        .timeout(CALL_TIMEOUT)
        .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let response = client
        .post(format!("{url}/api/v1/webhooks/plurx"))
        .header("X-Api-Key", key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(target: "plurxd::integrate", %error, "Get it back: Curator unreachable");
            ApiError::typed(
                StatusCode::BAD_GATEWAY,
                "curator_unreachable",
                "Curator cannot be reached right now",
            )
        })?;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ApiError::typed(
            StatusCode::BAD_GATEWAY,
            "curator_refused_key",
            "Curator refused this server's API key",
        ));
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
        return Err(too_old());
    }
    if status.is_client_error() {
        return Err(ApiError::typed(
            StatusCode::BAD_GATEWAY,
            "curator_rejected",
            format!("Curator did not accept the request ({status})"),
        ));
    }
    if !status.is_success() {
        return Err(ApiError::typed(
            StatusCode::BAD_GATEWAY,
            "curator_unreachable",
            format!("Curator answered {status}"),
        ));
    }
    let answer: CuratorAnswer = response.json().await.map_err(|_| too_old())?;
    // A Curator that predates reacquire records the event as a watch and
    // answers without a state.
    let Some(state_name) = answer.state.filter(|s| !s.is_empty()) else {
        return Err(too_old());
    };
    let _ = answer.media_item_id;
    // A state this build does not know is reported as what Curator said and
    // never read as success.
    let message = if KNOWN_STATES.contains(&state_name.as_str()) {
        match (
            state_name.as_str(),
            answer.message.filter(|m| !m.trim().is_empty()),
        ) {
            ("offloaded", Some(curator)) => curator,
            _ => message_for(&state_name).to_owned(),
        }
    } else {
        format!("Curator answered {state_name}.")
    };
    Ok(OffloadAnswer {
        matched: answer.matched,
        message,
        state: state_name,
    })
}

fn too_old() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_GATEWAY,
        "curator_too_old",
        "Curator is too old for Get it back; update it",
    )
}

/// POST /api/v1/items/:id/get-it-back (admin) — ask Curator to download this
/// offloaded movie, episode, season or show again, and answer with what it
/// says. Idempotent on Curator's side.
pub async fn get_it_back(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<OffloadAnswer>, ApiError> {
    let item = state
        .store
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    require_offloaded(&state, &item).await?;
    let ids = ids_for(&state, &item).await?;
    let answer = ask_curator(&state, "reacquire", &ids, &admin.0.username).await?;
    tracing::info!(target: "plurxd::integrate", item = id, state = %answer.state, "Get it back asked");
    Ok(Json(answer))
}

/// GET /api/v1/items/:id/offload-status — what Curator reports for this
/// offloaded title right now, read straight through and never stored.
pub async fn offload_status(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<OffloadAnswer>, ApiError> {
    let item = state
        .store
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    require_offloaded(&state, &item).await?;
    let ids = ids_for(&state, &item).await?;
    Ok(Json(
        ask_curator(&state, "reacquire_status", &ids, &user.username).await?,
    ))
}
