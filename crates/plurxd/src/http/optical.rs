//! Authenticated, path-free HTTP contracts for physical optical media.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use plurx_core::domain::ItemKind;
use plurx_core::optical::{
    OpticalDisc, OpticalDriveSnapshot, OpticalDriveState, OpticalLifecycleError, OpticalMatchKind,
    OpticalProgress, OpticalProgressWrite, OpticalServiceError, OpticalTitle, PlaybackSourceRef,
};
use plurx_core::playback::{self, Decision, DeviceCaps, Force};
use plurx_core::store::{keys, stored_switch};
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use crate::state::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/drives", get(drives))
        .route("/drives/{drive_id}/disc", get(drive_disc))
        .route(
            "/drives/{drive_id}/titles/{title_id}/decision",
            post(decision),
        )
        .route("/drives/{drive_id}/eject", post(eject))
        .route("/discs/{disc_id}/titles/{title_id}", get(title_detail))
        .route(
            "/discs/{disc_id}/titles/{title_id}/progress",
            post(progress),
        )
        .route("/discs/{disc_id}/titles/{title_id}/match", put(set_match))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

#[derive(Serialize)]
struct DriveDto {
    id: String,
    name: String,
    owner_node_id: String,
    enabled: bool,
    state: OpticalDriveState,
    requirements: Vec<plurx_core::optical::HostRequirement>,
    disc: Option<DiscSummary>,
}

#[derive(Serialize)]
struct DiscSummary {
    id: String,
    media_generation: String,
    format: plurx_core::optical::OpticalFormat,
    display_title: Option<String>,
    volume_label: Option<String>,
    suggested_title_id: Option<String>,
}

#[derive(Serialize)]
struct DriveDiscDto {
    drive: DriveDto,
    titles: Vec<TitleSummary>,
}

#[derive(Serialize)]
struct TitleSummary {
    id: String,
    duration_ms: Option<i64>,
    angles: u32,
    matched_item_id: Option<i64>,
    match_kind: Option<OpticalMatchKind>,
}

#[derive(Serialize)]
struct TitleDetailDto {
    disc: OpticalDisc,
    title: OpticalTitle,
    chapters: serde_json::Value,
    progress: Option<OpticalProgress>,
}

#[derive(Deserialize)]
struct TitleQuery {
    #[serde(default = "default_angle")]
    angle: u32,
}

fn default_angle() -> u32 {
    1
}

async fn optical_enabled(state: &AppState) -> Result<bool, ApiError> {
    Ok(stored_switch(
        state
            .store
            .get_setting(keys::OPTICAL_ENABLED)
            .await?
            .as_deref(),
        false,
    ))
}

async fn authorize_play(state: &AppState, user_id: i64) -> Result<(), ApiError> {
    if state.store.optical_play_grant(user_id).await? == Some(true) {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::FORBIDDEN,
            "optical_play_forbidden",
            "this user is not granted optical playback",
        ))
    }
}

async fn require_enabled(state: &AppState) -> Result<(), ApiError> {
    if optical_enabled(state).await? {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_disabled",
            "optical playback is disabled by the saved operator setting",
        ))
    }
}

fn local_drive_id<'a>(state: &AppState, requested: &'a str) -> Result<&'a str, ApiError> {
    if state.optical.manager().snapshot(requested).is_some() {
        return Ok(requested);
    }
    let prefix = format!("{}:", state.node_id);
    let local = requested
        .strip_prefix(&prefix)
        .ok_or(ApiError::NotFound("drive"))?;
    state
        .optical
        .manager()
        .snapshot(local)
        .is_some()
        .then_some(local)
        .ok_or(ApiError::NotFound("drive"))
}

async fn drive_dto(
    state: &AppState,
    snapshot: OpticalDriveSnapshot,
    enabled: bool,
) -> Result<DriveDto, ApiError> {
    let (generation, disc_id) = match &snapshot.state {
        OpticalDriveState::Ready {
            media_generation,
            disc_id,
        }
        | OpticalDriveState::Busy {
            media_generation,
            disc_id,
            ..
        } => (Some(media_generation.clone()), Some(disc_id.clone())),
        _ => (None, None),
    };
    let disc =
        match (generation, disc_id) {
            (Some(media_generation), Some(disc_id)) => state
                .store
                .optical_disc(&disc_id)
                .await?
                .map(|disc| DiscSummary {
                    id: disc.disc_id,
                    media_generation,
                    format: disc.format,
                    display_title: disc.display_title,
                    volume_label: disc.volume_label,
                    suggested_title_id: None,
                }),
            _ => None,
        };
    let requirements = state.optical.requirements(&snapshot.id).unwrap_or_default();
    Ok(DriveDto {
        id: format!("{}:{}", snapshot.owner_node_id, snapshot.id),
        name: snapshot.label,
        owner_node_id: snapshot.owner_node_id,
        enabled,
        state: snapshot.state,
        requirements,
        disc,
    })
}

async fn drives(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<DriveDto>>, ApiError> {
    authorize_play(&state, user.id).await?;
    let enabled = optical_enabled(&state).await?;
    let mut result = Vec::new();
    for snapshot in state.optical.manager().snapshots() {
        result.push(drive_dto(&state, snapshot, enabled).await?);
    }
    Ok(Json(result))
}

async fn drive_disc(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(drive_id): Path<String>,
) -> Result<Json<DriveDiscDto>, ApiError> {
    authorize_play(&state, user.id).await?;
    let enabled = optical_enabled(&state).await?;
    let drive_id = local_drive_id(&state, &drive_id)?;
    let snapshot = state
        .optical
        .manager()
        .snapshot(drive_id)
        .ok_or(ApiError::NotFound("drive"))?;
    let disc_id = match &snapshot.state {
        OpticalDriveState::Ready { disc_id, .. } | OpticalDriveState::Busy { disc_id, .. } => {
            Some(disc_id.clone())
        }
        _ => None,
    };
    let titles = if let Some(disc_id) = disc_id {
        state
            .store
            .optical_titles(&disc_id)
            .await?
            .into_iter()
            .map(|title| TitleSummary {
                id: title.title_id,
                duration_ms: title.duration_ms,
                angles: title.angles,
                matched_item_id: title.matched_item_id,
                match_kind: title.match_kind,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(DriveDiscDto {
        drive: drive_dto(&state, snapshot, enabled).await?,
        titles,
    }))
}

async fn title_detail(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((disc_id, title_id)): Path<(String, String)>,
    Query(query): Query<TitleQuery>,
) -> Result<Json<TitleDetailDto>, ApiError> {
    authorize_play(&state, user.id).await?;
    if query.angle == 0 {
        return Err(ApiError::BadRequest("angle must be at least one".into()));
    }
    let disc = state
        .store
        .optical_disc(&disc_id)
        .await?
        .ok_or(ApiError::NotFound("disc"))?;
    let title = state
        .store
        .optical_title(&disc_id, &title_id)
        .await?
        .ok_or(ApiError::NotFound("title"))?;
    if query.angle > title.angles {
        return Err(ApiError::BadRequest(
            "angle is not available for this title".into(),
        ));
    }
    let chapters = serde_json::from_str(&title.chapters_json)?;
    let progress = state
        .store
        .optical_progress(user.id, &disc_id, &title_id, query.angle)
        .await?;
    Ok(Json(TitleDetailDto {
        disc,
        title,
        chapters,
        progress,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    expected_disc_id: String,
    media_generation: String,
    #[serde(default = "default_angle")]
    angle: u32,
    caps: DeviceCaps,
    #[serde(default)]
    force: Option<String>,
}

#[derive(Serialize)]
struct OpticalDecisionResponse {
    source: PlaybackSourceRef,
    #[serde(flatten)]
    decision: Decision,
}

async fn decision(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((drive_id, title_id)): Path<(String, String)>,
    Json(request): Json<DecisionRequest>,
) -> Result<Json<OpticalDecisionResponse>, ApiError> {
    authorize_play(&state, user.id).await?;
    require_enabled(&state).await?;
    let drive_id = local_drive_id(&state, &drive_id)?;
    let snapshot = state
        .optical
        .manager()
        .snapshot(drive_id)
        .ok_or(ApiError::NotFound("drive"))?;
    validate_ready_insertion(
        &snapshot.state,
        &request.media_generation,
        &request.expected_disc_id,
    )?;
    let title = state
        .store
        .optical_title(&request.expected_disc_id, &title_id)
        .await?
        .ok_or(ApiError::NotFound("title"))?;
    if request.angle == 0 || request.angle > title.angles {
        return Err(ApiError::BadRequest(
            "angle is not available for this title".into(),
        ));
    }
    if request.caps.v != DeviceCaps::VERSION {
        return Err(ApiError::typed(
            StatusCode::BAD_REQUEST,
            "invalid_capabilities",
            format!(
                "capabilities document version {} is not supported",
                request.caps.v
            ),
        ));
    }
    let profile = playback::DeviceProfile::from_caps_v2(&request.caps);
    let node = super::stream::render_caps(&state).await;
    let plan = playback::decide_media_facts_forced(
        &title.facts,
        &profile,
        request
            .force
            .as_deref()
            .map(Force::parse)
            .unwrap_or(Force::Auto),
        &node,
    );
    let source = PlaybackSourceRef::Optical {
        owner_node_id: snapshot.owner_node_id,
        drive_id: drive_id.to_owned(),
        media_generation: request.media_generation,
        disc_id: request.expected_disc_id,
        title_id,
        angle: request.angle,
    };
    source
        .validate()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(Json(OpticalDecisionResponse {
        source,
        decision: plan,
    }))
}

fn validate_ready_insertion(
    state: &OpticalDriveState,
    generation: &str,
    disc_id: &str,
) -> Result<(), ApiError> {
    match state {
        OpticalDriveState::Ready {
            media_generation,
            disc_id: current_disc_id,
        } if media_generation == generation && current_disc_id == disc_id => Ok(()),
        OpticalDriveState::Ready { .. } => Err(optical_conflict(
            "optical_media_changed",
            "the requested optical insertion is no longer present",
        )),
        OpticalDriveState::Busy { .. } | OpticalDriveState::Inspecting { .. } => Err(
            optical_conflict("optical_drive_busy", "the optical drive is in use"),
        ),
        OpticalDriveState::Empty => Err(optical_conflict(
            "optical_media_changed",
            "the optical drive is empty",
        )),
        OpticalDriveState::Failed { .. } => Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_reader_unavailable",
            "the optical insertion is not readable",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressRequest {
    drive_id: String,
    media_generation: String,
    session_id: String,
    #[serde(default = "default_angle")]
    angle: u32,
    position_ms: i64,
    duration_ms: Option<i64>,
    audio: Option<serde_json::Value>,
    subtitle: Option<serde_json::Value>,
    recorded_at_ms: Option<i64>,
}

async fn progress(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((disc_id, title_id)): Path<(String, String)>,
    Json(request): Json<ProgressRequest>,
) -> Result<Json<OpticalProgress>, ApiError> {
    authorize_play(&state, user.id).await?;
    let drive_id = local_drive_id(&state, &request.drive_id)?;
    state
        .optical
        .manager()
        .authorize_session(
            drive_id,
            &request.media_generation,
            &disc_id,
            &title_id,
            &request.session_id,
        )
        .map_err(lifecycle_error)?;
    let progress = state
        .store
        .put_optical_progress(&OpticalProgressWrite {
            user_id: user.id,
            disc_id,
            title_id,
            angle: request.angle,
            position_ms: request.position_ms,
            duration_ms: request.duration_ms,
            audio_selection_json: request.audio.map(|value| value.to_string()),
            subtitle_selection_json: request.subtitle.map(|value| value.to_string()),
            recorded_at_ms: request.recorded_at_ms,
        })
        .await?;
    Ok(Json(progress))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchRequest {
    matched_item_id: Option<i64>,
    match_kind: Option<OpticalMatchKind>,
}

async fn set_match(
    AdminUser(_admin): AdminUser,
    State(state): State<AppState>,
    Path((disc_id, title_id)): Path<(String, String)>,
    Json(request): Json<MatchRequest>,
) -> Result<StatusCode, ApiError> {
    if request.matched_item_id.is_some() != request.match_kind.is_some() {
        return Err(ApiError::BadRequest(
            "matched_item_id and match_kind must be present together".into(),
        ));
    }
    if let (Some(item_id), Some(kind)) = (request.matched_item_id, request.match_kind) {
        let item = state
            .store
            .get_item(item_id)
            .await?
            .ok_or(ApiError::NotFound("item"))?;
        let allowed = match kind {
            OpticalMatchKind::Movie => item.kind == ItemKind::Movie,
            OpticalMatchKind::Episode => item.kind == ItemKind::Episode,
            OpticalMatchKind::Extra => matches!(
                item.kind,
                ItemKind::Movie | ItemKind::Episode | ItemKind::Video
            ),
        };
        if !allowed {
            return Err(ApiError::BadRequest(
                "the selected item kind does not match the optical match kind".into(),
            ));
        }
    }
    if state
        .store
        .set_optical_match(
            &disc_id,
            &title_id,
            request.matched_item_id,
            request.match_kind,
        )
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("title"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EjectRequest {
    expected_disc_id: String,
    media_generation: String,
    #[serde(default)]
    stop_active: bool,
    session_id: Option<String>,
}

async fn eject(
    AdminUser(_admin): AdminUser,
    State(state): State<AppState>,
    Path(drive_id): Path<String>,
    Json(request): Json<EjectRequest>,
) -> Result<StatusCode, ApiError> {
    require_enabled(&state).await?;
    let drive_id = local_drive_id(&state, &drive_id)?;
    let snapshot = state
        .optical
        .manager()
        .snapshot(drive_id)
        .ok_or(ApiError::NotFound("drive"))?;
    let current_disc_id = match &snapshot.state {
        OpticalDriveState::Ready { disc_id, .. } | OpticalDriveState::Busy { disc_id, .. } => {
            disc_id
        }
        _ => {
            return Err(optical_conflict(
                "optical_eject_conflict",
                "the requested insertion is not available to eject",
            ));
        }
    };
    if current_disc_id != &request.expected_disc_id {
        return Err(optical_conflict(
            "optical_eject_conflict",
            "the disc changed before eject",
        ));
    }
    let stopped = request
        .stop_active
        .then_some(request.session_id.as_deref())
        .flatten();
    state
        .optical
        .eject(drive_id, &request.media_generation, stopped)
        .await
        .map_err(service_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn optical_conflict(code: &'static str, message: &'static str) -> ApiError {
    ApiError::typed(StatusCode::CONFLICT, code, message)
}

fn lifecycle_error(error: OpticalLifecycleError) -> ApiError {
    match error {
        OpticalLifecycleError::Busy => {
            optical_conflict("optical_drive_busy", "the optical drive is in use")
        }
        OpticalLifecycleError::StaleGeneration | OpticalLifecycleError::StaleDisc => {
            optical_conflict(
                "optical_media_changed",
                "the requested optical insertion is no longer present",
            )
        }
        OpticalLifecycleError::UnknownDrive => ApiError::NotFound("drive"),
        OpticalLifecycleError::Empty => {
            optical_conflict("optical_media_changed", "the optical drive is empty")
        }
        OpticalLifecycleError::NotReady | OpticalLifecycleError::InvalidIdentity => {
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_reader_unavailable",
                error.to_string(),
            )
        }
    }
}

fn service_error(error: OpticalServiceError) -> ApiError {
    match error {
        OpticalServiceError::UnknownDrive => ApiError::NotFound("drive"),
        OpticalServiceError::Lifecycle(error) => lifecycle_error(error),
        OpticalServiceError::Host(plurx_core::optical::OpticalHostError::UnsupportedHost)
        | OpticalServiceError::Host(plurx_core::optical::OpticalHostError::HelperUnavailable) => {
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_reader_unavailable",
                error.to_string(),
            )
        }
        OpticalServiceError::Host(_) | OpticalServiceError::Inspection(_) => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_read_failed",
            error.to_string(),
        ),
        OpticalServiceError::Store(error) => ApiError::Internal(error.to_string()),
    }
}
