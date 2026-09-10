//! Authenticated Library-channel definitions, deterministic guides, and tune.
//!
//! This module owns orchestration only. Recipe matching and schedule math live
//! in `plurx_core::library_channels`; streaming remains the finite-HLS service.

use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use plurx_core::library_channels::{
    build_rotation, evaluate_recipe, generation_loop_duration, resolve_occurrence,
    ChannelBuildMutation, ChannelGenerationEntry, ChannelMatch, ChannelMutation, ChannelVisibility,
    LibraryChannel, LibraryChannelBuildClaim, LibraryChannelPublication, LibraryChannelRecipe,
    LibraryChannelUpdate, NewLibraryChannel, ResolvedOccurrence, CHANNEL_ACTIVATION_LEAD_MS,
    CHANNEL_CANDIDATE_PAGE, CHANNEL_CANDIDATE_ROWS_MAX, CHANNEL_DESCRIPTION_MAX,
    CHANNEL_GENERATION_STAGE_MAX, CHANNEL_GUIDE_CHANNELS_MAX, CHANNEL_GUIDE_OCCURRENCES_MAX,
    CHANNEL_NAME_MAX, CHANNEL_PREVIEW_PAGE_DEFAULT, CHANNEL_PREVIEW_PAGE_MAX,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::ApiError;
use super::extract::AuthUser;
use super::hls;
use crate::state::AppState;

const GUIDE_DEFAULT_MS: i64 = 90 * 60 * 1_000;
const GUIDE_MAX_MS: i64 = 24 * 60 * 60 * 1_000;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        // Collection names are registered before `/{id}` by construction.
        .route("/", get(list).post(create))
        .route("/preview", post(preview))
        .route("/guide", get(guide))
        .route("/{id}", get(get_one).put(update).delete(delete_one))
        .route("/{id}/rebuild", post(rebuild))
        .route("/{id}/build", get(build_state))
        .route("/{id}/favourite", put(favourite))
        .route("/{id}/resolve", post(resolve_now))
        .route("/{id}/sessions", post(start_session))
        .layer(middleware::from_fn(private_no_store))
}

async fn private_no_store(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryChannelPlaybackPurpose {
    pub channel_id: String,
    pub generation_id: String,
    pub cycle: i64,
    pub ordinal: u32,
    pub tune_sequence: u64,
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
}

impl LibraryChannelPlaybackPurpose {
    pub(crate) fn bind_session_fingerprint(&self, finite_fingerprint: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(b"plurx.library-channel.session.v1\0");
        digest.update(finite_fingerprint.as_bytes());
        digest.update(serde_json::to_vec(self).expect("bounded purpose serializes"));
        hex::encode(digest.finalize())
    }
}

#[derive(Deserialize)]
struct ListQuery {
    after: Option<String>,
    limit: Option<usize>,
    management: Option<bool>,
}

#[derive(Serialize)]
struct ChannelSummary {
    #[serde(flatten)]
    channel: LibraryChannel,
    source: &'static str,
    can_edit: bool,
    now: Option<Programme>,
    next: Option<Programme>,
}

async fn list(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ChannelSummary>>, ApiError> {
    let management = query.management.unwrap_or(false);
    if management && !user.is_admin {
        return Err(ApiError::Forbidden);
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 100) as i64;
    let channels = state
        .store
        .list_library_channels(
            user.id,
            user.is_admin,
            management,
            query.after.as_deref(),
            limit,
        )
        .await?;
    let now_ms = crate::media_sessions::unix_ms();
    let mut response = Vec::with_capacity(channels.len());
    for channel in channels {
        let now = programme_at(&state, &user, &channel, now_ms)
            .await
            .ok()
            .flatten();
        let next = match now.as_ref() {
            Some(current) => programme_at(&state, &user, &channel, current.ends_at_ms)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        response.push(ChannelSummary {
            can_edit: channel.owner_user_id == user.id || user.is_admin,
            channel,
            source: "library",
            now,
            next,
        });
    }
    Ok(Json(response))
}

#[derive(Debug, Deserialize, Serialize)]
struct DefinitionBody {
    request_id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    visibility: Option<ChannelVisibility>,
    #[serde(default)]
    enabled: bool,
    recipe: LibraryChannelRecipe,
}

#[derive(Debug, Deserialize, Serialize)]
struct UpdateBody {
    expected_revision: i64,
    #[serde(flatten)]
    definition: DefinitionBody,
}

#[derive(Serialize)]
struct MutationResponse {
    channel: LibraryChannel,
    build_state: &'static str,
}

async fn create(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<DefinitionBody>,
) -> Result<(StatusCode, Json<MutationResponse>), ApiError> {
    let (name, description, recipe, visibility) = normalized_definition(&body, user.is_admin)?;
    let candidates = matching_catalogue(&state, &recipe).await?;
    if body.enabled && candidates.is_empty() {
        return Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_empty",
            "an enabled channel needs at least one eligible title",
        ));
    }
    let now_ms = crate::media_sessions::unix_ms();
    let channel = NewLibraryChannel {
        id: uuid::Uuid::new_v4().to_string(),
        owner_user_id: user.id,
        request_id: valid_request_id(&body.request_id)?,
        request_hash: body_digest(&body),
        name,
        description,
        visibility,
        enabled: body.enabled,
        recipe,
        seed: new_seed(),
        now_ms,
    };
    let (stored, replay) = match state
        .store
        .create_library_channel(user.is_admin, &channel)
        .await?
    {
        ChannelMutation::Applied(channel) => (channel, false),
        ChannelMutation::Replay(channel) => (channel, true),
        other => return Err(mutation_error(other)),
    };
    let build_state = if replay || candidates.is_empty() {
        if stored.active_generation_id.is_some() {
            "ready"
        } else {
            "draft"
        }
    } else {
        build_channel(&state, &user, &stored, candidates, Activation::Initial).await?;
        "ready"
    };
    let refreshed = state
        .store
        .get_library_channel(user.id, user.is_admin, &stored.id)
        .await?
        .ok_or(ApiError::NotFound("library channel"))?;
    Ok((
        if replay {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(MutationResponse {
            channel: refreshed,
            build_state,
        }),
    ))
}

async fn get_one(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChannelDetail>, ApiError> {
    let channel = visible_channel(&state, &user, &id).await?;
    Ok(Json(ChannelDetail::new(channel, user.id, user.is_admin)))
}

#[derive(Serialize)]
struct ChannelDetail {
    #[serde(flatten)]
    channel: LibraryChannel,
    source: &'static str,
    can_edit: bool,
    can_delete: bool,
    can_share: bool,
}

impl ChannelDetail {
    fn new(channel: LibraryChannel, user_id: i64, admin: bool) -> Self {
        let editable = channel.owner_user_id == user_id || admin;
        Self {
            channel,
            source: "library",
            can_edit: editable,
            can_delete: editable,
            can_share: admin,
        }
    }
}

async fn update(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Result<(StatusCode, Json<MutationResponse>), ApiError> {
    let previous = visible_channel(&state, &user, &id).await?;
    let (name, description, recipe, visibility) =
        normalized_definition(&body.definition, user.is_admin)?;
    let candidates = matching_catalogue(&state, &recipe).await?;
    if body.definition.enabled && candidates.is_empty() {
        return Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_empty",
            "an enabled channel needs at least one eligible title",
        ));
    }
    let update = LibraryChannelUpdate {
        channel_id: id,
        actor_user_id: user.id,
        actor_is_admin: user.is_admin,
        expected_revision: body.expected_revision,
        request_id: valid_request_id(&body.definition.request_id)?,
        request_hash: body_digest(&body),
        name,
        description,
        visibility,
        enabled: body.definition.enabled,
        recipe,
        seed: previous.seed,
        now_ms: crate::media_sessions::unix_ms(),
    };
    let (stored, replay) = match state.store.update_library_channel(&update).await? {
        ChannelMutation::Applied(channel) => (channel, false),
        ChannelMutation::Replay(channel) => (channel, true),
        other => return Err(mutation_error(other)),
    };
    let has_candidates = !candidates.is_empty();
    if !replay && has_candidates {
        build_channel(&state, &user, &stored, candidates, Activation::NextRotation).await?;
    }
    let refreshed = visible_channel(&state, &user, &stored.id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(MutationResponse {
            channel: refreshed,
            build_state: if has_candidates { "ready" } else { "draft" },
        }),
    ))
}

#[derive(Deserialize)]
struct DeleteQuery {
    expected_revision: i64,
}

async fn delete_one(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    match state
        .store
        .delete_library_channel(user.id, user.is_admin, &id, query.expected_revision)
        .await?
    {
        ChannelMutation::Applied(()) => Ok(StatusCode::NO_CONTENT),
        other => Err(mutation_error(other)),
    }
}

#[derive(Deserialize)]
struct FavouriteBody {
    favourite: bool,
}

async fn favourite(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FavouriteBody>,
) -> Result<StatusCode, ApiError> {
    visible_channel(&state, &user, &id).await?;
    if state
        .store
        .set_library_channel_favourite(
            user.id,
            &id,
            body.favourite,
            crate::media_sessions::unix_ms(),
        )
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("library channel"))
    }
}

#[derive(Deserialize)]
struct PreviewBody {
    recipe: LibraryChannelRecipe,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct PreviewResponse {
    eligible_count: usize,
    unique_duration_ms: i64,
    repeat_description: String,
    content_digest: String,
    matches: Vec<ChannelMatch>,
    first_ten: Vec<ChannelGenerationEntry>,
}

async fn preview(
    AuthUser(_user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<PreviewBody>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let recipe = body.recipe.normalize().map_err(invalid_recipe)?;
    let matches = matching_catalogue(&state, &recipe).await?;
    let candidates = matches
        .iter()
        .map(|matched| matched.candidate.clone())
        .collect::<Vec<_>>();
    let rotation = if candidates.is_empty() {
        Vec::new()
    } else {
        build_rotation(candidates, recipe.ordering, [0; 32]).map_err(schedule_error)?
    };
    let duration = rotation
        .last()
        .map(|entry| entry.cumulative_start_ms + entry.duration_ms)
        .unwrap_or(0);
    let limit = body
        .limit
        .unwrap_or(CHANNEL_PREVIEW_PAGE_DEFAULT)
        .clamp(1, CHANNEL_PREVIEW_PAGE_MAX);
    Ok(Json(PreviewResponse {
        eligible_count: matches.len(),
        unique_duration_ms: duration,
        repeat_description: repeat_description(duration),
        content_digest: content_digest(&recipe, &rotation),
        matches: matches.into_iter().take(limit).collect(),
        first_ten: rotation.into_iter().take(10).collect(),
    }))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Activation {
    Initial,
    NextRotation,
    NextProgramme,
}

#[derive(Deserialize)]
struct RebuildBody {
    expected_revision: i64,
    request_id: String,
    activation: Activation,
    #[serde(default)]
    reshuffle: bool,
}

async fn rebuild(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RebuildBody>,
) -> Result<(StatusCode, Json<BuildDto>), ApiError> {
    valid_request_id(&body.request_id)?;
    let mut channel = visible_channel(&state, &user, &id).await?;
    if channel.owner_user_id != user.id && !user.is_admin {
        return Err(ApiError::Forbidden);
    }
    if channel.revision != body.expected_revision {
        return Err(channel_error(
            StatusCode::CONFLICT,
            "channel_revision_changed",
            "the channel changed; reload before rebuilding",
        ));
    }
    if body.reshuffle {
        channel.seed = new_seed();
    }
    let matches = matching_catalogue(&state, &channel.recipe).await?;
    if matches.is_empty() {
        return Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_empty",
            "the current recipe has no eligible titles; the old schedule was retained",
        ));
    }
    build_channel(&state, &user, &channel, matches, body.activation).await?;
    let channel = visible_channel(&state, &user, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(BuildDto::from(&channel))))
}

#[derive(Serialize)]
struct BuildDto {
    state: &'static str,
    active_generation_id: Option<String>,
    pending_generation_id: Option<String>,
    pending_activation_ms: Option<i64>,
}

impl From<&LibraryChannel> for BuildDto {
    fn from(channel: &LibraryChannel) -> Self {
        Self {
            state: if channel.pending_generation_id.is_some()
                || channel.active_generation_id.is_some()
            {
                "ready"
            } else {
                "draft"
            },
            active_generation_id: channel.active_generation_id.clone(),
            pending_generation_id: channel.pending_generation_id.clone(),
            pending_activation_ms: channel.pending_epoch_ms,
        }
    }
}

async fn build_state(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<BuildDto>, ApiError> {
    let channel = visible_channel(&state, &user, &id).await?;
    Ok(Json(BuildDto::from(&channel)))
}

#[derive(Deserialize)]
struct GuideQuery {
    channel_ids: String,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

#[derive(Clone, Serialize)]
struct Programme {
    source: &'static str,
    channel_id: String,
    generation_id: String,
    cycle: i64,
    ordinal: u32,
    starts_at_ms: i64,
    ends_at_ms: i64,
    item_id: i64,
    file_id: i64,
    title: String,
}

async fn guide(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<GuideQuery>,
) -> Result<Json<Vec<Programme>>, ApiError> {
    let ids = query
        .channel_ids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if ids.is_empty() || ids.len() > CHANNEL_GUIDE_CHANNELS_MAX {
        return Err(invalid_recipe_message(format!(
            "channel_ids must contain 1 to {CHANNEL_GUIDE_CHANNELS_MAX} unique IDs"
        )));
    }
    let now_ms = crate::media_sessions::unix_ms();
    let start_ms = query.start_ms.unwrap_or(now_ms);
    let end_ms = query
        .end_ms
        .unwrap_or(start_ms.saturating_add(GUIDE_DEFAULT_MS));
    if end_ms <= start_ms || end_ms.saturating_sub(start_ms) > GUIDE_MAX_MS {
        return Err(invalid_recipe_message(
            "guide window must be positive and no longer than 24 hours",
        ));
    }
    let mut programmes = Vec::new();
    for id in ids {
        let channel = visible_channel(&state, &user, id).await?;
        let mut cursor = start_ms;
        while cursor < end_ms && programmes.len() < CHANNEL_GUIDE_OCCURRENCES_MAX {
            let Some(programme) = programme_at(&state, &user, &channel, cursor).await? else {
                break;
            };
            cursor = programme.ends_at_ms.max(cursor.saturating_add(1));
            programmes.push(programme);
        }
    }
    programmes.sort_by_key(|programme| (programme.starts_at_ms, programme.channel_id.clone()));
    Ok(Json(programmes))
}

#[derive(Serialize)]
struct OccurrenceIdentity {
    cycle: i64,
    ordinal: u32,
}

#[derive(Serialize)]
struct ResolveResponse {
    source: &'static str,
    channel_id: String,
    definition_revision: i64,
    generation_id: String,
    occurrence: OccurrenceIdentity,
    server_now_ms: i64,
    starts_at_ms: i64,
    ends_at_ms: i64,
    item_id: i64,
    file_id: i64,
    position_ms: i64,
    capabilities: ChannelCapabilities,
}

#[derive(Serialize)]
struct ChannelCapabilities {
    join_schedule: bool,
    watch_from_start: bool,
    seek_while_following: bool,
    record: bool,
}

async fn resolve_now(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ResolveResponse>, ApiError> {
    require_runtime_enabled(&state).await?;
    let channel = visible_channel(&state, &user, &id).await?;
    if !channel.enabled {
        return Err(channel_error(
            StatusCode::GONE,
            "channel_unavailable",
            "the channel is disabled",
        ));
    }
    let now_ms = crate::media_sessions::unix_ms();
    let (generation_id, occurrence) = occurrence_at(&state, &user, &channel, now_ms).await?;
    validate_file(&state, &occurrence.entry).await?;
    Ok(Json(resolve_dto(
        &channel,
        generation_id,
        occurrence,
        now_ms,
    )))
}

#[derive(Deserialize)]
struct SessionOccurrence {
    cycle: i64,
    ordinal: u32,
}

#[derive(Deserialize)]
struct ChannelSessionBody {
    generation_id: String,
    occurrence: SessionOccurrence,
    tune_sequence: u64,
    playback: hls::CreateSession,
}

#[derive(Serialize)]
struct ChannelSessionResponse {
    playback: hls::StartResponse,
    library_channel: LibraryChannelPlaybackPurpose,
}

async fn start_session(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    super::network::RemoteAddress(remote): super::network::RemoteAddress,
    Json(mut body): Json<ChannelSessionBody>,
) -> Result<Json<ChannelSessionResponse>, ApiError> {
    require_runtime_enabled(&state).await?;
    let channel = visible_channel(&state, &user, &id).await?;
    if !channel.enabled {
        return Err(channel_error(
            StatusCode::GONE,
            "channel_unavailable",
            "the channel is disabled",
        ));
    }
    let now_ms = crate::media_sessions::unix_ms();
    let (generation_id, occurrence) = occurrence_at(&state, &user, &channel, now_ms).await?;
    if generation_id != body.generation_id
        || occurrence.cycle != body.occurrence.cycle
        || occurrence.ordinal != body.occurrence.ordinal
    {
        return Err(channel_error(
            StatusCode::CONFLICT,
            "channel_occurrence_changed",
            "the scheduled programme changed; resolve the channel again",
        ));
    }
    validate_file(&state, &occurrence.entry).await?;
    body.playback.start = Some(occurrence.position_ms as f64 / 1_000.0);
    body.playback.presentation = Some("vod".to_owned());
    let purpose = LibraryChannelPlaybackPurpose {
        channel_id: channel.id,
        generation_id,
        cycle: occurrence.cycle,
        ordinal: occurrence.ordinal,
        tune_sequence: body.tune_sequence,
        starts_at_ms: occurrence.starts_at_ms,
        ends_at_ms: occurrence.ends_at_ms,
    };
    let playback = hls::create_for_library_channel(
        user,
        state,
        occurrence.entry.file_id,
        headers,
        remote,
        body.playback,
        purpose.clone(),
    )
    .await?;
    Ok(Json(ChannelSessionResponse {
        playback,
        library_channel: purpose,
    }))
}

fn resolve_dto(
    channel: &LibraryChannel,
    generation_id: String,
    occurrence: ResolvedOccurrence,
    now_ms: i64,
) -> ResolveResponse {
    ResolveResponse {
        source: "library",
        channel_id: channel.id.clone(),
        definition_revision: channel.revision,
        generation_id,
        occurrence: OccurrenceIdentity {
            cycle: occurrence.cycle,
            ordinal: occurrence.ordinal,
        },
        server_now_ms: now_ms,
        starts_at_ms: occurrence.starts_at_ms,
        ends_at_ms: occurrence.ends_at_ms,
        item_id: occurrence.entry.item_id,
        file_id: occurrence.entry.file_id,
        position_ms: occurrence.position_ms,
        capabilities: ChannelCapabilities {
            join_schedule: true,
            watch_from_start: true,
            seek_while_following: false,
            record: false,
        },
    }
}

async fn programme_at(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    at_ms: i64,
) -> Result<Option<Programme>, ApiError> {
    if !channel.enabled {
        return Ok(None);
    }
    let (generation_id, occurrence) = match occurrence_at(state, user, channel, at_ms).await {
        Ok(value) => value,
        Err(ApiError::Typed {
            code: "channel_empty",
            ..
        }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let titles = state.store.item_titles(&[occurrence.entry.item_id]).await?;
    Ok(Some(Programme {
        source: "library",
        channel_id: channel.id.clone(),
        generation_id,
        cycle: occurrence.cycle,
        ordinal: occurrence.ordinal,
        starts_at_ms: occurrence.starts_at_ms,
        ends_at_ms: occurrence.ends_at_ms,
        item_id: occurrence.entry.item_id,
        file_id: occurrence.entry.file_id,
        title: titles
            .get(&occurrence.entry.item_id)
            .cloned()
            .unwrap_or_else(|| "Unavailable programme".to_owned()),
    }))
}

async fn occurrence_at(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    at_ms: i64,
) -> Result<(String, ResolvedOccurrence), ApiError> {
    let pending_effective = channel.pending_epoch_ms.is_some_and(|epoch| epoch <= at_ms);
    let (generation_id, epoch) = if pending_effective {
        (
            channel.pending_generation_id.as_ref(),
            channel.pending_epoch_ms,
        )
    } else {
        (
            channel.active_generation_id.as_ref(),
            channel.active_epoch_ms,
        )
    };
    let generation_id = generation_id.ok_or_else(|| {
        channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_empty",
            "the channel has no published schedule",
        )
    })?;
    let epoch =
        epoch.ok_or_else(|| ApiError::Internal("channel generation has no epoch".into()))?;
    let generation = state
        .store
        .read_library_channel_generation(user.id, user.is_admin, &channel.id, generation_id)
        .await?
        .ok_or(ApiError::NotFound("library channel generation"))?;
    let occurrence =
        resolve_occurrence(&generation.entries, epoch, at_ms).map_err(schedule_error)?;
    Ok((generation_id.clone(), occurrence))
}

async fn matching_catalogue(
    state: &AppState,
    recipe: &LibraryChannelRecipe,
) -> Result<Vec<ChannelMatch>, ApiError> {
    let mut after = 0_i64;
    let mut candidates = Vec::new();
    loop {
        let page = state
            .store
            .library_channel_catalog_page(after, CHANNEL_CANDIDATE_PAGE as i64)
            .await?;
        if page.is_empty() {
            break;
        }
        after = page
            .last()
            .map(|candidate| candidate.item_id)
            .unwrap_or(after);
        candidates.extend(page);
        if candidates.len() > CHANNEL_CANDIDATE_ROWS_MAX {
            return Err(channel_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "channel_limit_exceeded",
                "candidate evaluation exceeds 100,000 rows; narrow the recipe",
            ));
        }
        if candidates.len() % CHANNEL_CANDIDATE_PAGE != 0 {
            break;
        }
    }
    evaluate_recipe(recipe, candidates).map_err(|error| {
        if error.message.contains("eligible pool exceeds") {
            channel_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "channel_limit_exceeded",
                error.to_string(),
            )
        } else {
            invalid_recipe(error)
        }
    })
}

async fn build_channel(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    matches: Vec<ChannelMatch>,
    activation: Activation,
) -> Result<(), ApiError> {
    let entries = build_rotation(
        matches
            .into_iter()
            .map(|matched| matched.candidate)
            .collect(),
        channel.recipe.ordering,
        channel.seed,
    )
    .map_err(schedule_error)?;
    let loop_duration_ms = generation_loop_duration(&entries).map_err(schedule_error)?;
    let digest = content_digest(&channel.recipe, &entries);
    let now_ms = crate::media_sessions::unix_ms();
    let generation_id = uuid::Uuid::new_v4().to_string();
    let claim_id = uuid::Uuid::new_v4().to_string();
    let claim = LibraryChannelBuildClaim {
        channel_id: channel.id.clone(),
        generation_id: generation_id.clone(),
        expected_revision: channel.revision,
        claim_id: claim_id.clone(),
        content_digest: digest.clone(),
        now_ms,
        expires_at_ms: now_ms.saturating_add(120_000),
    };
    match state.store.claim_library_channel_build(&claim).await? {
        ChannelBuildMutation::Applied => {}
        other => return Err(build_error(other)),
    }
    for batch in entries.chunks(CHANNEL_GENERATION_STAGE_MAX) {
        match state
            .store
            .stage_library_channel_entries(
                &channel.id,
                &generation_id,
                &claim_id,
                batch,
                crate::media_sessions::unix_ms(),
            )
            .await?
        {
            ChannelBuildMutation::Applied => {}
            other => return Err(build_error(other)),
        }
    }
    let epoch = activation_epoch(state, user, channel, activation, now_ms).await?;
    let publication = LibraryChannelPublication {
        channel_id: channel.id.clone(),
        generation_id,
        expected_revision: channel.revision,
        claim_id,
        content_digest: digest,
        entry_count: entries.len() as i64,
        loop_duration_ms,
        activation_epoch_ms: epoch,
        pending: channel.active_generation_id.is_some(),
        now_ms: crate::media_sessions::unix_ms(),
    };
    match state
        .store
        .publish_library_channel_generation(&publication)
        .await?
    {
        ChannelBuildMutation::Applied => Ok(()),
        other => Err(build_error(other)),
    }
}

async fn activation_epoch(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    activation: Activation,
    now_ms: i64,
) -> Result<i64, ApiError> {
    if channel.active_generation_id.is_none() || matches!(activation, Activation::Initial) {
        return Ok(now_ms);
    }
    let (_, occurrence) = occurrence_at(state, user, channel, now_ms).await?;
    let minimum = now_ms.saturating_add(CHANNEL_ACTIVATION_LEAD_MS);
    match activation {
        Activation::NextProgramme => {
            if occurrence.ends_at_ms >= minimum {
                Ok(occurrence.ends_at_ms)
            } else {
                let (_, next) = occurrence_at(state, user, channel, occurrence.ends_at_ms).await?;
                Ok(next.ends_at_ms)
            }
        }
        Activation::Initial | Activation::NextRotation => {
            let generation_id = channel
                .active_generation_id
                .as_ref()
                .ok_or_else(|| ApiError::Internal("active generation disappeared".into()))?;
            let generation = state
                .store
                .read_library_channel_generation(user.id, user.is_admin, &channel.id, generation_id)
                .await?
                .ok_or(ApiError::NotFound("library channel generation"))?;
            let loop_ms = generation_loop_duration(&generation.entries).map_err(schedule_error)?;
            let mut boundary = channel
                .active_epoch_ms
                .and_then(|epoch| {
                    occurrence
                        .cycle
                        .checked_add(1)
                        .and_then(|cycle| cycle.checked_mul(loop_ms))
                        .and_then(|offset| epoch.checked_add(offset))
                })
                .ok_or_else(|| ApiError::Internal("channel activation overflow".into()))?;
            if boundary < minimum {
                boundary = boundary
                    .checked_add(loop_ms)
                    .ok_or_else(|| ApiError::Internal("channel activation overflow".into()))?;
            }
            Ok(boundary)
        }
    }
}

async fn visible_channel(
    state: &AppState,
    user: &plurx_core::domain::User,
    id: &str,
) -> Result<LibraryChannel, ApiError> {
    state
        .store
        .get_library_channel(user.id, user.is_admin, id)
        .await?
        .ok_or(ApiError::NotFound("library channel"))
}

async fn require_runtime_enabled(state: &AppState) -> Result<(), ApiError> {
    let enabled = state
        .store
        .get_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED)
        .await?
        .is_some_and(|value| plurx_core::store::stored_switch(Some(&value), false));
    if enabled {
        Ok(())
    } else {
        Err(channel_error(
            StatusCode::GONE,
            "channel_unavailable",
            "Library-channel playback is disabled in Settings → Developer",
        ))
    }
}

async fn validate_file(state: &AppState, entry: &ChannelGenerationEntry) -> Result<(), ApiError> {
    let file = state.store.get_file(entry.file_id).await?;
    let valid = file.is_some_and(|file| {
        file.item_id == entry.item_id
            && file.probed
            && file.video_codec.is_some()
            && file.size == entry.file_size
            && file.mtime == entry.file_mtime
            && file.duration_ms == Some(entry.duration_ms)
    });
    if valid {
        Ok(())
    } else {
        Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "scheduled_media_unavailable",
            "the pinned file for this occurrence is missing or changed",
        ))
    }
}

fn normalized_definition(
    body: &DefinitionBody,
    actor_is_admin: bool,
) -> Result<(String, String, LibraryChannelRecipe, ChannelVisibility), ApiError> {
    let name = body.name.trim().to_owned();
    let description = body.description.trim().to_owned();
    if name.chars().count() == 0 || name.chars().count() > CHANNEL_NAME_MAX {
        return Err(invalid_recipe_message("name must contain 1–80 characters"));
    }
    if description.chars().count() > CHANNEL_DESCRIPTION_MAX {
        return Err(invalid_recipe_message(
            "description must contain at most 500 characters",
        ));
    }
    let visibility = body.visibility.unwrap_or(ChannelVisibility::Personal);
    if visibility == ChannelVisibility::Shared && !actor_is_admin {
        return Err(ApiError::Forbidden);
    }
    let recipe = body.recipe.clone().normalize().map_err(invalid_recipe)?;
    Ok((name, description, recipe, visibility))
}

fn valid_request_id(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        Err(invalid_recipe_message(
            "request_id must contain 1 to 128 characters",
        ))
    } else {
        Ok(value.to_owned())
    }
}

fn new_seed() -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"plurx.library-channel.seed.v1\0");
    digest.update(uuid::Uuid::new_v4().as_bytes());
    digest.update(uuid::Uuid::new_v4().as_bytes());
    digest.finalize().into()
}

fn body_digest(body: &impl Serialize) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(body).expect("bounded definition serializes"));
    hex::encode(digest.finalize())
}

fn content_digest(recipe: &LibraryChannelRecipe, entries: &[ChannelGenerationEntry]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"plurx.library-channel.generation.v1\0");
    digest.update(serde_json::to_vec(recipe).expect("bounded recipe serializes"));
    digest.update(serde_json::to_vec(entries).expect("bounded entries serialize"));
    hex::encode(digest.finalize())
}

fn repeat_description(duration_ms: i64) -> String {
    let minutes = duration_ms / 60_000;
    format!(
        "{}h {}m before the rotation repeats",
        minutes / 60,
        minutes % 60
    )
}

fn invalid_recipe(error: plurx_core::library_channels::RecipeValidationError) -> ApiError {
    channel_error(StatusCode::BAD_REQUEST, "invalid_recipe", error.to_string())
}

fn invalid_recipe_message(message: impl Into<String>) -> ApiError {
    channel_error(StatusCode::BAD_REQUEST, "invalid_recipe", message)
}

fn schedule_error(error: plurx_core::library_channels::ScheduleError) -> ApiError {
    channel_error(StatusCode::CONFLICT, "catalogue_changed", error.to_string())
}

fn channel_error(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::typed(status, code, message)
}

fn mutation_error<T>(mutation: ChannelMutation<T>) -> ApiError {
    match mutation {
        ChannelMutation::NotFound => ApiError::NotFound("library channel"),
        ChannelMutation::Forbidden => ApiError::Forbidden,
        ChannelMutation::Stale => channel_error(
            StatusCode::CONFLICT,
            "channel_revision_changed",
            "the channel changed; reload and compare your edits",
        ),
        ChannelMutation::RequestConflict => channel_error(
            StatusCode::CONFLICT,
            "request_id_reused",
            "request_id was already used for a different mutation",
        ),
        ChannelMutation::LimitExceeded => channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_limit_exceeded",
            "the account or server channel limit was reached",
        ),
        ChannelMutation::Applied(_) | ChannelMutation::Replay(_) => {
            ApiError::Internal("unexpected channel mutation result".into())
        }
    }
}

fn build_error(mutation: ChannelBuildMutation) -> ApiError {
    match mutation {
        ChannelBuildMutation::NotFound => ApiError::NotFound("library channel"),
        ChannelBuildMutation::Forbidden => ApiError::Forbidden,
        ChannelBuildMutation::Stale => channel_error(
            StatusCode::CONFLICT,
            "channel_revision_changed",
            "a newer channel definition superseded this build",
        ),
        ChannelBuildMutation::Busy => channel_error(
            StatusCode::TOO_MANY_REQUESTS,
            "channel_build_busy",
            "a channel build is already in progress; retry shortly",
        ),
        ChannelBuildMutation::Invalid => {
            ApiError::Internal("the staged generation failed its publication invariants".into())
        }
        ChannelBuildMutation::Applied => {
            ApiError::Internal("unexpected applied build mapping".into())
        }
    }
}
