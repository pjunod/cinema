//! Authenticated Library-channel definitions, deterministic guides, and tune.
//!
//! This module owns orchestration only. Recipe matching and schedule math live
//! in `plurx_core::library_channels`; streaming remains the finite-HLS service.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use plurx_core::library_channels::{
    build_rotation, evaluate_recipe, generation_content_digest, generation_loop_duration,
    resolve_occurrence, ChannelBuildMutation, ChannelGenerationEntry, ChannelMatch,
    ChannelMutation, ChannelVisibility, LibraryChannel, LibraryChannelBuildClaim,
    LibraryChannelBuildFailure, LibraryChannelDelete, LibraryChannelPublication,
    LibraryChannelRecipe, LibraryChannelUpdate, NewLibraryChannel, ResolvedOccurrence,
    CHANNEL_ACTIVATION_LEAD_MS, CHANNEL_BUILD_CLAIM_MS, CHANNEL_BUILD_RENEW_MS,
    CHANNEL_CANDIDATE_ROWS_MAX, CHANNEL_DESCRIPTION_MAX, CHANNEL_GENERATION_STAGE_MAX,
    CHANNEL_GUIDE_CHANNELS_MAX, CHANNEL_GUIDE_OCCURRENCES_MAX, CHANNEL_NAME_MAX,
    CHANNEL_PREVIEW_PAGE_DEFAULT, CHANNEL_PREVIEW_PAGE_MAX,
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
        .merge(crate::channel_subjects::router())
        .route("/preview", post(preview))
        .route("/guide", get(guide))
        .route("/{id}", get(get_one).put(update).delete(delete_one))
        .route("/{id}/rebuild", post(rebuild))
        .route("/{id}/build", get(build_state))
        .route("/{id}/favourite", put(favourite))
        .route("/{id}/resolve", post(resolve_now))
        .route("/{id}/sessions", post(start_session))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(private_no_store))
}

/// Register the canonical collection route explicitly, plus the spelling
/// shipped by the first native clients while they roll forward. Axum 0.8 does
/// not treat the two paths as equivalent when a router is nested.
pub(crate) fn collection_router() -> Router<AppState> {
    Router::new()
        .route("/library-channels", get(list).post(create))
        .route("/library-channels/", get(list).post(create))
        .layer(DefaultBodyLimit::max(64 * 1024))
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
        .await
        .map_err(channel_store_error)?;
    let now_ms = crate::media_sessions::unix_ms();
    let mut response = Vec::with_capacity(channels.len());
    for channel in channels {
        let now = programme_at(&state, &user, &channel, now_ms).await?;
        let next = match now.as_ref() {
            Some(current) => programme_at(&state, &user, &channel, current.ends_at_ms).await?,
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
    let item_ids = response
        .iter()
        .flat_map(|summary| [summary.now.as_ref(), summary.next.as_ref()])
        .flatten()
        .map(|programme| programme.item_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let titles = state
        .store
        .item_titles(&item_ids)
        .await
        .map_err(channel_store_error)?;
    for summary in &mut response {
        for programme in [&mut summary.now, &mut summary.next]
            .into_iter()
            .filter_map(Option::as_mut)
        {
            programme.title = titles
                .get(&programme.item_id)
                .cloned()
                .unwrap_or_else(|| "Unavailable programme".to_owned());
        }
    }
    Ok(Json(response))
}

#[derive(Debug, Default, Serialize)]
#[serde(untagged)]
enum SubjectPresence {
    #[default]
    Absent,
    Value(Option<String>),
}
impl SubjectPresence {
    fn absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}
#[derive(Debug, Serialize)]
struct InputRecipe {
    #[serde(flatten)]
    recipe: LibraryChannelRecipe,
    #[serde(skip_serializing_if = "SubjectPresence::absent")]
    subject: SubjectPresence,
}
impl<'de> Deserialize<'de> for InputRecipe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let subject = match value.as_object_mut().and_then(|v| v.remove("subject")) {
            None => SubjectPresence::Absent,
            Some(value) => SubjectPresence::Value(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            ),
        };
        Ok(Self {
            recipe: serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            subject,
        })
    }
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
    recipe: InputRecipe,
    /// Hex seed returned by preview. Omitting it creates a fresh schedule;
    /// supplying it makes the saved rotation exactly the previewed rotation.
    #[serde(default)]
    preview_seed: Option<String>,
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
        seed: body
            .preview_seed
            .as_deref()
            .map(parse_seed)
            .transpose()?
            .unwrap_or_else(new_seed),
        now_ms,
    };
    let (stored, replay) = match state
        .store
        .create_library_channel(user.is_admin, &channel)
        .await
        .map_err(channel_store_error)?
    {
        ChannelMutation::Applied(channel) => (channel, false),
        ChannelMutation::Replay(channel) => (channel, true),
        other => return Err(mutation_error(other)),
    };
    queue_saved_build(state.clone(), user.clone(), stored.clone());
    let refreshed = state
        .store
        .get_library_channel(user.id, user.is_admin, &stored.id)
        .await
        .map_err(channel_store_error)?
        .ok_or(ApiError::NotFound("library channel"))?;
    let build_state = build_state_label(&refreshed);
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

fn queue_saved_build(state: AppState, user: plurx_core::domain::User, channel: LibraryChannel) {
    tokio::spawn(async move {
        let result = async {
            if channel.recipe.subject.is_some() {
                return crate::channel_subjects::request_channel(&state, &channel).await;
            }
            let preferred = preferred_generation_files(&state, &user, &channel).await?;
            let evaluation = matching_catalogue(&state, &channel.recipe, Some(&preferred)).await?;
            if evaluation.matches.is_empty() {
                return record_build_failure(
                    &state,
                    &channel,
                    None,
                    None,
                    "channel_empty",
                    "No current matches; any existing schedule remains active.",
                    0,
                )
                .await;
            }
            build_channel(
                &state,
                &user,
                &channel,
                evaluation,
                Activation::NextRotation,
                false,
            )
            .await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(channel_id=%channel.id,error=?error,"Saved definition retained after background build failure");
        }
    });
}

pub(crate) async fn publish_subject(
    state: &AppState,
    channel: &LibraryChannel,
    matches: Vec<ChannelMatch>,
    snapshot_digest: String,
    job_id: &str,
    claim: &str,
    next_programme: bool,
) -> Result<(), ApiError> {
    let user = state
        .store
        .get_user(channel.owner_user_id)
        .await
        .map_err(channel_store_error)?
        .ok_or(ApiError::Forbidden)?;
    let evaluation = CatalogueEvaluation {
        candidate_count: matches.len(),
        matches,
        snapshot_digest,
        subject_claim: Some((job_id.to_owned(), claim.to_owned())),
    };
    build_channel(
        state,
        &user,
        channel,
        evaluation,
        if next_programme {
            Activation::NextProgramme
        } else {
            Activation::NextRotation
        },
        false,
    )
    .await
}

async fn get_one(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChannelDetail>, ApiError> {
    let channel = editable_channel(&state, &user, &id).await?;
    let matching = crate::channel_subjects::summary(&state, &channel).await;
    let mut detail = ChannelDetail::new(channel, user.id, user.is_admin);
    detail.matching = matching;
    Ok(Json(detail))
}

#[derive(Serialize)]
struct ChannelDetail {
    #[serde(flatten)]
    channel: LibraryChannel,
    source: &'static str,
    can_edit: bool,
    can_delete: bool,
    can_share: bool,
    matching: Option<crate::channel_subjects::Summary>,
    subject_recipe: serde_json::Value,
}

impl ChannelDetail {
    fn new(channel: LibraryChannel, user_id: i64, admin: bool) -> Self {
        let editable = channel.owner_user_id == user_id || admin;
        let mut subject_recipe = serde_json::to_value(&channel.recipe).expect("recipe serializes");
        for key in [
            "library_ids",
            "include_item_ids",
            "include_show_ids",
            "exclude_item_ids",
            "exclude_show_ids",
        ] {
            if let Some(values) = subject_recipe[key].as_array_mut() {
                for value in values {
                    if let Some(id) = value.as_i64() {
                        *value = serde_json::Value::String(id.to_string());
                    }
                }
            }
        }
        Self {
            subject_recipe,
            channel,
            source: "library",
            can_edit: editable,
            can_delete: editable,
            can_share: admin,
            matching: None,
        }
    }
}

async fn update(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Result<(StatusCode, Json<MutationResponse>), ApiError> {
    let previous = editable_channel(&state, &user, &id).await?;
    let (name, description, mut recipe, visibility) =
        normalized_definition(&body.definition, user.is_admin)?;
    if body.definition.recipe.subject.absent() {
        recipe.subject = previous.recipe.subject.clone();
    }
    let update = LibraryChannelUpdate {
        subject_next_programme: false,
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
    let (stored, replay) = match state
        .store
        .update_library_channel(&update)
        .await
        .map_err(channel_store_error)?
    {
        ChannelMutation::Applied(channel) => (channel, false),
        ChannelMutation::Replay(channel) => (channel, true),
        other => return Err(mutation_error(other)),
    };
    let _ = replay;
    queue_saved_build(state.clone(), user.clone(), stored.clone());
    let refreshed = visible_channel(&state, &user, &stored.id).await?;
    let build_state = build_state_label(&refreshed);
    Ok((
        StatusCode::ACCEPTED,
        Json(MutationResponse {
            channel: refreshed,
            build_state,
        }),
    ))
}

#[derive(Deserialize, Serialize)]
struct DeleteQuery {
    expected_revision: i64,
    request_id: String,
}

async fn delete_one(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    let deletion = LibraryChannelDelete {
        channel_id: id.clone(),
        actor_user_id: user.id,
        actor_is_admin: user.is_admin,
        expected_revision: query.expected_revision,
        request_id: valid_request_id(&query.request_id)?,
        request_hash: body_digest(&(id, &query)),
        now_ms: crate::media_sessions::unix_ms(),
    };
    match state
        .store
        .delete_library_channel(&deletion)
        .await
        .map_err(channel_store_error)?
    {
        ChannelMutation::Applied(()) | ChannelMutation::Replay(()) => Ok(StatusCode::NO_CONTENT),
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
        .await
        .map_err(channel_store_error)?
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
    cursor: Option<String>,
    #[serde(default)]
    preview_seed: Option<String>,
}

#[derive(Serialize)]
struct PreviewResponse {
    eligible_count: usize,
    unique_duration_ms: i64,
    repeat_description: String,
    content_digest: String,
    preview_seed: String,
    excluded_count: usize,
    diagnostics: Vec<PreviewDiagnostic>,
    matches: Vec<ChannelMatch>,
    first_ten: Vec<ChannelGenerationEntry>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct PreviewDiagnostic {
    code: &'static str,
    count: usize,
    examples: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct PreviewCursor {
    actor_user_id: i64,
    recipe_digest: String,
    content_digest: String,
    offset: usize,
}

async fn preview(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<PreviewBody>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let recipe = body.recipe.normalize().map_err(invalid_recipe)?;
    let evaluation = matching_catalogue(&state, &recipe, None).await?;
    let seed = body
        .preview_seed
        .as_deref()
        .map(parse_seed)
        .transpose()?
        .unwrap_or_else(new_seed);
    let candidates = evaluation
        .matches
        .iter()
        .map(|matched| matched.candidate.clone())
        .collect::<Vec<_>>();
    let rotation = if candidates.is_empty() {
        Vec::new()
    } else {
        build_rotation(candidates, recipe.ordering, seed).map_err(schedule_error)?
    };
    let duration = rotation
        .last()
        .map(|entry| entry.cumulative_start_ms + entry.duration_ms)
        .unwrap_or(0);
    let content_digest = generation_content_digest(seed, &rotation);
    let recipe_digest = body_digest(&recipe);
    let limit = body
        .limit
        .unwrap_or(CHANNEL_PREVIEW_PAGE_DEFAULT)
        .clamp(1, CHANNEL_PREVIEW_PAGE_MAX);
    let offset = if let Some(cursor) = body.cursor.as_deref() {
        let cursor = decode_preview_cursor(cursor)?;
        if cursor.actor_user_id != user.id
            || cursor.recipe_digest != recipe_digest
            || cursor.content_digest != content_digest
            || cursor.offset > evaluation.matches.len()
        {
            return Err(channel_error(
                StatusCode::CONFLICT,
                "catalogue_changed",
                "the preview selection changed; restart preview from its first page",
            ));
        }
        cursor.offset
    } else {
        0
    };
    let end = offset.saturating_add(limit).min(evaluation.matches.len());
    let next_cursor = (end < evaluation.matches.len()).then(|| {
        encode_preview_cursor(&PreviewCursor {
            actor_user_id: user.id,
            recipe_digest,
            content_digest: content_digest.clone(),
            offset: end,
        })
    });
    Ok(Json(PreviewResponse {
        eligible_count: evaluation.matches.len(),
        unique_duration_ms: duration,
        repeat_description: repeat_description(duration),
        content_digest,
        preview_seed: hex::encode(seed),
        excluded_count: evaluation
            .candidate_count
            .saturating_sub(evaluation.matches.len()),
        diagnostics: preview_diagnostics(&evaluation),
        matches: evaluation.matches[offset..end].to_vec(),
        first_ten: rotation.into_iter().take(10).collect(),
        next_cursor,
    }))
}

fn encode_preview_cursor(cursor: &PreviewCursor) -> String {
    hex::encode(serde_json::to_vec(cursor).expect("bounded preview cursor serializes"))
}

fn decode_preview_cursor(value: &str) -> Result<PreviewCursor, ApiError> {
    if value.len() > 2_048 {
        return Err(invalid_recipe_message("preview cursor is too large"));
    }
    let bytes =
        hex::decode(value).map_err(|_| invalid_recipe_message("preview cursor is not valid"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| invalid_recipe_message("preview cursor is not valid"))
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Activation {
    Initial,
    NextRotation,
    NextProgramme,
}

#[derive(Deserialize, Serialize)]
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
    let request_id = valid_request_id(&body.request_id)?;
    let channel = visible_channel(&state, &user, &id).await?;
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
    let update = LibraryChannelUpdate {
        subject_next_programme: matches!(body.activation, Activation::NextProgramme),
        channel_id: id.clone(),
        actor_user_id: user.id,
        actor_is_admin: user.is_admin,
        expected_revision: body.expected_revision,
        request_id,
        request_hash: body_digest(&(id.as_str(), &body)),
        name: channel.name.clone(),
        description: channel.description.clone(),
        visibility: channel.visibility,
        enabled: channel.enabled,
        recipe: channel.recipe.clone(),
        seed: if body.reshuffle {
            new_seed()
        } else {
            channel.seed
        },
        now_ms: crate::media_sessions::unix_ms(),
    };
    let (channel, replay) = match state
        .store
        .update_library_channel(&update)
        .await
        .map_err(channel_store_error)?
    {
        ChannelMutation::Applied(channel) => (channel, false),
        ChannelMutation::Replay(channel) => (channel, true),
        other => return Err(mutation_error(other)),
    };
    if replay {
        return Ok((StatusCode::ACCEPTED, Json(BuildDto::from(&channel))));
    }
    if channel.recipe.subject.is_some() {
        crate::channel_subjects::request_channel_activation(
            &state,
            &channel,
            matches!(body.activation, Activation::NextProgramme),
        )
        .await?;
        return Ok((StatusCode::ACCEPTED, Json(BuildDto::from(&channel))));
    }
    let preferred_files = preferred_generation_files(&state, &user, &channel).await?;
    let matches = matching_catalogue(&state, &channel.recipe, Some(&preferred_files)).await?;
    if matches.matches.is_empty() {
        record_build_failure(
            &state,
            &channel,
            None,
            None,
            "channel_empty",
            "the current recipe has no eligible titles; the old schedule was retained",
            0,
        )
        .await?;
        return Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_empty",
            "the current recipe has no eligible titles; the old schedule was retained",
        ));
    }
    build_channel(&state, &user, &channel, matches, body.activation, false).await?;
    let channel = editable_channel(&state, &user, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(BuildDto::from(&channel))))
}

#[derive(Serialize)]
struct BuildDto {
    matching: Option<crate::channel_subjects::Summary>,
    state: String,
    error_code: Option<String>,
    error_message: Option<String>,
    candidate_count: i64,
    entry_count: i64,
    last_attempt_ms: Option<i64>,
    last_success_ms: Option<i64>,
    active_generation_id: Option<String>,
    pending_generation_id: Option<String>,
    pending_activation_ms: Option<i64>,
}

impl From<&LibraryChannel> for BuildDto {
    fn from(channel: &LibraryChannel) -> Self {
        Self {
            matching: None,
            state: channel.build_state.clone(),
            error_code: channel.build_error_code.clone(),
            error_message: channel.build_error_message.clone(),
            candidate_count: channel.build_candidate_count,
            entry_count: channel.build_entry_count,
            last_attempt_ms: channel.build_last_attempt_ms,
            last_success_ms: channel.build_last_success_ms,
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
    let mut result = BuildDto::from(&channel);
    if channel.owner_user_id == user.id || user.is_admin {
        result.matching = crate::channel_subjects::summary(&state, &channel).await;
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
struct GuideQuery {
    channel_ids: String,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    cursor: Option<String>,
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

#[derive(Deserialize, Serialize)]
struct GuideCursor {
    actor_user_id: i64,
    channel_digest: String,
    start_ms: i64,
    end_ms: i64,
    last_starts_at_ms: i64,
    last_channel_id: String,
}

async fn guide(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<GuideQuery>,
) -> Result<(HeaderMap, Json<Vec<Programme>>), ApiError> {
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
    let cursor = query
        .cursor
        .as_deref()
        .map(decode_guide_cursor)
        .transpose()?;
    let mut channels = Vec::with_capacity(ids.len());
    for id in ids {
        channels.push(visible_channel(&state, &user, id).await?);
    }
    let schedule_binding = channels
        .iter()
        .map(|channel| {
            (
                channel.id.as_str(),
                channel.revision,
                channel.enabled,
                channel.active_generation_id.as_deref(),
                channel.active_epoch_ms,
                channel.pending_generation_id.as_deref(),
                channel.pending_epoch_ms,
            )
        })
        .collect::<Vec<_>>();
    let channel_digest = body_digest(&schedule_binding);
    if let Some(cursor) = cursor.as_ref() {
        if cursor.actor_user_id != user.id || cursor.channel_digest != channel_digest {
            return Err(channel_error(
                StatusCode::CONFLICT,
                "catalogue_changed",
                "the guide cursor belongs to another viewer or channel selection",
            ));
        }
    }
    let now_ms = crate::media_sessions::unix_ms();
    let start_ms = cursor
        .as_ref()
        .map(|cursor| cursor.start_ms)
        .unwrap_or_else(|| query.start_ms.unwrap_or(now_ms));
    let end_ms = cursor
        .as_ref()
        .map(|cursor| cursor.end_ms)
        .unwrap_or_else(|| {
            query
                .end_ms
                .unwrap_or(start_ms.saturating_add(GUIDE_DEFAULT_MS))
        });
    if cursor.is_some()
        && (query.start_ms.is_some_and(|value| value != start_ms)
            || query.end_ms.is_some_and(|value| value != end_ms))
    {
        return Err(invalid_recipe_message(
            "guide cursor window does not match start_ms/end_ms",
        ));
    }
    if end_ms <= start_ms || end_ms.saturating_sub(start_ms) > GUIDE_MAX_MS {
        return Err(invalid_recipe_message(
            "guide window must be positive and no longer than 24 hours",
        ));
    }
    let last_key = cursor
        .as_ref()
        .map(|cursor| (cursor.last_starts_at_ms, cursor.last_channel_id.as_str()));
    let mut streams = Vec::with_capacity(channels.len());
    for channel in channels {
        let mut at_ms = last_key
            .map(|(starts_at_ms, _)| starts_at_ms.max(start_ms))
            .unwrap_or(start_ms);
        let mut head = programme_at(&state, &user, &channel, at_ms).await?;
        while let (Some(programme), Some(last)) = (head.as_ref(), last_key) {
            if (programme.starts_at_ms, programme.channel_id.as_str()) > last {
                break;
            }
            at_ms = programme.ends_at_ms.max(at_ms.saturating_add(1));
            if at_ms >= end_ms {
                head = None;
                break;
            }
            head = programme_at(&state, &user, &channel, at_ms).await?;
        }
        streams.push((channel, head));
    }
    let mut programmes = Vec::with_capacity(CHANNEL_GUIDE_OCCURRENCES_MAX);
    while programmes.len() < CHANNEL_GUIDE_OCCURRENCES_MAX {
        let Some(index) = streams
            .iter()
            .enumerate()
            .filter_map(|(index, (_, head))| head.as_ref().map(|head| (index, head)))
            .filter(|(_, head)| head.starts_at_ms < end_ms)
            .min_by_key(|(_, head)| (head.starts_at_ms, head.channel_id.as_str()))
            .map(|(index, _)| index)
        else {
            break;
        };
        let programme = streams[index]
            .1
            .take()
            .expect("selected guide stream has a head");
        let next_at_ms = programme.ends_at_ms;
        programmes.push(programme);
        streams[index].1 = if next_at_ms < end_ms {
            programme_at(&state, &user, &streams[index].0, next_at_ms).await?
        } else {
            None
        };
    }
    let item_ids = programmes
        .iter()
        .map(|programme| programme.item_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let titles = state
        .store
        .item_titles(&item_ids)
        .await
        .map_err(channel_store_error)?;
    for programme in &mut programmes {
        programme.title = titles
            .get(&programme.item_id)
            .cloned()
            .unwrap_or_else(|| "Unavailable programme".to_owned());
    }
    let has_more = streams.iter().any(|(_, head)| {
        head.as_ref()
            .is_some_and(|programme| programme.starts_at_ms < end_ms)
    });
    let mut headers = HeaderMap::new();
    if has_more {
        let last = programmes
            .last()
            .expect("a full guide page has a final programme");
        let next = encode_guide_cursor(&GuideCursor {
            actor_user_id: user.id,
            channel_digest,
            start_ms,
            end_ms,
            last_starts_at_ms: last.starts_at_ms,
            last_channel_id: last.channel_id.clone(),
        });
        headers.insert(
            "x-plurx-next-cursor",
            HeaderValue::from_str(&next)
                .map_err(|_| ApiError::Internal("guide cursor header is invalid".into()))?,
        );
    }
    Ok((headers, Json(programmes)))
}

fn encode_guide_cursor(cursor: &GuideCursor) -> String {
    hex::encode(serde_json::to_vec(cursor).expect("bounded guide cursor serializes"))
}

fn decode_guide_cursor(value: &str) -> Result<GuideCursor, ApiError> {
    if value.len() > 4_096 {
        return Err(invalid_recipe_message("guide cursor is too large"));
    }
    let bytes =
        hex::decode(value).map_err(|_| invalid_recipe_message("guide cursor is invalid"))?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_recipe_message("guide cursor is invalid"))
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
    RESOLVE_TOTAL.fetch_add(1, Ordering::Relaxed);
    let _latency = ResolveLatency(Instant::now());
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
        OCCURRENCE_CHANGES.fetch_add(1, Ordering::Relaxed);
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
        state.clone(),
        occurrence.entry.file_id,
        headers,
        remote,
        body.playback,
        purpose.clone(),
    )
    .await?;
    let programme = state
        .store
        .item_titles(&[occurrence.entry.item_id])
        .await
        .map_err(channel_store_error)?
        .remove(&occurrence.entry.item_id)
        .unwrap_or_else(|| "Unavailable programme".to_owned());
    activity_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            playback.session_id.clone(),
            ChannelActivityContext {
                channel: channel.name,
                programme,
            },
        );
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
        title: String::new(),
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
    let generation = cached_generation(state, user, channel, generation_id).await?;
    let occurrence =
        resolve_occurrence(&generation.entries, epoch, at_ms).map_err(schedule_error)?;
    Ok((generation_id.clone(), occurrence))
}

const GENERATION_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
const GENERATION_CACHE_MAX_ENTRIES: usize = 64;

#[derive(Clone)]
struct CachedGeneration {
    channel_id: String,
    generation_id: String,
    content_digest: String,
    bytes: usize,
    generation: plurx_core::library_channels::LibraryChannelGeneration,
}

#[derive(Default)]
struct GenerationCache {
    entries: VecDeque<CachedGeneration>,
    bytes: usize,
}

fn generation_cache() -> &'static std::sync::Mutex<GenerationCache> {
    static CACHE: OnceLock<std::sync::Mutex<GenerationCache>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(GenerationCache::default()))
}

async fn cached_generation(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    generation_id: &str,
) -> Result<plurx_core::library_channels::LibraryChannelGeneration, ApiError> {
    {
        let mut cache = generation_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = cache.entries.iter().position(|entry| {
            entry.channel_id == channel.id && entry.generation_id == generation_id
        }) {
            let entry = cache
                .entries
                .remove(index)
                .expect("located generation cache entry exists");
            debug_assert_eq!(entry.content_digest, entry.generation.content_digest);
            let generation = entry.generation.clone();
            cache.entries.push_front(entry);
            GENERATION_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(generation);
        }
    }
    GENERATION_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    let generation = state
        .store
        .read_library_channel_generation(user.id, user.is_admin, &channel.id, generation_id)
        .await
        .map_err(channel_store_error)?
        .ok_or(ApiError::NotFound("library channel generation"))?;
    if generation.state != "ready" {
        return Err(ApiError::NotFound("library channel generation"));
    }
    let bytes = generation
        .entries
        .len()
        .saturating_mul(std::mem::size_of::<ChannelGenerationEntry>())
        .saturating_add(generation.id.len())
        .saturating_add(generation.channel_id.len())
        .saturating_add(generation.content_digest.len());
    if bytes <= GENERATION_CACHE_MAX_BYTES {
        let mut cache = generation_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.entries.retain(|entry| {
            !(entry.channel_id == channel.id && entry.generation_id == generation_id)
        });
        cache.bytes = cache.entries.iter().map(|entry| entry.bytes).sum();
        cache.bytes = cache.bytes.saturating_add(bytes);
        cache.entries.push_front(CachedGeneration {
            channel_id: channel.id.clone(),
            generation_id: generation_id.to_owned(),
            content_digest: generation.content_digest.clone(),
            bytes,
            generation: generation.clone(),
        });
        while cache.entries.len() > GENERATION_CACHE_MAX_ENTRIES
            || cache.bytes > GENERATION_CACHE_MAX_BYTES
        {
            if let Some(evicted) = cache.entries.pop_back() {
                cache.bytes = cache.bytes.saturating_sub(evicted.bytes);
            } else {
                break;
            }
        }
    }
    Ok(generation)
}

static GENERATION_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static GENERATION_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static BUILD_SUCCEEDED: AtomicUsize = AtomicUsize::new(0);
static BUILD_FAILED: AtomicUsize = AtomicUsize::new(0);
static BUILD_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static RESOLVE_TOTAL: AtomicUsize = AtomicUsize::new(0);
static RESOLVE_LATENCY_US: AtomicUsize = AtomicUsize::new(0);
static PUBLICATION_CONFLICTS: AtomicUsize = AtomicUsize::new(0);
static BUILD_QUEUED: AtomicUsize = AtomicUsize::new(0);
static CANDIDATE_ROWS: AtomicUsize = AtomicUsize::new(0);
static OCCURRENCE_CHANGES: AtomicUsize = AtomicUsize::new(0);
static UNAVAILABLE_SLOTS: AtomicUsize = AtomicUsize::new(0);

struct ResolveLatency(Instant);

impl Drop for ResolveLatency {
    fn drop(&mut self) {
        RESOLVE_LATENCY_US.fetch_add(
            usize::try_from(self.0.elapsed().as_micros()).unwrap_or(usize::MAX),
            Ordering::Relaxed,
        );
    }
}

#[derive(Clone)]
struct ChannelActivityContext {
    channel: String,
    programme: String,
}

fn activity_registry() -> &'static std::sync::Mutex<BTreeMap<String, ChannelActivityContext>> {
    static REGISTRY: OnceLock<std::sync::Mutex<BTreeMap<String, ChannelActivityContext>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

pub(crate) fn active_contexts(active_session_ids: &[String]) -> Vec<(String, String)> {
    let active = active_session_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut registry = activity_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|session, _| active.contains(session.as_str()));
    registry
        .values()
        .map(|context| (context.channel.clone(), context.programme.clone()))
        .collect()
}

pub(crate) fn prometheus() -> String {
    format!(
        "# HELP plurx_library_channel_build_total Library-channel build outcomes.\n\
         # TYPE plurx_library_channel_build_total counter\n\
         plurx_library_channel_build_total{{outcome=\"attempt\"}} {}\n\
         plurx_library_channel_build_total{{outcome=\"ready\"}} {}\n\
         plurx_library_channel_build_total{{outcome=\"failed\"}} {}\n\
         # HELP plurx_library_channel_build_queue_depth Coalesced channel builds waiting for a worker.\n\
         # TYPE plurx_library_channel_build_queue_depth gauge\n\
         plurx_library_channel_build_queue_depth {}\n\
         # HELP plurx_library_channel_resolve_total Authoritative channel resolves.\n\
         # TYPE plurx_library_channel_resolve_total counter\n\
         plurx_library_channel_resolve_total {}\n\
         # TYPE plurx_library_channel_resolve_latency_seconds summary\n\
         plurx_library_channel_resolve_latency_seconds_count {}\n\
         plurx_library_channel_resolve_latency_seconds_sum {:.6}\n\
         # TYPE plurx_library_channel_candidate_rows_total counter\n\
         plurx_library_channel_candidate_rows_total {}\n\
         # TYPE plurx_library_channel_occurrence_change_total counter\n\
         plurx_library_channel_occurrence_change_total {}\n\
         # TYPE plurx_library_channel_unavailable_slot_total counter\n\
         plurx_library_channel_unavailable_slot_total {}\n\
         # HELP plurx_library_channel_publication_conflict_total Catalogue or definition fences that refused publication.\n\
         # TYPE plurx_library_channel_publication_conflict_total counter\n\
         plurx_library_channel_publication_conflict_total {}\n\
         plurx_library_channel_generation_cache_total{{outcome=\"hit\"}} {}\n\
         plurx_library_channel_generation_cache_total{{outcome=\"miss\"}} {}\n",
        BUILD_ATTEMPTS.load(Ordering::Relaxed), BUILD_SUCCEEDED.load(Ordering::Relaxed),
        BUILD_FAILED.load(Ordering::Relaxed), BUILD_QUEUED.load(Ordering::Relaxed),
        RESOLVE_TOTAL.load(Ordering::Relaxed), RESOLVE_TOTAL.load(Ordering::Relaxed),
        RESOLVE_LATENCY_US.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        CANDIDATE_ROWS.load(Ordering::Relaxed), OCCURRENCE_CHANGES.load(Ordering::Relaxed),
        UNAVAILABLE_SLOTS.load(Ordering::Relaxed), PUBLICATION_CONFLICTS.load(Ordering::Relaxed),
        GENERATION_CACHE_HITS.load(Ordering::Relaxed), GENERATION_CACHE_MISSES.load(Ordering::Relaxed),
    )
}

#[derive(Clone)]
struct CatalogueEvaluation {
    subject_claim: Option<(String, String)>,
    matches: Vec<ChannelMatch>,
    snapshot_digest: String,
    candidate_count: usize,
}

async fn matching_catalogue(
    state: &AppState,
    recipe: &LibraryChannelRecipe,
    preferred_files: Option<&BTreeMap<i64, i64>>,
) -> Result<CatalogueEvaluation, ApiError> {
    // One bounded Store query is one coherent SQLite snapshot / Hiqlite
    // consistent read. Separate page calls can straddle catalogue mutations
    // and produce a rotation which never existed at any instant.
    let mut candidates = state
        .store
        .library_channel_catalog_snapshot((CHANNEL_CANDIDATE_ROWS_MAX + 1) as i64)
        .await
        .map_err(channel_store_error)?;
    if candidates.len() > CHANNEL_CANDIDATE_ROWS_MAX {
        return Err(channel_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel_limit_exceeded",
            "candidate evaluation exceeds 100,000 rows; narrow the recipe",
        ));
    }
    let snapshot_digest = body_digest(&candidates);
    let candidate_count = candidates.len();
    CANDIDATE_ROWS.fetch_add(candidate_count, Ordering::Relaxed);
    candidates.sort_by_key(|candidate| {
        (
            candidate.item_id,
            usize::from(
                preferred_files
                    .and_then(|files| files.get(&candidate.item_id))
                    .is_none_or(|file_id| *file_id != candidate.file_id),
            ),
            candidate.file_id,
        )
    });
    let matches = evaluate_recipe(recipe, candidates).map_err(|error| {
        if error.message.contains("eligible pool exceeds") {
            channel_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "channel_limit_exceeded",
                error.to_string(),
            )
        } else {
            invalid_recipe(error)
        }
    })?;
    Ok(CatalogueEvaluation {
        subject_claim: None,
        matches,
        snapshot_digest,
        candidate_count,
    })
}

async fn preferred_generation_files(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
) -> Result<BTreeMap<i64, i64>, ApiError> {
    let now_ms = crate::media_sessions::unix_ms();
    let generation_id = if channel
        .pending_epoch_ms
        .is_some_and(|epoch| epoch <= now_ms)
    {
        channel.pending_generation_id.as_deref()
    } else {
        channel.active_generation_id.as_deref()
    };
    let Some(generation_id) = generation_id else {
        return Ok(BTreeMap::new());
    };
    let generation = cached_generation(state, user, channel, generation_id).await?;
    Ok(generation
        .entries
        .into_iter()
        .map(|entry| (entry.item_id, entry.file_id))
        .collect())
}

async fn build_channel(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    evaluation: CatalogueEvaluation,
    activation: Activation,
    automatic: bool,
) -> Result<(), ApiError> {
    BUILD_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let candidate_count = evaluation.candidate_count as i64;
    let result = build_channel_inner(state, user, channel, evaluation, activation, automatic).await;
    if let Err(error) = result {
        // The definition mutation remains successful and replayable; its
        // independently durable build verdict explains what needs retrying.
        let message = format!("{error:?}");
        let _ = record_build_failure(
            state,
            channel,
            None,
            None,
            "channel_build_failed",
            &message,
            candidate_count,
        )
        .await;
        return Err(error);
    }
    Ok(())
}

async fn build_channel_inner(
    state: &AppState,
    user: &plurx_core::domain::User,
    channel: &LibraryChannel,
    mut evaluation: CatalogueEvaluation,
    activation: Activation,
    automatic: bool,
) -> Result<(), ApiError> {
    let channel_lock = channel_build_lock(&channel.id);
    let _channel_guard = channel_lock.lock().await;
    let _build_permit = acquire_build_slot().await?;
    let current = editable_channel(state, user, &channel.id).await?;
    if current.revision != channel.revision {
        return Err(build_error(ChannelBuildMutation::Stale));
    }
    if automatic
        && current.last_auto_build_ms.is_some_and(|last| {
            crate::media_sessions::unix_ms().saturating_sub(last) < 15 * 60 * 1_000
        })
    {
        return Ok(());
    }
    for attempt in 0..=2 {
        let entries = build_rotation(
            evaluation
                .matches
                .iter()
                .map(|matched| matched.candidate.clone())
                .collect(),
            channel.recipe.ordering,
            channel.seed,
        )
        .map_err(schedule_error)?;
        let loop_duration_ms = generation_loop_duration(&entries).map_err(schedule_error)?;
        let digest = generation_content_digest(channel.seed, &entries);
        for generation_id in [
            current.pending_generation_id.as_deref(),
            current.active_generation_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if state
                .store
                .read_library_channel_generation(user.id, true, &channel.id, generation_id)
                .await
                .map_err(channel_store_error)?
                .is_some_and(|generation| generation.content_digest == digest)
            {
                match state
                    .store
                    .complete_library_channel_build_without_publication(
                        &channel.id,
                        channel.revision,
                        evaluation.matches.len() as i64,
                        automatic,
                        crate::media_sessions::unix_ms(),
                    )
                    .await
                    .map_err(channel_store_error)?
                {
                    ChannelBuildMutation::Applied => {
                        BUILD_SUCCEEDED.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                    other => return Err(build_error(other)),
                }
            }
        }
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
            expires_at_ms: now_ms.saturating_add(CHANNEL_BUILD_CLAIM_MS),
        };
        match state
            .store
            .claim_library_channel_build(&claim)
            .await
            .map_err(channel_store_error)?
        {
            ChannelBuildMutation::Applied => {}
            other => return Err(build_error(other)),
        }
        let mut last_renewed_ms = now_ms;
        for batch in entries.chunks(CHANNEL_GENERATION_STAGE_MAX) {
            let batch_now_ms = crate::media_sessions::unix_ms();
            if batch_now_ms.saturating_sub(last_renewed_ms) >= CHANNEL_BUILD_RENEW_MS {
                match state
                    .store
                    .renew_library_channel_build(
                        &channel.id,
                        &generation_id,
                        &claim_id,
                        batch_now_ms,
                        batch_now_ms.saturating_add(CHANNEL_BUILD_CLAIM_MS),
                    )
                    .await
                    .map_err(channel_store_error)?
                {
                    ChannelBuildMutation::Applied => last_renewed_ms = batch_now_ms,
                    other => return Err(build_error(other)),
                }
            }
            match state
                .store
                .stage_library_channel_entries(
                    &channel.id,
                    &generation_id,
                    &claim_id,
                    batch,
                    batch_now_ms,
                )
                .await
                .map_err(channel_store_error)?
            {
                ChannelBuildMutation::Applied => {}
                other => return Err(build_error(other)),
            }
        }
        let preferred = entries
            .iter()
            .map(|entry| (entry.item_id, entry.file_id))
            .collect();
        let fenced = matching_catalogue(state, &channel.recipe, Some(&preferred)).await?;
        if fenced.snapshot_digest != evaluation.snapshot_digest {
            if evaluation.subject_claim.is_some() {
                return Err(channel_error(
                    StatusCode::CONFLICT,
                    "catalogue_changed",
                    "Catalogue changed during subject publication; reconstruct the job.",
                ));
            }
            PUBLICATION_CONFLICTS.fetch_add(1, Ordering::Relaxed);
            record_build_failure(
                state,
                channel,
                Some(&generation_id),
                Some(&claim_id),
                "catalogue_changed",
                "the catalogue changed during staging; retrying from a coherent snapshot",
                evaluation.matches.len() as i64,
            )
            .await?;
            if attempt == 2 {
                return Err(channel_error(
                    StatusCode::CONFLICT,
                    "catalogue_changed",
                    "the catalogue kept changing during publication; retry shortly",
                ));
            }
            evaluation = fenced;
            continue;
        }
        // Publication time is sampled only after staging and the catalogue
        // fence. Replacement lead time therefore cannot be consumed by work.
        let publication_now_ms = crate::media_sessions::unix_ms();
        let pointer = editable_channel(state, user, &channel.id).await?;
        let epoch = activation_epoch(state, user, &pointer, activation, publication_now_ms).await?;
        let publication = LibraryChannelPublication {
            subject_claim: evaluation.subject_claim.clone(),
            channel_id: channel.id.clone(),
            generation_id,
            expected_revision: channel.revision,
            claim_id,
            content_digest: digest,
            seed: channel.seed,
            entry_count: entries.len() as i64,
            loop_duration_ms,
            activation_epoch_ms: epoch,
            pending: pointer.active_generation_id.is_some(),
            automatic,
            now_ms: publication_now_ms,
        };
        return match state
            .store
            .publish_library_channel_generation(&publication)
            .await
            .map_err(channel_store_error)?
        {
            ChannelBuildMutation::Applied => {
                BUILD_SUCCEEDED.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            other => Err(build_error(other)),
        };
    }
    unreachable!("bounded publication loop always returns")
}

fn channel_build_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        locks
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

/// Recover catalogue mutation notifications that were coalesced or missed.
/// The first pass waits for the same 30-second debounce used by mutations;
/// later passes run every 15 minutes. Unchanged content exits before a claim,
/// so an idle channel produces no recurring Raft writes.
pub(crate) async fn reconcile_loop(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    const RECONCILE: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    const INITIAL_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(30);
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(INITIAL_DEBOUNCE) => {}
    }
    reconcile_once(&state).await;
    loop {
        let mutation = tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = catalogue_mutations().notified() => true,
            _ = tokio::time::sleep(RECONCILE) => false,
        };
        if mutation {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(INITIAL_DEBOUNCE) => {}
            }
        }
        reconcile_once(&state).await;
    }
}

fn catalogue_mutations() -> &'static tokio::sync::Notify {
    static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// Catalogue writers emit one cheap edge; the reconciler coalesces all edges
/// for 30 seconds and the durable per-channel timestamp enforces the 15-minute
/// floor across process restarts.
pub(crate) fn notify_catalogue_mutation() {
    catalogue_mutations().notify_one();
}

async fn reconcile_once(state: &AppState) {
    if let Err(error) = state
        .store
        .prune_library_channel_state(
            crate::media_sessions::unix_ms(),
            plurx_core::library_channels::CHANNEL_PRUNE_BATCH_MAX,
        )
        .await
    {
        tracing::warn!(error = ?error, "Library-channel state pruning failed");
    }

    let mut after: Option<String> = None;
    loop {
        let channels = match state
            .store
            .list_library_channel_refresh_candidates(
                after.as_deref(),
                100,
                crate::media_sessions::unix_ms(),
            )
            .await
        {
            Ok(channels) => channels,
            Err(error) => {
                tracing::warn!(error = ?error, "Library-channel reconciliation could not list definitions");
                return;
            }
        };
        if channels.is_empty() {
            return;
        }
        let page_len = channels.len();
        after = channels.last().map(|channel| channel.id.clone());
        for channel in channels {
            let user = match state.store.get_user(channel.owner_user_id).await {
                Ok(Some(user)) => user,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(channel_id = %channel.id, error = ?error, "Library-channel reconciliation could not read owner");
                    continue;
                }
            };
            let result = async {
                if channel.recipe.subject.is_some() {
                    return crate::channel_subjects::request_channel(state, &channel).await;
                }
                let preferred_files = preferred_generation_files(state, &user, &channel).await?;
                let matches =
                    matching_catalogue(state, &channel.recipe, Some(&preferred_files)).await?;
                if matches.matches.is_empty() {
                    return Ok::<(), ApiError>(());
                }
                build_channel(
                    state,
                    &user,
                    &channel,
                    matches,
                    Activation::NextRotation,
                    true,
                )
                .await
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(channel_id = %channel.id, error = ?error, "Library-channel automatic rebuild retained the previous schedule");
            }
        }
        if page_len < 100 {
            return;
        }
    }
}

fn build_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2))))
}

async fn acquire_build_slot() -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    let slots = build_slots();
    if let Ok(permit) = Arc::clone(&slots).try_acquire_owned() {
        return Ok(permit);
    }
    let queued_before = BUILD_QUEUED.fetch_add(1, Ordering::AcqRel);
    if queued_before >= plurx_core::library_channels::CHANNEL_BUILD_QUEUE_MAX {
        BUILD_QUEUED.fetch_sub(1, Ordering::AcqRel);
        return Err(ApiError::TypedRetry {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "channel_build_busy",
            message: "the bounded Library-channel build queue is full; retry shortly".to_owned(),
            retry_after_seconds: 2,
        });
    }
    let permit = slots.acquire_owned().await.map_err(|_| {
        ApiError::ServiceUnavailable("the Library-channel builder is shutting down".to_owned())
    });
    BUILD_QUEUED.fetch_sub(1, Ordering::AcqRel);
    permit
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
    let (effective_generation_id, occurrence) = occurrence_at(state, user, channel, now_ms).await?;
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
            let generation =
                cached_generation(state, user, channel, &effective_generation_id).await?;
            let loop_ms = generation_loop_duration(&generation.entries).map_err(schedule_error)?;
            let effective_epoch = if channel.pending_generation_id.as_deref()
                == Some(effective_generation_id.as_str())
            {
                channel.pending_epoch_ms
            } else {
                channel.active_epoch_ms
            };
            let mut boundary = effective_epoch
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
        .get_library_channel(user.id, false, id)
        .await
        .map_err(channel_store_error)?
        .ok_or(ApiError::NotFound("library channel"))
}

async fn editable_channel(
    state: &AppState,
    user: &plurx_core::domain::User,
    id: &str,
) -> Result<LibraryChannel, ApiError> {
    let channel = state
        .store
        .get_library_channel(user.id, user.is_admin, id)
        .await
        .map_err(channel_store_error)?
        .ok_or(ApiError::NotFound("library channel"))?;
    if channel.owner_user_id == user.id || user.is_admin {
        Ok(channel)
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn require_runtime_enabled(state: &AppState) -> Result<(), ApiError> {
    let enabled = state
        .store
        .get_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED)
        .await
        .map_err(channel_store_error)?
        .is_some_and(|value| plurx_core::store::stored_switch(Some(&value), false));
    if enabled {
        Ok(())
    } else {
        Err(channel_error(
            StatusCode::GONE,
            "channel_unavailable",
            "Library-channel playback is disabled in Settings → Live TV",
        ))
    }
}

async fn validate_file(state: &AppState, entry: &ChannelGenerationEntry) -> Result<(), ApiError> {
    let file = state
        .store
        .get_file(entry.file_id)
        .await
        .map_err(channel_store_error)?;
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
        UNAVAILABLE_SLOTS.fetch_add(1, Ordering::Relaxed);
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
    let mut recipe = body.recipe.recipe.clone();
    if let SubjectPresence::Value(subject) = &body.recipe.subject {
        recipe.subject = subject.clone();
    }
    let recipe = recipe.normalize().map_err(invalid_recipe)?;
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

fn parse_seed(value: &str) -> Result<[u8; 32], ApiError> {
    let bytes =
        hex::decode(value).map_err(|_| invalid_recipe_message("preview_seed is invalid"))?;
    <[u8; 32]>::try_from(bytes)
        .map_err(|_| invalid_recipe_message("preview_seed must encode 32 bytes"))
}

fn body_digest(body: &impl Serialize) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(body).expect("bounded definition serializes"));
    hex::encode(digest.finalize())
}

fn build_state_label(channel: &LibraryChannel) -> &'static str {
    match channel.build_state.as_str() {
        "queued" => "queued",
        "building" => "building",
        "ready" => "ready",
        "failed" => "failed",
        _ => "draft",
    }
}

fn preview_diagnostics(evaluation: &CatalogueEvaluation) -> Vec<PreviewDiagnostic> {
    let mut diagnostics = Vec::new();
    let inherited = evaluation
        .matches
        .iter()
        .filter(|matched| {
            matched
                .reasons
                .iter()
                .any(|reason| reason.starts_with("show."))
        })
        .count();
    if inherited > 0 {
        diagnostics.push(PreviewDiagnostic {
            code: "inherited_show_metadata",
            count: inherited,
            examples: evaluation
                .matches
                .iter()
                .filter(|matched| {
                    matched
                        .reasons
                        .iter()
                        .any(|reason| reason.starts_with("show."))
                })
                .take(3)
                .map(|matched| matched.candidate.title.clone())
                .collect(),
        });
    }
    let excluded = evaluation
        .candidate_count
        .saturating_sub(evaluation.matches.len());
    if excluded > 0 {
        diagnostics.push(PreviewDiagnostic {
            code: "recipe_excluded",
            count: excluded,
            examples: Vec::new(),
        });
    }
    diagnostics
}

async fn record_build_failure(
    state: &AppState,
    channel: &LibraryChannel,
    generation_id: Option<&str>,
    claim_id: Option<&str>,
    code: &str,
    message: &str,
    candidate_count: i64,
) -> Result<(), ApiError> {
    let failure = LibraryChannelBuildFailure {
        channel_id: channel.id.clone(),
        generation_id: generation_id.map(str::to_owned),
        claim_id: claim_id.map(str::to_owned),
        expected_revision: channel.revision,
        error_code: code.to_owned(),
        error_message: message.to_owned(),
        candidate_count,
        now_ms: crate::media_sessions::unix_ms(),
    };
    BUILD_FAILED.fetch_add(1, Ordering::Relaxed);
    match state
        .store
        .fail_library_channel_build(&failure)
        .await
        .map_err(channel_store_error)?
    {
        ChannelBuildMutation::Applied => Ok(()),
        other => Err(build_error(other)),
    }
}

fn channel_store_error(error: plurx_core::error::StoreError) -> ApiError {
    tracing::warn!(error = ?error, "Library-channel store operation failed");
    channel_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "channel_store_unavailable",
        "Library-channel state is temporarily unavailable",
    )
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
        ChannelMutation::RequestLedgerFull => ApiError::TypedRetry {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "channel_build_busy",
            message: "the request replay ledger is full; retry after older keys expire".to_owned(),
            retry_after_seconds: 60,
        },
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
        ChannelBuildMutation::Busy => ApiError::TypedRetry {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "channel_build_busy",
            message: "a channel build is already in progress; retry shortly".to_owned(),
            retry_after_seconds: 2,
        },
        ChannelBuildMutation::Invalid => {
            ApiError::Internal("the staged generation failed its publication invariants".into())
        }
        ChannelBuildMutation::Applied => {
            ApiError::Internal("unexpected applied build mapping".into())
        }
    }
}

#[cfg(test)]
mod subject_wire_tests {
    use super::*;
    #[test]
    fn subject_absence_preserves_intent_without_changing_request_replay_hash() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/contracts/channel-subject-wire.json"
        ))
        .expect("fixture");
        for case in fixture["cases"].as_array().expect("cases") {
            let input: InputRecipe =
                serde_json::from_value(case["recipe"].clone()).expect("wire recipe");
            let original = body_digest(&input);
            let mut effective = input.recipe.clone();
            effective.subject = match &input.subject {
                SubjectPresence::Absent => Some("stored subject".into()),
                SubjectPresence::Value(subject) => subject.clone(),
            };
            let effective = effective.normalize().expect("normalization");
            assert_eq!(
                serde_json::to_value(effective.subject).expect("subject"),
                case["expected_update_subject"]
            );
            assert_eq!(
                original,
                body_digest(&input),
                "resolving an omitted field must not change retry identity"
            );
            if case["name"] == "missing" {
                assert_eq!(
                    original,
                    body_digest(&input.recipe),
                    "old client hash representation is preserved"
                );
            }
        }
    }
}
