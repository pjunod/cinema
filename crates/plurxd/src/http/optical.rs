//! Authenticated, path-free HTTP contracts for physical optical media.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use plurx_core::domain::ItemKind;
use plurx_core::optical::{
    HostRequirement, HostRequirementStatus, OpticalDisc, OpticalDriveSnapshot, OpticalDriveState,
    OpticalLifecycleError, OpticalMatchKind, OpticalProgress, OpticalProgressWrite,
    OpticalServiceError, OpticalTitle, PlaybackSourceRef,
};
use plurx_core::playback::{self, Decision, DeviceCaps, Force};
use plurx_core::store::{keys, stored_switch};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use super::peer_transport::{
    deadline_after, exact_auth_from_headers, signed_response_payload, PeerAuthMode, PeerTransport,
    PeerTransportError, RESPONSE_SIGNATURE_HEADER,
};
use crate::state::AppState;

pub(crate) const OWNER_PATH: &str = "/internal/optical/owner";
pub(crate) const MAX_OWNER_REQUEST_BYTES: usize = 128 * 1024;
const MAX_OWNER_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const OWNER_EXCHANGE_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/drives", get(drives))
        .route("/drives/{drive_id}/disc", get(drive_disc))
        .route(
            "/drives/{drive_id}/titles/{title_id}/decision",
            post(decision),
        )
        .route(
            "/drives/{drive_id}/titles/{title_id}/sessions",
            post(start_session),
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

#[derive(Serialize, Deserialize)]
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
    let local = if let Some(local) = requested.strip_prefix(&prefix) {
        local
    } else if requested.contains(':') {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_owner_unavailable",
            "this ingress cannot yet relay the request to the advertised drive owner",
        ));
    } else {
        return Err(ApiError::NotFound("drive"));
    };
    state
        .optical
        .manager()
        .snapshot(local)
        .is_some()
        .then_some(local)
        .ok_or(ApiError::NotFound("drive"))
}

enum DriveTarget {
    Local(String),
    Remote {
        owner_node_id: String,
        drive_id: String,
    },
}

async fn drive_target(state: &AppState, requested: &str) -> Result<DriveTarget, ApiError> {
    if state.optical.manager().snapshot(requested).is_some() {
        return Ok(DriveTarget::Local(requested.to_owned()));
    }
    if let Some(local) = requested.strip_prefix(&format!("{}:", state.node_id)) {
        return state
            .optical
            .manager()
            .snapshot(local)
            .is_some()
            .then(|| DriveTarget::Local(local.to_owned()))
            .ok_or(ApiError::NotFound("drive"));
    }
    let Some((owner_node_id, drive_id)) = requested.split_once(':') else {
        return Err(ApiError::NotFound("drive"));
    };
    let advertised = state
        .media_pool
        .remote_optical_drives()
        .await
        .into_iter()
        .any(|snapshot| snapshot.owner_node_id == owner_node_id && snapshot.id == drive_id);
    if !advertised {
        return Err(owner_unavailable());
    }
    Ok(DriveTarget::Remote {
        owner_node_id: owner_node_id.to_owned(),
        drive_id: drive_id.to_owned(),
    })
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerRequest {
    DriveDisc {
        drive_id: String,
    },
    Decision {
        drive_id: String,
        title_id: String,
        request: DecisionRequest,
    },
    Eject {
        drive_id: String,
        request: EjectRequest,
    },
}

impl OwnerRequest {
    fn mutates(&self) -> bool {
        matches!(self, Self::Eject { .. })
    }
}

async fn owner_peer(state: &AppState, owner_node_id: &str) -> Result<(String, String), ApiError> {
    state
        .membership
        .activity_peers()
        .await
        .map_err(|_| owner_unavailable())?
        .into_iter()
        .find(|peer| peer.node_id == owner_node_id && peer.reachable)
        .and_then(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .ok_or_else(owner_unavailable)
}

async fn relay_owner(
    state: &AppState,
    owner_node_id: &str,
    request: &OwnerRequest,
) -> Result<super::peer_transport::PeerResponse, ApiError> {
    let body =
        serde_json::to_vec(request).map_err(|error| ApiError::Internal(error.to_string()))?;
    if body.len() > MAX_OWNER_REQUEST_BYTES {
        return Err(ApiError::BadRequest(
            "optical owner request exceeds its byte limit".to_owned(),
        ));
    }
    let (node_id, base) = owner_peer(state, owner_node_id).await?;
    PeerTransport::new(state.membership.clone())
        .request(
            &node_id,
            &base,
            reqwest::Method::POST,
            OWNER_PATH,
            body,
            deadline_after(OWNER_EXCHANGE_DEADLINE),
            MAX_OWNER_RESPONSE_BYTES,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(|error| match error {
            PeerTransportError::Unreachable
            | PeerTransportError::TimedOut
            | PeerTransportError::InvalidResponse => owner_unavailable(),
        })
}

fn owner_unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "optical_owner_unavailable",
        "the advertised optical drive owner is unavailable",
    )
}

#[derive(Deserialize)]
struct OwnerWireError {
    code: String,
    message: String,
}

fn owner_wire_error(status: reqwest::StatusCode, body: &[u8]) -> ApiError {
    let Ok(error) = serde_json::from_slice::<OwnerWireError>(body) else {
        return owner_unavailable();
    };
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    let code = match error.code.as_str() {
        "optical_media_changed" => "optical_media_changed",
        "optical_drive_busy" => "optical_drive_busy",
        "optical_eject_conflict" => "optical_eject_conflict",
        "optical_reader_unavailable" => "optical_reader_unavailable",
        "optical_read_failed" => "optical_read_failed",
        "optical_disabled" => "optical_disabled",
        "invalid_capabilities" => "invalid_capabilities",
        _ => return owner_unavailable(),
    };
    let message: String = error
        .message
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect();
    ApiError::typed(status, code, message)
}

fn relayed_json(response: super::peer_transport::PeerResponse) -> Result<Response, ApiError> {
    if !response.status.is_success() {
        return Err(owner_wire_error(response.status, &response.body));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from(response.body))
        .map_err(|error| ApiError::Internal(error.to_string()))
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
    let requirements = if snapshot.owner_node_id == state.node_id {
        state.optical.requirements(&snapshot.id).unwrap_or_default()
    } else {
        vec![HostRequirement {
            id: "owner_advertisement",
            status: HostRequirementStatus::Met,
            detail: "Drive owner has a fresh optical_v1 cluster advertisement".to_owned(),
        }]
    };
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
    let mut snapshots = state.optical.manager().snapshots();
    snapshots.extend(state.media_pool.remote_optical_drives().await);
    snapshots.sort_by(|left, right| {
        left.owner_node_id
            .cmp(&right.owner_node_id)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut result = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        result.push(drive_dto(&state, snapshot, enabled).await?);
    }
    Ok(Json(result))
}

async fn drive_disc(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(drive_id): Path<String>,
) -> Result<Response, ApiError> {
    authorize_play(&state, user.id).await?;
    match drive_target(&state, &drive_id).await? {
        DriveTarget::Local(drive_id) => {
            let response = local_drive_disc(&state, &drive_id).await?;
            Ok(Json(response).into_response())
        }
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::DriveDisc { drive_id },
            )
            .await?;
            relayed_json(response)
        }
    }
}

async fn local_drive_disc(state: &AppState, drive_id: &str) -> Result<DriveDiscDto, ApiError> {
    let enabled = optical_enabled(state).await?;
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
    Ok(DriveDiscDto {
        drive: drive_dto(state, snapshot, enabled).await?,
        titles,
    })
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

#[derive(Clone, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpticalSessionRequest {
    expected_disc_id: String,
    media_generation: String,
    #[serde(default = "default_angle")]
    angle: u32,
    playback_id: String,
    request_id: String,
    #[serde(default)]
    start: f64,
    height: Option<i64>,
    audio: Option<i64>,
    subtitle_burn: Option<i64>,
    audio_offset_ms: Option<i64>,
    block_budget_secs: Option<f64>,
    caps: DeviceCaps,
}

async fn decision(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((drive_id, title_id)): Path<(String, String)>,
    Json(request): Json<DecisionRequest>,
) -> Result<Response, ApiError> {
    authorize_play(&state, user.id).await?;
    require_enabled(&state).await?;
    match drive_target(&state, &drive_id).await? {
        DriveTarget::Local(drive_id) => {
            let response = local_decision(&state, &drive_id, &title_id, request).await?;
            Ok(Json(response).into_response())
        }
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::Decision {
                    drive_id,
                    title_id,
                    request,
                },
            )
            .await?;
            relayed_json(response)
        }
    }
}

async fn local_decision(
    state: &AppState,
    drive_id: &str,
    title_id: &str,
    request: DecisionRequest,
) -> Result<OpticalDecisionResponse, ApiError> {
    require_enabled(state).await?;
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
        title_id: title_id.to_owned(),
        angle: request.angle,
    };
    source
        .validate()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(OpticalDecisionResponse {
        source,
        decision: plan,
    })
}

async fn start_session(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((drive_id, title_id)): Path<(String, String)>,
    Json(request): Json<OpticalSessionRequest>,
) -> Result<Json<super::hls::StartResponse>, ApiError> {
    authorize_play(&state, user.id).await?;
    require_enabled(&state).await?;
    if request.playback_id.is_empty()
        || request.playback_id.len() > 256
        || request.request_id.is_empty()
        || request.request_id.len() > 256
        || !request.start.is_finite()
        || request.start < 0.0
        || request.angle == 0
    {
        return Err(ApiError::BadRequest(
            "invalid optical session identity or timeline".to_owned(),
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
    let drive_id =
        match drive_target(&state, &drive_id).await? {
            DriveTarget::Local(drive_id) => drive_id,
            DriveTarget::Remote { .. } => return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_owner_unavailable",
                "optical session start must currently reach the advertised drive owner directly",
            )),
        };
    let snapshot = state
        .optical
        .manager()
        .snapshot(&drive_id)
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
    if request.angle > title.angles {
        return Err(ApiError::BadRequest(
            "angle is not available for this title".to_owned(),
        ));
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut request_hash = Sha256::new();
    request_hash.update(user.id.to_le_bytes());
    request_hash.update(
        serde_json::to_vec(&request).map_err(|error| ApiError::Internal(error.to_string()))?,
    );
    let request_digest = hex::encode(request_hash.finalize());
    let claim = state
        .optical
        .claim_playback_title_request(
            &drive_id,
            &request.media_generation,
            &request.expected_disc_id,
            &title,
            request.angle,
            &session_id,
            &request.request_id,
            &request_digest,
        )
        .map_err(service_error)?;
    let source_height = title.facts.height;
    if let plurx_core::optical::OpticalTitleClaim::Replay { session_id } = claim {
        let info = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(info) = state.transcode.recover_optical_vod(&session_id).await {
                    return info;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| {
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_drive_busy",
                "the matching optical session is still preparing",
            )
        })?;
        return Ok(Json(optical_start_response(info, source_height)));
    }
    let plurx_core::optical::OpticalTitleClaim::Claimed(lease) = claim else {
        unreachable!("replay returned above")
    };
    let requested_height = request
        .height
        .unwrap_or_else(|| source_height.unwrap_or(1080));
    if requested_height < 2 {
        return Err(ApiError::BadRequest(
            "height must be at least two".to_owned(),
        ));
    }
    let supersession_user = serde_json::json!(["user_id", user.id]).to_string();
    let item_title = state
        .store
        .optical_disc(&request.expected_disc_id)
        .await?
        .and_then(|disc| disc.display_title.or(disc.volume_label))
        .unwrap_or_else(|| title.title_id.clone());
    let info = state
        .transcode
        .start_optical_vod(
            crate::transcode::OpticalSessionRequest {
                playback_id: request.playback_id,
                start_seconds: request.start,
                target_height: requested_height,
                audio_index: request.audio.filter(|index| *index >= 0),
                subtitle_burn: request.subtitle_burn.filter(|index| *index >= 0),
                audio_offset_ms: request.audio_offset_ms.unwrap_or(0),
                block_budget_secs: request
                    .block_budget_secs
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0),
            },
            title.facts,
            title.probe_json,
            lease,
            &user.username,
            &supersession_user,
            &item_title,
            session_id,
        )
        .await
        .map_err(optical_start_error)?;
    Ok(Json(optical_start_response(info, source_height)))
}

fn optical_start_response(
    info: crate::transcode::StartInfo,
    source_height: Option<i64>,
) -> super::hls::StartResponse {
    super::hls::StartResponse {
        session_id: info.session_id,
        playlist_url: info.playlist_url,
        duration_ms: info.duration_ms,
        start_seconds: info.start_seconds,
        media_origin_ms: Some((info.media_origin_seconds * 1000.0).round() as i64),
        height: info.target_height,
        encoder: info.encoder.to_owned(),
        vod: info.vod,
        ladder: crate::transcode::advertised_ladder(source_height, info.target_height),
        prior_kbps: None,
        delivered_dynamic_range: Some("sdr".to_owned()),
        delivered_dolby_vision_profile: None,
        control: None,
        plan_notes: vec!["managed optical source: encoded VOD".to_owned()],
    }
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

#[derive(Clone, Serialize, Deserialize)]
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
    match drive_target(&state, &drive_id).await? {
        DriveTarget::Local(drive_id) => local_eject(&state, &drive_id, request).await?,
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::Eject { drive_id, request },
            )
            .await?;
            if !response.status.is_success() {
                return Err(owner_wire_error(response.status, &response.body));
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn local_eject(
    state: &AppState,
    drive_id: &str,
    request: EjectRequest,
) -> Result<(), ApiError> {
    require_enabled(state).await?;
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
    Ok(())
}

pub(crate) async fn owner(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = match serde_json::from_slice::<OwnerRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            if authorize_owner(&state, &headers, &body, false)
                .await
                .is_err()
            {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            return signed_owner_error(
                &state,
                &headers,
                ApiError::BadRequest("invalid optical owner request".to_owned()),
            )
            .await;
        }
    };
    if authorize_owner(&state, &headers, &body, request.mutates())
        .await
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.serving.is_ready() {
        return signed_owner_error(&state, &headers, owner_unavailable()).await;
    }
    match request {
        OwnerRequest::DriveDisc { drive_id } => match local_drive_disc(&state, &drive_id).await {
            Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
            Err(error) => signed_owner_error(&state, &headers, error).await,
        },
        OwnerRequest::Decision {
            drive_id,
            title_id,
            request,
        } => match local_decision(&state, &drive_id, &title_id, request).await {
            Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
            Err(error) => signed_owner_error(&state, &headers, error).await,
        },
        OwnerRequest::Eject { drive_id, request } => {
            match local_eject(&state, &drive_id, request).await {
                Ok(()) => {
                    signed_owner_json(&state, &headers, StatusCode::OK, &serde_json::json!({}))
                }
                Err(error) => signed_owner_error(&state, &headers, error).await,
            }
        }
    }
}

async fn authorize_owner(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    mutation: bool,
) -> Result<(), StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let allowed = if mutation {
        state
            .membership
            .authorize_internal_peer_voter_request(&auth, "POST", OWNER_PATH, body)
            .await
    } else {
        state
            .membership
            .authorize_internal_peer_read_request(&auth, "POST", OWNER_PATH, body)
            .await
    }
    .unwrap_or(false);
    allowed.then_some(()).ok_or(StatusCode::UNAUTHORIZED)
}

fn signed_owner_json<T: Serialize>(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    value: &T,
) -> Response {
    let body = match serde_json::to_vec(value) {
        Ok(body) => body,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    signed_owner_bytes(state, headers, status, body).unwrap_or_else(|status| status.into_response())
}

async fn signed_owner_error(state: &AppState, headers: &HeaderMap, error: ApiError) -> Response {
    let response = error.into_response();
    let status = response.status();
    let body = match axum::body::to_bytes(response.into_body(), MAX_OWNER_RESPONSE_BYTES).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    signed_owner_bytes(state, headers, status, body).unwrap_or_else(|status| status.into_response())
}

fn signed_owner_bytes(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    body: Vec<u8>,
) -> Result<Response, StatusCode> {
    let auth = exact_auth_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = signed_response_payload(status.as_u16(), &body);
    let signature = state
        .membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, OWNER_PATH, &payload)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(RESPONSE_SIGNATURE_HEADER, signature)
        .body(axum::body::Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn optical_conflict(code: &'static str, message: &'static str) -> ApiError {
    ApiError::typed(StatusCode::CONFLICT, code, message)
}

fn optical_start_error(error: String) -> ApiError {
    if let Some((code, message)) = crate::transcode::vod_refusal(&error) {
        let (status, code) = match code {
            "optical_request_conflict" => (StatusCode::CONFLICT, "optical_request_conflict"),
            "vod_audio_track_missing" | "vod_subtitle_track_missing" | "vod_invalid_height" => {
                (StatusCode::BAD_REQUEST, "optical_selection_invalid")
            }
            "vod_source_unsupported"
            | "vod_video_geometry_unknown"
            | "vod_frame_cadence_unknown" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "optical_format_unsupported",
            ),
            _ => (StatusCode::SERVICE_UNAVAILABLE, "optical_read_failed"),
        };
        return ApiError::typed(status, code, message.to_owned());
    }
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "optical_read_failed",
        error,
    )
}

fn lifecycle_error(error: OpticalLifecycleError) -> ApiError {
    match error {
        OpticalLifecycleError::Busy => {
            optical_conflict("optical_drive_busy", "the optical drive is in use")
        }
        OpticalLifecycleError::RequestConflict => optical_conflict(
            "optical_request_conflict",
            "the optical request id was reused with a different payload",
        ),
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
