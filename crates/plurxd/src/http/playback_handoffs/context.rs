//! Restricted library context and sequential episode authorization for the player.
use super::{now_unix, ApiError, AppState, HandoffToken};
use axum::{extract::State, http::StatusCode, Json};
use plurx_core::domain::{Item, ItemKind};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct Metadata {
    title: String,
    series_title: Option<String>,
    kind: ItemKind,
    overview: Option<String>,
    year: Option<i32>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    runtime_ms: Option<i64>,
    air_date: Option<String>,
}

#[derive(Serialize)]
pub struct Episode {
    file_id: i64,
    item_id: i64,
    metadata: Metadata,
}

#[derive(Serialize)]
pub struct Context {
    file_id: i64,
    item_id: i64,
    start_ms: i64,
    audio: Option<i64>,
    subtitle: Option<i64>,
    metadata: Metadata,
    next: Option<Episode>,
}

async fn season_and_show(state: &AppState, item: &Item) -> Result<Option<(Item, Item)>, ApiError> {
    if item.kind != ItemKind::Episode {
        return Ok(None);
    }
    let Some(season_id) = item.parent_id else {
        return Ok(None);
    };
    let Some(season) = state.store.get_item(season_id).await? else {
        return Ok(None);
    };
    if season.kind != ItemKind::Season || season.library_id != item.library_id {
        return Ok(None);
    }
    let Some(show_id) = season.parent_id else {
        return Ok(None);
    };
    let Some(show) = state.store.get_item(show_id).await? else {
        return Ok(None);
    };
    if show.kind != ItemKind::Show || show.library_id != item.library_id {
        return Ok(None);
    }
    Ok(Some((season, show)))
}

fn metadata(item: &Item, series_title: Option<String>) -> Metadata {
    Metadata {
        title: item.title.clone(),
        series_title,
        kind: item.kind,
        overview: item.overview.clone(),
        year: item.year,
        season_number: item.season_number,
        episode_number: item.episode_number,
        runtime_ms: item.runtime_ms,
        air_date: item.air_date.clone(),
    }
}

/// Follow the same ordered immediate successor as the web player. Metadata
/// reads never grant access to that file; only the explicit CAS below does.
async fn next_episode(state: &AppState, item: &Item) -> Result<Option<Episode>, ApiError> {
    let Some((season, show)) = season_and_show(state, item).await? else {
        return Ok(None);
    };
    let episodes: Vec<_> = state
        .store
        .get_item_children(season.id)
        .await?
        .into_iter()
        .filter(|child| child.kind == ItemKind::Episode && child.library_id == item.library_id)
        .collect();
    let Some(index) = episodes.iter().position(|episode| episode.id == item.id) else {
        return Ok(None);
    };
    let mut next = episodes.get(index + 1).cloned();
    if next.is_none() {
        let seasons: Vec<_> = state
            .store
            .get_item_children(show.id)
            .await?
            .into_iter()
            .filter(|child| child.kind == ItemKind::Season && child.library_id == item.library_id)
            .collect();
        if let Some(next_season) = seasons
            .iter()
            .position(|entry| entry.id == season.id)
            .and_then(|index| seasons.get(index + 1))
        {
            next = state
                .store
                .get_item_children(next_season.id)
                .await?
                .into_iter()
                .find(|child| {
                    child.kind == ItemKind::Episode && child.library_id == item.library_id
                });
        }
    }
    let Some(next) = next else {
        return Ok(None);
    };
    let files = state.store.files_for_item(next.id).await?;
    let availability = state
        .detail_availability
        .observe_many(
            &files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
        )
        .await;
    // Preserve the usual quality order, skipping only confirmed missing
    // copies. A bounded unknown remains eligible; the source open decides.
    let Some((file, _)) = files
        .into_iter()
        .zip(availability)
        .find(|(_, observation)| observation.available())
    else {
        return Ok(None);
    };
    Ok(Some(Episode {
        file_id: file.id,
        item_id: next.id,
        metadata: metadata(&next, Some(show.title)),
    }))
}

async fn for_file(state: &AppState, file_id: i64) -> Result<Context, ApiError> {
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let item = state
        .store
        .get_item(file.item_id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    let series = season_and_show(state, &item)
        .await?
        .map(|(_, show)| show.title);
    let next = next_episode(state, &item).await?;
    Ok(Context {
        file_id,
        item_id: item.id,
        start_ms: 0,
        audio: None,
        subtitle: None,
        metadata: metadata(&item, series),
        next,
    })
}

/// GET /api/v1/playback-handoffs/self/context — no arbitrary item id is accepted.
pub async fn context(
    handoff: HandoffToken,
    State(state): State<AppState>,
) -> Result<Json<Context>, ApiError> {
    Ok(Json(for_file(&state, handoff.scope.file_id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextRequest {
    expected_file_id: i64,
}

fn stale() -> ApiError {
    ApiError::typed(
        StatusCode::CONFLICT,
        "handoff_stale",
        "The player has already changed episodes.",
    )
}

/// POST /api/v1/playback-handoffs/self/next — the server, never the request,
/// chooses the next file. The existing token retains its expiry and minter.
pub async fn advance_next(
    handoff: HandoffToken,
    State(state): State<AppState>,
    Json(request): Json<NextRequest>,
) -> Result<Json<Context>, ApiError> {
    if handoff.scope.file_id != request.expected_file_id {
        return Err(stale());
    }
    let current = for_file(&state, handoff.scope.file_id).await?;
    let next = current.next.ok_or(ApiError::NotFound("next episode"))?;
    // Prepare the response before changing authority, so a failed metadata
    // read cannot leave the player uncertain which file it may open.
    let response = for_file(&state, next.file_id).await?;
    if !state
        .store
        .advance_playback_handoff(
            &handoff.digest,
            request.expected_file_id,
            next.file_id,
            now_unix(),
        )
        .await?
    {
        if state
            .store
            .playback_handoff_token_scope(&handoff.digest, now_unix())
            .await?
            .is_none()
        {
            return Err(ApiError::Unauthorized);
        }
        return Err(stale());
    }
    Ok(Json(response))
}
