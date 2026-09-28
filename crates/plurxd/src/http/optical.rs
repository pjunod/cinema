//! Authenticated, path-free HTTP contracts for physical optical media.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use plurx_core::domain::{ItemKind, MediaSessionActivation, MEDIA_SESSION_PUBLICATION_BLOCKED};
use plurx_core::optical::{
    optical_output_identity, DurableOpticalSessionSource, HostRequirement, HostRequirementStatus,
    OpticalDriveSnapshot, OpticalDriveState, OpticalLifecycleError, OpticalMatchKind,
    OpticalProgress, OpticalProgressWrite, OpticalServiceError, PlaybackSourceRef,
    OPTICAL_SESSION_PAYLOAD_V1,
};
use plurx_core::playback::{self, Decision, DeviceCaps, Force, PlaybackMediaFacts};
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
const OWNER_START_DEADLINE: Duration = Duration::from_secs(45);

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
    state: PublicOpticalDriveState,
    requirements: Vec<plurx_core::optical::HostRequirement>,
    disc: Option<DiscSummary>,
}

/// Client-visible drive state is an explicit allow-list. In particular, the
/// manager's busy state carries the exact HLS session id, which is itself a
/// bearer capability and must never be disclosed by drive discovery.
#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum PublicOpticalDriveState {
    Empty,
    Inspecting {
        media_generation: String,
    },
    Ready {
        media_generation: String,
        disc_id: String,
    },
    Busy {
        media_generation: String,
        disc_id: String,
        title_id: String,
    },
    Failed {
        media_generation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl PublicOpticalDriveState {
    fn from_manager(state: OpticalDriveState, include_diagnostics: bool) -> Self {
        match state {
            OpticalDriveState::Empty => Self::Empty,
            OpticalDriveState::Inspecting { media_generation } => {
                Self::Inspecting { media_generation }
            }
            OpticalDriveState::Ready {
                media_generation,
                disc_id,
            } => Self::Ready {
                media_generation,
                disc_id,
            },
            OpticalDriveState::Busy {
                media_generation,
                disc_id,
                title_id,
                session_id: _,
            } => Self::Busy {
                media_generation,
                disc_id,
                title_id,
            },
            OpticalDriveState::Failed {
                media_generation,
                reason,
            } => Self::Failed {
                media_generation,
                reason: include_diagnostics.then(|| public_drive_failure_reason(&reason)),
            },
        }
    }
}

pub(crate) fn public_drive_failure_reason(reason: &str) -> String {
    let normalized = reason.to_ascii_lowercase();
    if normalized.contains("protection") || normalized.contains("decrypt") {
        "The inserted disc uses protection this host cannot decrypt.".to_owned()
    } else if normalized.contains("helper") && normalized.contains("unavailable") {
        "The optical reader helper is unavailable.".to_owned()
    } else if normalized.contains("timed out") || normalized.contains("cancelled") {
        "Optical inspection did not complete.".to_owned()
    } else if normalized.contains("byte limit")
        || normalized.contains("invalid json")
        || normalized.contains("invalid reply")
    {
        "The optical reader returned an invalid response.".to_owned()
    } else if normalized.contains("canonical block device")
        || normalized.contains("canonical read-only directory")
    {
        "The configured optical source is unavailable.".to_owned()
    } else {
        "The optical drive could not inspect this disc. Check server logs.".to_owned()
    }
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
    disc: PublicOpticalDisc,
    title: PublicOpticalTitle,
    chapters: serde_json::Value,
    progress: Option<PublicOpticalProgress>,
}

/// Public title detail is an explicit allow-list. Store records also retain
/// fingerprint evidence, bounded helper diagnostics, title locators and the
/// ffprobe document used to freeze a decode plan; none of those belong on a
/// client wire response.
#[derive(Serialize)]
struct PublicOpticalDisc {
    disc_id: String,
    format: plurx_core::optical::OpticalFormat,
    volume_label: Option<String>,
    display_title: Option<String>,
}

#[derive(Serialize)]
struct PublicOpticalTitle {
    disc_id: String,
    title_id: String,
    angles: u32,
    facts: PlaybackMediaFacts,
    duration_ms: Option<i64>,
    matched_item_id: Option<i64>,
    match_kind: Option<OpticalMatchKind>,
}

#[derive(Serialize)]
struct PublicOpticalProgress {
    disc_id: String,
    title_id: String,
    angle: u32,
    position_ms: i64,
    duration_ms: Option<i64>,
    watched: bool,
    updated_at_ms: i64,
}

impl From<OpticalProgress> for PublicOpticalProgress {
    fn from(progress: OpticalProgress) -> Self {
        Self {
            disc_id: progress.disc_id,
            title_id: progress.title_id,
            angle: progress.angle,
            position_ms: progress.position_ms,
            duration_ms: progress.duration_ms,
            watched: progress.watched,
            updated_at_ms: progress.updated_at_ms,
        }
    }
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
        user_id: i64,
        drive_id: String,
    },
    Decision {
        user_id: i64,
        drive_id: String,
        title_id: String,
        request: DecisionRequest,
    },
    StartSession {
        user_id: i64,
        drive_id: String,
        title_id: String,
        request: OpticalSessionRequest,
    },
    Progress {
        user_id: i64,
        disc_id: String,
        title_id: String,
        request: ProgressRequest,
    },
    Eject {
        admin_user_id: i64,
        drive_id: String,
        request: EjectRequest,
    },
}

impl OwnerRequest {
    fn mutates(&self) -> bool {
        matches!(
            self,
            Self::StartSession { .. } | Self::Progress { .. } | Self::Eject { .. }
        )
    }

    fn deadline(&self) -> Duration {
        match self {
            Self::StartSession { .. } => OWNER_START_DEADLINE,
            _ => OWNER_EXCHANGE_DEADLINE,
        }
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
            deadline_after(request.deadline()),
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
        "optical_request_conflict" => "optical_request_conflict",
        "optical_eject_conflict" => "optical_eject_conflict",
        "optical_selection_invalid" => "optical_selection_invalid",
        "optical_format_unsupported" => "optical_format_unsupported",
        "optical_protection_unsupported" => "optical_protection_unsupported",
        "optical_reader_unavailable" => "optical_reader_unavailable",
        "optical_read_failed" => "optical_read_failed",
        "optical_play_forbidden" => "optical_play_forbidden",
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
    include_diagnostics: bool,
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
        state: PublicOpticalDriveState::from_manager(snapshot.state, include_diagnostics),
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
        result.push(drive_dto(&state, snapshot, enabled, user.is_admin).await?);
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
            let response = local_drive_disc(&state, user.id, &drive_id).await?;
            Ok(Json(response).into_response())
        }
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::DriveDisc {
                    user_id: user.id,
                    drive_id,
                },
            )
            .await?;
            relayed_json(response)
        }
    }
}

async fn local_drive_disc(
    state: &AppState,
    user_id: i64,
    drive_id: &str,
) -> Result<DriveDiscDto, ApiError> {
    authorize_play(state, user_id).await?;
    let include_diagnostics = state
        .store
        .get_user(user_id)
        .await?
        .is_some_and(|user| user.is_admin);
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
        drive: drive_dto(state, snapshot, enabled, include_diagnostics).await?,
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
        .await?
        .map(PublicOpticalProgress::from);
    Ok(Json(TitleDetailDto {
        disc: PublicOpticalDisc {
            disc_id: disc.disc_id,
            format: disc.format,
            volume_label: disc.volume_label,
            display_title: disc.display_title,
        },
        title: PublicOpticalTitle {
            disc_id: title.disc_id,
            title_id: title.title_id,
            angles: title.angles,
            facts: title.facts,
            duration_ms: title.duration_ms,
            matched_item_id: title.matched_item_id,
            match_kind: title.match_kind,
        },
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
    audio: Option<i64>,
    subtitle: Option<i64>,
}

#[derive(Serialize)]
struct OpticalDecisionResponse {
    playback_source: PlaybackSourceRef,
    #[serde(flatten)]
    decision: Decision,
    play_url: String,
    delivery: OpticalDeliveryPlan,
    source: OpticalSourceSummary,
    audio: Vec<OpticalAudioTrack>,
    subtitles: Vec<OpticalSubtitleTrack>,
    markers: Vec<serde_json::Value>,
    audio_offset_ms: i64,
    declared_offset_ms: Option<i64>,
    ladder: Vec<crate::transcode::Rung>,
    prior_kbps: Option<u32>,
    vod_indexed: bool,
    prefer_segmented: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum OpticalDeliveryPlan {
    Transcode {
        sessions_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: Option<i64>,
    },
}

#[derive(Serialize)]
struct OpticalSourceSummary {
    container: Option<String>,
    video_codec: Option<String>,
    video_profile: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    bit_depth: Option<i64>,
    hdr: Option<String>,
    hdr_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dv_profile: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dv_el_present: Option<bool>,
    bitrate: Option<i64>,
    duration_ms: Option<i64>,
}

#[derive(Serialize)]
struct OpticalAudioTrack {
    index: i64,
    codec: String,
    channels: Option<i64>,
    language: Option<String>,
    title: Option<String>,
    default: bool,
}

#[derive(Serialize)]
struct OpticalSubtitleTrack {
    index: i64,
    codec: String,
    language: Option<String>,
    title: Option<String>,
    default: bool,
    forced: bool,
    text: bool,
    native: bool,
}

fn optical_source_summary(facts: &PlaybackMediaFacts) -> OpticalSourceSummary {
    OpticalSourceSummary {
        container: facts.container.clone(),
        video_codec: facts.video_codec.clone(),
        video_profile: facts.video_profile.clone(),
        width: facts.width,
        height: facts.height,
        bit_depth: facts.bit_depth,
        hdr: facts.hdr.clone(),
        hdr_format: facts.hdr_format.clone(),
        dv_profile: facts.dolby_vision.profile,
        dv_el_present: facts.dolby_vision.el_present,
        bitrate: facts.bitrate,
        duration_ms: facts.duration_ms,
    }
}

fn optical_audio_tracks(
    facts: &PlaybackMediaFacts,
    selected: Option<i64>,
) -> Vec<OpticalAudioTrack> {
    facts
        .audio_streams
        .iter()
        .map(|track| OpticalAudioTrack {
            index: track.index,
            codec: track.codec.clone(),
            channels: track.channels,
            language: track.language.clone(),
            title: track.title.clone(),
            default: selected.map_or(track.default, |selected| selected == track.index),
        })
        .collect()
}

fn optical_subtitle_tracks(
    facts: &PlaybackMediaFacts,
    selected: Option<i64>,
) -> Vec<OpticalSubtitleTrack> {
    facts
        .subtitle_streams
        .iter()
        .map(|track| OpticalSubtitleTrack {
            index: track.index,
            codec: track.codec.clone(),
            language: track.language.clone(),
            title: track.title.clone(),
            default: selected.map_or(track.default, |selected| selected == track.index),
            forced: track.forced,
            // Optical subtitles currently travel through the admitted title
            // reader and encoder. Advertising a file sidecar or native HLS
            // rendition here would create a second, unowned physical read.
            text: false,
            native: false,
        })
        .collect()
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
            let response = local_decision(&state, user.id, &drive_id, &title_id, request).await?;
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
                    user_id: user.id,
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
    user_id: i64,
    drive_id: &str,
    title_id: &str,
    request: DecisionRequest,
) -> Result<OpticalDecisionResponse, ApiError> {
    authorize_play(state, user_id).await?;
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
        .optical_title(&request.expected_disc_id, title_id)
        .await?
        .ok_or(ApiError::NotFound("title"))?;
    if request.angle == 0 || request.angle > title.angles {
        return Err(ApiError::BadRequest(
            "angle is not available for this title".into(),
        ));
    }
    if request.audio.is_some_and(|index| {
        !title
            .facts
            .audio_streams
            .iter()
            .any(|track| track.index == index)
    }) {
        return Err(ApiError::BadRequest("unknown audio track".to_owned()));
    }
    if request.subtitle.is_some_and(|index| {
        index < -1
            || (index >= 0
                && !title
                    .facts
                    .subtitle_streams
                    .iter()
                    .any(|track| track.index == index))
    }) {
        return Err(ApiError::BadRequest("unknown subtitle track".to_owned()));
    }
    if request.subtitle.is_some_and(|index| {
        index >= 0
            && title
                .facts
                .subtitle_streams
                .iter()
                .find(|track| track.index == index)
                .is_some_and(|track| !plurx_core::tracks::is_bitmap_subtitle(&track.codec))
    }) {
        return Err(optical_text_subtitle_unavailable());
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
    let node = super::stream::render_caps(state).await;
    // The first optical producer is encoded VOD. Even a codec-compatible
    // title cannot use file direct-play or the progressive remux endpoint,
    // and claiming either would hand the client an executable plan that does
    // not exist. Copy-video optical VOD can replace this forced verdict once
    // its title-aware reader path is implemented.
    let requested_force = request
        .force
        .as_deref()
        .map(Force::parse)
        .unwrap_or(Force::Auto);
    let mut plan =
        playback::decide_media_facts_forced(&title.facts, &profile, Force::Transcode, &node);
    if plan.delivered_dynamic_range != "sdr" {
        plan.reasons.insert(
            0,
            "managed optical VOD currently delivers SDR encoded output".to_owned(),
        );
        plan.delivered_dynamic_range = "sdr";
        plan.delivered_dolby_vision_profile = None;
        plan.preserve_dolby_vision = false;
        plan.convert_dolby_vision = false;
    }
    if requested_force == Force::Original {
        plan.reasons.insert(
            0,
            "Original is not yet available for managed optical titles; using encoded VOD"
                .to_owned(),
        );
    }
    let route_drive_id = format!("{}:{}", snapshot.owner_node_id, drive_id);
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
    let sessions_url = format!(
        "/api/v1/optical/drives/{}/titles/{}/sessions",
        route_drive_id, title_id
    );
    let source_summary = optical_source_summary(&title.facts);
    let audio = optical_audio_tracks(&title.facts, request.audio);
    let subtitles =
        optical_subtitle_tracks(&title.facts, request.subtitle.filter(|index| *index >= 0));
    let ladder =
        crate::transcode::advertised_ladder(title.facts.height, title.facts.height.unwrap_or(1080));
    Ok(OpticalDecisionResponse {
        playback_source: source,
        decision: plan,
        play_url: sessions_url.clone(),
        delivery: OpticalDeliveryPlan::Transcode {
            sessions_url,
            audio: request.audio,
        },
        source: source_summary,
        audio,
        subtitles,
        markers: Vec::new(),
        audio_offset_ms: 0,
        declared_offset_ms: (title.facts.audio_offset_ms != 0)
            .then_some(title.facts.audio_offset_ms),
        ladder,
        prior_kbps: None,
        vod_indexed: false,
        prefer_segmented: Some("optical title delivery is finite HLS".to_owned()),
    })
}

async fn start_session(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((drive_id, title_id)): Path<(String, String)>,
    Json(request): Json<OpticalSessionRequest>,
) -> Result<Response, ApiError> {
    authorize_play(&state, user.id).await?;
    require_enabled(&state).await?;
    match drive_target(&state, &drive_id).await? {
        DriveTarget::Local(drive_id) => Ok(Json(
            local_start_session(&state, user.id, &drive_id, &title_id, request).await?,
        )
        .into_response()),
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::StartSession {
                    user_id: user.id,
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

async fn local_start_session(
    state: &AppState,
    user_id: i64,
    drive_id: &str,
    title_id: &str,
    request: OpticalSessionRequest,
) -> Result<super::hls::StartResponse, ApiError> {
    authorize_play(state, user_id).await?;
    require_enabled(state).await?;
    if request.playback_id.is_empty()
        || request.playback_id.len() > 128
        || request.request_id.is_empty()
        || request.request_id.len() > 128
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
        .optical_title(&request.expected_disc_id, title_id)
        .await?
        .ok_or(ApiError::NotFound("title"))?;
    if request.angle > title.angles {
        return Err(ApiError::BadRequest(
            "angle is not available for this title".to_owned(),
        ));
    }
    if request.audio.is_some_and(|index| {
        !title
            .facts
            .audio_streams
            .iter()
            .any(|track| track.index == index)
    }) {
        return Err(ApiError::BadRequest("unknown audio track".to_owned()));
    }
    if request.subtitle_burn.is_some_and(|index| {
        !title
            .facts
            .subtitle_streams
            .iter()
            .any(|track| track.index == index)
    }) {
        return Err(ApiError::BadRequest("unknown subtitle track".to_owned()));
    }
    if request.subtitle_burn.is_some_and(|index| {
        title
            .facts
            .subtitle_streams
            .iter()
            .find(|track| track.index == index)
            .is_some_and(|track| !plurx_core::tracks::is_bitmap_subtitle(&track.codec))
    }) {
        return Err(optical_text_subtitle_unavailable());
    }
    if request
        .audio_offset_ms
        .is_some_and(|value| value.unsigned_abs() > 15_000)
    {
        return Err(ApiError::BadRequest(
            "audio offset exceeds the supported range".to_owned(),
        ));
    }
    if request
        .block_budget_secs
        .is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0 || seconds > 120.0)
    {
        return Err(ApiError::BadRequest(
            "block budget exceeds the supported range".to_owned(),
        ));
    }
    if title
        .duration_ms
        .is_some_and(|duration_ms| request.start * 1000.0 >= duration_ms as f64)
    {
        return Err(ApiError::BadRequest(
            "start is outside the title timeline".to_owned(),
        ));
    }
    let source_height = title.facts.height;
    let requested_height = request
        .height
        .unwrap_or_else(|| source_height.unwrap_or(1080));
    if !(2..=4320).contains(&requested_height) {
        return Err(ApiError::BadRequest(
            "height must be between 2 and 4320".to_owned(),
        ));
    }
    let user = state.store.get_user(user_id).await?.ok_or_else(|| {
        ApiError::typed(
            StatusCode::FORBIDDEN,
            "optical_play_forbidden",
            "the optical playback user no longer exists",
        )
    })?;
    let authority = state.serving.authority();
    let admitted_generation = authority.admit().ok_or_else(owner_unavailable)?;
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
            drive_id,
            &request.media_generation,
            &request.expected_disc_id,
            &title,
            request.angle,
            &session_id,
            &request.request_id,
            &request_digest,
        )
        .map_err(service_error)?;
    let source = match &claim {
        plurx_core::optical::OpticalTitleClaim::Claimed(lease) => lease.source.clone(),
        plurx_core::optical::OpticalTitleClaim::Replay { .. } => PlaybackSourceRef::Optical {
            owner_node_id: state.node_id.clone(),
            drive_id: drive_id.to_owned(),
            media_generation: request.media_generation.clone(),
            disc_id: request.expected_disc_id.clone(),
            title_id: title.title_id.clone(),
            angle: request.angle,
        },
    };
    let audio_identity = request.audio.map(|index| index.to_string());
    let subtitle_identity = request.subtitle_burn.map(|index| index.to_string());
    let output_identity = optical_output_identity(
        &source,
        title.locator,
        audio_identity.as_deref(),
        subtitle_identity.as_deref(),
        &request_digest,
    )
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    let recipe_json = DurableOpticalSessionSource {
        version: OPTICAL_SESSION_PAYLOAD_V1,
        source,
        locator: title.locator,
        output_identity,
    }
    .encode()
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    let supersession_user = serde_json::json!(["user_id", user_id]).to_string();
    let item_title = state
        .store
        .optical_disc(&request.expected_disc_id)
        .await?
        .and_then(|disc| disc.display_title.or(disc.volume_label))
        .unwrap_or_else(|| title.title_id.clone());
    let info = match claim {
        plurx_core::optical::OpticalTitleClaim::Claimed(lease) => state
            .transcode
            .start_optical_vod(
                crate::transcode::OpticalSessionRequest {
                    playback_id: request.playback_id.clone(),
                    start_seconds: request.start,
                    target_height: requested_height,
                    audio_index: request.audio.filter(|index| *index >= 0),
                    subtitle_burn: request.subtitle_burn.filter(|index| *index >= 0),
                    audio_offset_ms: request.audio_offset_ms.unwrap_or(0),
                    block_budget_secs: request
                        .block_budget_secs
                        .filter(|seconds| seconds.is_finite() && *seconds > 0.0),
                },
                crate::vodserve::OpticalVodSource {
                    facts: title.facts,
                    probe_json: title.probe_json,
                    lease: *lease,
                },
                crate::vodserve::VodAttribution {
                    user_name: &user.username,
                    item_title: &item_title,
                    supersession_user: &supersession_user,
                },
                session_id,
            )
            .await
            .map_err(optical_start_error)?,
        plurx_core::optical::OpticalTitleClaim::Replay { session_id } => {
            tokio::time::timeout(Duration::from_secs(5), async {
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
            })?
        }
    };
    let durable_session_id = info.session_id.clone();
    let media_origin_ms = (info.media_origin_seconds * 1_000.0).round() as i64;
    let response = optical_start_response(info, source_height);
    let activation_now_ms = crate::media_sessions::unix_ms();
    let activation = MediaSessionActivation {
        incarnation_id: durable_session_id.clone(),
        session_id: durable_session_id.clone(),
        user_id,
        playback_id: request.playback_id,
        recovery_epoch: durable_session_id.clone(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: None,
        request_fingerprint: request_digest,
        owner_node_id: state.node_id.clone(),
        recipe_json,
        response_json: serde_json::to_string(&response)
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms,
        now_ms: activation_now_ms,
        lease_expires_at_ms: activation_now_ms.saturating_add(crate::media_sessions::LEASE_TTL_MS),
        expected_desired_revision: None,
    };
    let outcome = match super::hls::activate_session_under_authority(
        state.clone(),
        activation,
        None,
        None,
        authority,
        admitted_generation,
        None,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            state
                .transcode
                .stop_session(
                    &durable_session_id,
                    "optical session durable activation failed",
                )
                .await;
            return Err(error);
        }
    };
    state.media_sessions.seed_owned_lease(&outcome.route).await;
    Ok(response)
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

#[derive(Serialize, Deserialize)]
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

#[derive(Deserialize)]
#[serde(untagged)]
enum ProgressAudioSelection {
    Index(i64),
    Detail(ProgressAudioSelectionDetail),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressAudioSelectionDetail {
    index: i64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProgressSubtitleSelection {
    Index(i64),
    Detail(ProgressSubtitleSelectionDetail),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressSubtitleSelectionDetail {
    index: i64,
    #[serde(default)]
    burned: bool,
}

fn progress_audio_index(value: Option<&serde_json::Value>) -> Result<Option<i64>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let selection = serde_json::from_value::<ProgressAudioSelection>(value.clone())
        .map_err(|_| ApiError::BadRequest("invalid optical audio selection".to_owned()))?;
    let index = match selection {
        ProgressAudioSelection::Index(index) => index,
        ProgressAudioSelection::Detail(detail) => detail.index,
    };
    if index < 0 {
        return Err(ApiError::BadRequest(
            "invalid optical audio selection".to_owned(),
        ));
    }
    Ok(Some(index))
}

fn progress_subtitle_selection(
    value: Option<&serde_json::Value>,
) -> Result<Option<(i64, bool)>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let selection = serde_json::from_value::<ProgressSubtitleSelection>(value.clone())
        .map_err(|_| ApiError::BadRequest("invalid optical subtitle selection".to_owned()))?;
    let (index, burned) = match selection {
        ProgressSubtitleSelection::Index(index) => (index, false),
        ProgressSubtitleSelection::Detail(detail) => (detail.index, detail.burned),
    };
    if index < 0 {
        return Err(ApiError::BadRequest(
            "invalid optical subtitle selection".to_owned(),
        ));
    }
    Ok(Some((index, burned)))
}

async fn progress(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((disc_id, title_id)): Path<(String, String)>,
    Json(request): Json<ProgressRequest>,
) -> Result<Response, ApiError> {
    authorize_play(&state, user.id).await?;
    match drive_target(&state, &request.drive_id).await? {
        DriveTarget::Local(drive_id) => Ok(Json(
            local_progress(&state, user.id, &disc_id, &title_id, &drive_id, request).await?,
        )
        .into_response()),
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let request = ProgressRequest {
                drive_id,
                ..request
            };
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::Progress {
                    user_id: user.id,
                    disc_id,
                    title_id,
                    request,
                },
            )
            .await?;
            relayed_json(response)
        }
    }
}

async fn local_progress(
    state: &AppState,
    user_id: i64,
    disc_id: &str,
    title_id: &str,
    drive_id: &str,
    request: ProgressRequest,
) -> Result<PublicOpticalProgress, ApiError> {
    authorize_play(state, user_id).await?;
    if request.angle == 0
        || request.position_ms < 0
        || request.duration_ms.is_some_and(|duration| duration < 0)
        || request
            .recorded_at_ms
            .is_some_and(|recorded_at| recorded_at < 0)
        || request
            .duration_ms
            .is_some_and(|duration| request.position_ms > duration)
    {
        return Err(ApiError::BadRequest(
            "invalid optical progress timeline".to_owned(),
        ));
    }
    let audio_index = progress_audio_index(request.audio.as_ref())?;
    let subtitle_selection = progress_subtitle_selection(request.subtitle.as_ref())?;
    state
        .optical
        .manager()
        .authorize_session(
            drive_id,
            &request.media_generation,
            disc_id,
            title_id,
            &request.session_id,
        )
        .map_err(lifecycle_error)?;
    let route = state
        .store
        .media_session_route(&request.session_id)
        .await?
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::FORBIDDEN,
                "optical_play_forbidden",
                "the optical session is not authorized for this user",
            )
        })?;
    if route.user_id != user_id {
        return Err(ApiError::typed(
            StatusCode::FORBIDDEN,
            "optical_play_forbidden",
            "the optical session is not authorized for this user",
        ));
    }
    let durable_source = DurableOpticalSessionSource::decode(&route.recipe_json).map_err(|_| {
        ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_read_failed",
            "the optical session source could not be verified",
        )
    })?;
    let PlaybackSourceRef::Optical {
        owner_node_id,
        drive_id: source_drive_id,
        media_generation,
        disc_id: source_disc_id,
        title_id: source_title_id,
        angle,
    } = durable_source.source
    else {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "optical_read_failed",
            "the optical session source could not be verified",
        ));
    };
    if owner_node_id != state.node_id
        || source_drive_id != drive_id
        || media_generation != request.media_generation
        || source_disc_id != disc_id
        || source_title_id != title_id
        || angle != request.angle
    {
        return Err(optical_conflict(
            "optical_media_changed",
            "the progress update does not match the active optical title",
        ));
    }
    let title = state
        .store
        .optical_title(disc_id, title_id)
        .await?
        .ok_or(ApiError::NotFound("title"))?;
    if title
        .duration_ms
        .is_some_and(|duration| request.position_ms > duration)
    {
        return Err(ApiError::BadRequest(
            "optical progress exceeds the title timeline".to_owned(),
        ));
    }
    if audio_index.is_some_and(|index| {
        !title
            .facts
            .audio_streams
            .iter()
            .any(|track| track.index == index)
    }) {
        return Err(ApiError::BadRequest(
            "unknown optical audio selection".to_owned(),
        ));
    }
    if subtitle_selection.is_some_and(|(index, burned)| {
        !title.facts.subtitle_streams.iter().any(|track| {
            track.index == index
                && (!burned || plurx_core::tracks::is_bitmap_subtitle(&track.codec))
        })
    }) {
        return Err(ApiError::BadRequest(
            "unknown optical subtitle selection".to_owned(),
        ));
    }
    let progress = state
        .store
        .put_optical_progress(&OpticalProgressWrite {
            user_id,
            disc_id: disc_id.to_owned(),
            title_id: title_id.to_owned(),
            angle: request.angle,
            position_ms: request.position_ms,
            duration_ms: request.duration_ms,
            audio_selection_json: request.audio.map(|value| value.to_string()),
            subtitle_selection_json: request.subtitle.map(|value| value.to_string()),
            recorded_at_ms: request.recorded_at_ms,
        })
        .await?;
    Ok(progress.into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchRequest {
    matched_item_id: Option<OpticalItemId>,
    match_kind: Option<OpticalMatchKind>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OpticalItemId {
    Number(i64),
    Text(String),
}

impl OpticalItemId {
    fn parse(self) -> Result<i64, ApiError> {
        let value = match self {
            Self::Number(value) => value,
            Self::Text(value) => value
                .parse::<i64>()
                .map_err(|_| ApiError::BadRequest("invalid matched item id".to_owned()))?,
        };
        (value > 0)
            .then_some(value)
            .ok_or_else(|| ApiError::BadRequest("invalid matched item id".to_owned()))
    }
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
    let matched_item_id = request
        .matched_item_id
        .map(OpticalItemId::parse)
        .transpose()?;
    if let (Some(item_id), Some(kind)) = (matched_item_id, request.match_kind) {
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
        .set_optical_match(&disc_id, &title_id, matched_item_id, request.match_kind)
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
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(drive_id): Path<String>,
    Json(request): Json<EjectRequest>,
) -> Result<StatusCode, ApiError> {
    require_enabled(&state).await?;
    match drive_target(&state, &drive_id).await? {
        DriveTarget::Local(drive_id) => local_eject(&state, admin.id, &drive_id, request).await?,
        DriveTarget::Remote {
            owner_node_id,
            drive_id,
        } => {
            let response = relay_owner(
                &state,
                &owner_node_id,
                &OwnerRequest::Eject {
                    admin_user_id: admin.id,
                    drive_id,
                    request,
                },
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
    admin_user_id: i64,
    drive_id: &str,
    request: EjectRequest,
) -> Result<(), ApiError> {
    if !state
        .store
        .get_user(admin_user_id)
        .await?
        .is_some_and(|user| user.is_admin)
    {
        return Err(ApiError::typed(
            StatusCode::FORBIDDEN,
            "optical_play_forbidden",
            "optical eject requires an administrator",
        ));
    }
    require_enabled(state).await?;
    let snapshot = state
        .optical
        .manager()
        .snapshot(drive_id)
        .ok_or(ApiError::NotFound("drive"))?;
    let active_session = eject_target(&snapshot.state, &request)?;
    if let Some(session_id) = active_session {
        let status = super::hls::release_with_terminal(
            state.clone(),
            session_id.clone(),
            crate::vodserve::Terminal::AdminStop,
            "stopped by admin before optical eject",
        )
        .await;
        if status != StatusCode::NO_CONTENT {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_drive_busy",
                "the active optical session is still settling; retry eject shortly",
            ));
        }
        // Durable terminal projection intentionally detaches physical cleanup.
        // Rejoin that exact cleanup before asking the drive to open its tray.
        let stopped = state
            .transcode
            .stop_session(&session_id, "stopped by admin before optical eject")
            .await;
        if !stopped
            && matches!(
                state.optical.manager().snapshot(drive_id).map(|row| row.state),
                Some(OpticalDriveState::Busy { session_id: current, .. }) if current == session_id
            )
        {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_drive_busy",
                "the active optical reader has not released the drive",
            ));
        }
    }
    state
        .optical
        .eject(drive_id, &request.media_generation)
        .await
        .map_err(service_error)?;
    Ok(())
}

/// Validate every stale-confirmation fence before stopping an active reader.
/// The manager repeats the generation check at the eventual hardware call,
/// but that later fence cannot undo a session stop performed for an old UI
/// confirmation.
fn eject_target(
    state: &OpticalDriveState,
    request: &EjectRequest,
) -> Result<Option<String>, ApiError> {
    match state {
        OpticalDriveState::Ready {
            media_generation,
            disc_id,
        } => {
            if media_generation != &request.media_generation || disc_id != &request.expected_disc_id
            {
                return Err(optical_conflict(
                    "optical_eject_conflict",
                    "the disc changed before eject",
                ));
            }
            Ok(None)
        }
        OpticalDriveState::Busy {
            media_generation,
            disc_id,
            session_id,
            ..
        } => {
            if media_generation != &request.media_generation || disc_id != &request.expected_disc_id
            {
                return Err(optical_conflict(
                    "optical_eject_conflict",
                    "the disc changed before eject",
                ));
            }
            if !request.stop_active || request.session_id.as_deref() != Some(session_id.as_str()) {
                return Err(optical_conflict(
                    "optical_drive_busy",
                    "stop-and-eject requires the exact active optical session",
                ));
            }
            Ok(Some(session_id.clone()))
        }
        OpticalDriveState::Failed {
            media_generation: Some(media_generation),
            ..
        } if media_generation == &request.media_generation
            && request.expected_disc_id.is_empty() =>
        {
            Ok(None)
        }
        _ => Err(optical_conflict(
            "optical_eject_conflict",
            "the requested insertion is not available to eject",
        )),
    }
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
        OwnerRequest::DriveDisc { user_id, drive_id } => {
            match local_drive_disc(&state, user_id, &drive_id).await {
                Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
                Err(error) => signed_owner_error(&state, &headers, error).await,
            }
        }
        OwnerRequest::Decision {
            user_id,
            drive_id,
            title_id,
            request,
        } => match local_decision(&state, user_id, &drive_id, &title_id, request).await {
            Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
            Err(error) => signed_owner_error(&state, &headers, error).await,
        },
        OwnerRequest::StartSession {
            user_id,
            drive_id,
            title_id,
            request,
        } => match local_start_session(&state, user_id, &drive_id, &title_id, request).await {
            Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
            Err(error) => signed_owner_error(&state, &headers, error).await,
        },
        OwnerRequest::Progress {
            user_id,
            disc_id,
            title_id,
            request,
        } => {
            let drive_id = request.drive_id.clone();
            match local_progress(&state, user_id, &disc_id, &title_id, &drive_id, request).await {
                Ok(response) => signed_owner_json(&state, &headers, StatusCode::OK, &response),
                Err(error) => signed_owner_error(&state, &headers, error).await,
            }
        }
        OwnerRequest::Eject {
            admin_user_id,
            drive_id,
            request,
        } => match local_eject(&state, admin_user_id, &drive_id, request).await {
            Ok(()) => signed_owner_json(&state, &headers, StatusCode::OK, &serde_json::json!({})),
            Err(error) => signed_owner_error(&state, &headers, error).await,
        },
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

fn optical_text_subtitle_unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::UNPROCESSABLE_ENTITY,
        "vod_subtitle_burn_unavailable",
        "This optical subtitle format cannot be rendered without reopening the physical title. Choose a DVD subpicture or Blu-ray PGS track.",
    )
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
            "vod_subtitle_burn_unavailable" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_subtitle_burn_unavailable",
            ),
            _ => (StatusCode::SERVICE_UNAVAILABLE, "optical_read_failed"),
        };
        return ApiError::typed(status, code, message.to_owned());
    }
    tracing::warn!(%error, "optical VOD start failed");
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "optical_read_failed",
        "the optical drive could not start this title",
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
        OpticalServiceError::Host(plurx_core::optical::OpticalHostError::ProtectionUnsupported) => {
            ApiError::typed(
                StatusCode::UNPROCESSABLE_ENTITY,
                "optical_protection_unsupported",
                "the inserted disc is protected and the configured reader cannot decrypt it",
            )
        }
        OpticalServiceError::Host(_) | OpticalServiceError::Inspection(_) => {
            tracing::warn!(%error, "optical reader operation failed");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "optical_read_failed",
                "the optical drive could not read this title",
            )
        }
        OpticalServiceError::Store(error) => ApiError::Internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        eject_target, optical_audio_tracks, optical_subtitle_tracks, progress_audio_index,
        progress_subtitle_selection, EjectRequest, PublicOpticalDriveState,
    };
    use plurx_core::domain::{AudioStream, SubtitleStream};
    use plurx_core::optical::OpticalDriveState;
    use plurx_core::playback::PlaybackMediaFacts;

    #[test]
    fn public_busy_state_never_serializes_the_playback_capability() {
        let state = PublicOpticalDriveState::from_manager(
            OpticalDriveState::Busy {
                media_generation: "generation-a".to_owned(),
                disc_id: "disc-a".to_owned(),
                title_id: "title-a".to_owned(),
                session_id: "secret-session-capability".to_owned(),
            },
            true,
        );

        let json = serde_json::to_value(state).expect("serialize public drive state");
        assert_eq!(json["state"], "busy");
        assert_eq!(json["title_id"], "title-a");
        assert!(json.get("session_id").is_none());
        assert!(!json.to_string().contains("secret-session-capability"));
    }

    #[test]
    fn failed_drive_diagnostics_are_admin_only() {
        let public = PublicOpticalDriveState::from_manager(
            OpticalDriveState::Failed {
                media_generation: Some("generation-a".to_owned()),
                reason: "sensitive host diagnostic".to_owned(),
            },
            false,
        );
        let admin = PublicOpticalDriveState::from_manager(
            OpticalDriveState::Failed {
                media_generation: Some("generation-a".to_owned()),
                reason: "sensitive host diagnostic".to_owned(),
            },
            true,
        );

        let public_json = serde_json::to_value(public).expect("serialize public state");
        let admin_json = serde_json::to_value(admin).expect("serialize admin state");
        assert!(public_json.get("reason").is_none());
        assert_eq!(
            admin_json["reason"],
            "The optical drive could not inspect this disc. Check server logs."
        );
        assert!(!admin_json.to_string().contains("sensitive host diagnostic"));
    }

    #[test]
    fn eject_validates_generation_before_returning_an_active_session() {
        let state = OpticalDriveState::Busy {
            media_generation: "generation-new".to_owned(),
            disc_id: "disc-a".to_owned(),
            title_id: "title-a".to_owned(),
            session_id: "session-new".to_owned(),
        };
        let stale = EjectRequest {
            expected_disc_id: "disc-a".to_owned(),
            media_generation: "generation-old".to_owned(),
            stop_active: true,
            session_id: Some("session-new".to_owned()),
        };
        assert!(eject_target(&state, &stale).is_err());

        let current = EjectRequest {
            media_generation: "generation-new".to_owned(),
            ..stale
        };
        assert_eq!(
            eject_target(&state, &current).expect("current eject target"),
            Some("session-new".to_owned())
        );
    }

    #[test]
    fn failed_drive_recovery_eject_is_generation_fenced() {
        let state = OpticalDriveState::Failed {
            media_generation: Some("generation-new".to_owned()),
            reason: "private diagnostic".to_owned(),
        };
        let request = EjectRequest {
            expected_disc_id: String::new(),
            media_generation: "generation-old".to_owned(),
            stop_active: false,
            session_id: None,
        };
        assert!(eject_target(&state, &request).is_err());
    }

    #[test]
    fn optical_tracks_preserve_semantic_ffmpeg_indexes() {
        let facts = PlaybackMediaFacts {
            audio_streams: vec![
                AudioStream {
                    index: 2,
                    codec: "aac".to_owned(),
                    default: true,
                    ..AudioStream::default()
                },
                AudioStream {
                    index: 7,
                    codec: "ac3".to_owned(),
                    ..AudioStream::default()
                },
            ],
            subtitle_streams: vec![SubtitleStream {
                index: 5,
                codec: "hdmv_pgs_subtitle".to_owned(),
                default: true,
                ..SubtitleStream::default()
            }],
            ..PlaybackMediaFacts::default()
        };

        let audio = optical_audio_tracks(&facts, Some(7));
        let subtitles = optical_subtitle_tracks(&facts, Some(5));

        assert_eq!(
            audio.iter().map(|track| track.index).collect::<Vec<_>>(),
            [2, 7]
        );
        assert!(!audio[0].default);
        assert!(audio[1].default);
        assert_eq!(subtitles[0].index, 5);
        assert!(subtitles[0].default);
    }

    #[test]
    fn progress_selections_accept_native_and_web_wire_shapes() {
        assert_eq!(
            progress_audio_index(Some(&serde_json::json!(7))).expect("native audio selection"),
            Some(7)
        );
        assert_eq!(
            progress_audio_index(Some(&serde_json::json!({ "index": 7 })))
                .expect("web audio selection"),
            Some(7)
        );
        assert_eq!(
            progress_subtitle_selection(Some(&serde_json::json!(5)))
                .expect("native subtitle selection"),
            Some((5, false))
        );
        assert_eq!(
            progress_subtitle_selection(Some(&serde_json::json!({
                "index": 5,
                "burned": true
            })))
            .expect("web subtitle selection"),
            Some((5, true))
        );
    }

    #[test]
    fn progress_selections_reject_negative_or_ambiguous_shapes() {
        assert!(progress_audio_index(Some(&serde_json::json!(-1))).is_err());
        assert!(
            progress_audio_index(Some(&serde_json::json!({ "index": 7, "extra": true }))).is_err()
        );
        assert!(progress_subtitle_selection(Some(&serde_json::json!(-1))).is_err());
        assert!(progress_subtitle_selection(Some(&serde_json::json!({
            "index": 5,
            "burned": false,
            "extra": true
        })))
        .is_err());
    }
}
