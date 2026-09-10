//! Public HDHomeRun readiness and sanitized channel lineup.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use super::peer_transport::{deadline_after, PeerAuthMode, PeerTransport, PeerTransportError};
use crate::live_tv::{
    capability_owner, guide, GuideWindow, LiveTvActivateRequest, LiveTvActivated, LiveTvConfig,
    LiveTvDrainAck, LiveTvError, LiveTvGuide, LiveTvResourceRequest, LiveTvSnapshot,
    LiveTvStartRequest, LiveTvStartRequestV2, LiveTvStopRequest, SnapshotFreshness,
    SnapshotRequest, ACTIVATE_PATH, GUIDE_PATH, MAX_SNAPSHOT_BYTES, RESOURCE_PATH, SNAPSHOT_PATH,
    START_PATH, START_V2_PATH, STOP_PATH,
};
use crate::live_tv_delivery::LivePlaybackRequest;
use crate::state::AppState;

const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(25);
const START_EXCHANGE_ATTEMPT: Duration = Duration::from_secs(17);
const START_EXCHANGE_TOTAL: Duration = Duration::from_secs(24);
const CONTROL_EXCHANGE_DEADLINE: Duration = Duration::from_secs(5);
const DRAIN_EXCHANGE_DEADLINE: Duration = Duration::from_secs(20);
const RESOURCE_EXCHANGE_DEADLINE: Duration = Duration::from_secs(12);
const MAX_START_RESPONSE_BYTES: usize = 32 * 1024;
const GUIDE_EXCHANGE_DEADLINE: Duration = Duration::from_secs(25);
const MAX_GUIDE_REQUEST_HOURS: u8 = 72;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LiveTvReadinessCheck {
    pub(crate) id: &'static str,
    pub(crate) ready: bool,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LiveTvReadiness {
    pub(crate) ready: bool,
    pub(crate) enabled: bool,
    pub(crate) owner_node_id: String,
    pub(crate) generation: i64,
    pub(crate) checks: Vec<LiveTvReadinessCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<LiveTvSnapshot>,
}

#[derive(Serialize)]
pub(crate) struct LiveTvChannelsResponse {
    pub(crate) freshness: SnapshotFreshness,
    pub(crate) age_seconds: u64,
    pub(crate) last_success_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_error: Option<String>,
    pub(crate) channels: Vec<crate::live_tv::LiveTvChannel>,
}

#[derive(Deserialize)]
pub(crate) struct GuideQuery {
    from: Option<i64>,
    hours: Option<u8>,
}

/// The public guide read. It serves the owner's cache clipped to the requested
/// window and never triggers a fetch, so it is always fast and always answers
/// — including with `unavailable`, which is a rendered state and not an error.
/// Every failure mode here is one a client draws rather than one it retries.
pub(crate) async fn guide_document(
    _user: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<GuideQuery>,
) -> Result<Json<LiveTvGuide>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    if !config.enabled {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "live_tv_disabled",
            "Live TV is disabled; an administrator can enable it in Settings → Developer",
        ));
    }
    let window = requested_window(&config, query);
    Ok(Json(owner_guide(&state, &config, window).await))
}

fn requested_window(config: &LiveTvConfig, query: GuideQuery) -> GuideWindow {
    // An hour back by default so the programme that started before the page
    // opened is present: a grid whose first cell is always cut off reads as
    // broken rather than as correct.
    let start = query
        .from
        .unwrap_or_else(|| crate::live_tv::unix_seconds() - 3600);
    let hours = query
        .hours
        .unwrap_or(config.guide_hours)
        .clamp(1, MAX_GUIDE_REQUEST_HOURS);
    GuideWindow {
        start,
        end: start.saturating_add(i64::from(hours) * 3600),
    }
}

/// Owner-side read, or a relay of the owner's public answer.
///
/// Deliberately **not** gated on the Live TV protocol capability. An owner
/// that predates the guide answers 404 to the internal path, and the honest
/// rendering of that is `unavailable` with a sentence saying so — not
/// `live_tv_protocol_unready`, which would take the whole feature down across
/// a mixed fleet mid-rollout over a read-only extra.
pub(crate) async fn owner_guide(
    state: &AppState,
    config: &LiveTvConfig,
    window: GuideWindow,
) -> LiveTvGuide {
    if config.owner_node_id == state.node_id {
        return state.live_tv.local_guide(config, window).await;
    }
    if let Some(cached) = state.live_tv.relayed_guide(config.generation).await {
        return cached.clipped(&window);
    }
    let relayed = relay_guide(state, config).await;
    match relayed {
        Ok(guide) => {
            state
                .live_tv
                .remember_relayed_guide(config.generation, guide.clone())
                .await;
            guide.clipped(&window)
        }
        Err(message) => LiveTvGuide::unavailable(config.guide_source, window, Some(message)),
    }
}

async fn relay_guide(state: &AppState, config: &LiveTvConfig) -> Result<LiveTvGuide, String> {
    if !state.membership.is_replicated() {
        return Err("the selected owner is not this single-node server".to_owned());
    }
    let peer = state
        .membership
        .activity_peers()
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|peer| peer.node_id == config.owner_node_id)
        .filter(|peer| peer.reachable)
        .and_then(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .ok_or_else(|| "the tuner owner is not a reachable committed voter".to_owned())?;
    let body = serde_json::to_vec(&SnapshotRequest {
        generation: config.generation,
        force: false,
        probe_graph: false,
    })
    .map_err(|error| error.to_string())?;
    let response = PeerTransport::new(state.membership.clone())
        .request(
            &peer.0,
            &peer.1,
            reqwest::Method::POST,
            GUIDE_PATH,
            body,
            deadline_after(GUIDE_EXCHANGE_DEADLINE),
            MAX_SNAPSHOT_BYTES,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(|error| sanitize_public_error(&peer_error(error).to_string()))?;
    if response.status == StatusCode::NOT_FOUND {
        return Err("the tuner owner does not serve a programme guide yet".to_owned());
    }
    if !response.status.is_success() {
        return Err(format!(
            "the tuner owner returned HTTP {}",
            response.status.as_u16()
        ));
    }
    serde_json::from_slice::<LiveTvGuide>(&response.body)
        .map_err(|_| "the tuner owner returned an invalid guide".to_owned())
}

/// Admin: force a refresh now rather than waiting for the loop. Bounded by the
/// same admission semaphore the lineup's forced refresh uses.
pub(crate) async fn refresh_guide(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<LiveTvGuide>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    if config.owner_node_id != state.node_id {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "a guide refresh runs on the tuner owner; ask that node",
        ));
    }
    let guide = state
        .live_tv
        .refresh_guide(&config, true)
        .await
        .map_err(api_error)?;
    let window = guide.window.clone();
    Ok(Json(guide.clipped(&window)))
}

/// Advisory only. Every row here says what would have to be true for the guide
/// to work, and whether it is true right now — and none of them refuse a
/// change. `guide_source` is a save like any other save.
pub(crate) async fn guide_readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<LiveTvGuideReadiness>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    let owner_is_local = config.owner_node_id == state.node_id;
    let mut checks = Vec::new();
    checks.push(LiveTvReadinessCheck {
        id: "live_tv_enabled",
        ready: config.enabled,
        message: if config.enabled {
            "Live TV is enabled, so the owner is running a refresh loop.".to_owned()
        } else {
            "Live TV is off. The guide is saved but nothing fetches it until Live TV is on."
                .to_owned()
        },
    });
    checks.push(LiveTvReadinessCheck {
        id: "guide_source",
        ready: config.guide_source != crate::live_tv::GuideSource::Off,
        message: match config.guide_source {
            crate::live_tv::GuideSource::Off => {
                "No source is selected, so channel rows show number and callsign only.".to_owned()
            }
            crate::live_tv::GuideSource::HdHomeRun => {
                "The HDHomeRun guide service is selected.".to_owned()
            }
            crate::live_tv::GuideSource::Xmltv => "An XMLTV document is selected.".to_owned(),
        },
    });
    checks.push(match config.guide_source {
        crate::live_tv::GuideSource::HdHomeRun => LiveTvReadinessCheck {
            id: "outbound_host",
            ready: config.device_ipv4.is_some(),
            message: "The owner node needs outbound HTTPS to api.hdhomerun.com and a tuner address to read DeviceAuth from. plurx sends that credential over TLS and never stores it."
                .to_owned(),
        },
        crate::live_tv::GuideSource::Xmltv => LiveTvReadinessCheck {
            id: "outbound_host",
            ready: crate::live_tv::validate_xmltv_url(&config.xmltv_url).is_ok(),
            message: "The owner node needs to be able to fetch the XMLTV URL you gave. Nothing else is contacted."
                .to_owned(),
        },
        crate::live_tv::GuideSource::Off => LiveTvReadinessCheck {
            id: "outbound_host",
            ready: true,
            message: "No outbound request is made while the source is off.".to_owned(),
        },
    });
    checks.push(LiveTvReadinessCheck {
        id: "owner_node",
        ready: owner_is_local || state.membership.is_replicated(),
        message: if owner_is_local {
            "This node owns the tuner, so it fetches the guide and other nodes relay its answer."
                .to_owned()
        } else {
            format!(
                "Node {} owns the tuner and fetches the guide; this node relays its answer.",
                config.owner_node_id
            )
        },
    });
    // The same horizon a client asks for, so `programmes` is the depth the
    // guide actually has. A zero-width window here counted only what is on air
    // this second, which reads as roughly one row per channel however deep the
    // guide is — the opposite of what an operator opens this panel to learn.
    let now = crate::live_tv::unix_seconds();
    let guide = owner_guide(
        &state,
        &config,
        GuideWindow {
            start: now - 3600,
            end: now.saturating_add(i64::from(config.guide_hours) * 3600),
        },
    )
    .await;
    let programmes = guide.total_programmes();
    Ok(Json(LiveTvGuideReadiness {
        // Advisory: this is what the operator is told, never what they are
        // held to. Nothing consults it before a save.
        advisory: true,
        source: config.guide_source.as_str().to_owned(),
        guide_hours: config.guide_hours,
        freshness: guide.freshness,
        age_seconds: guide.age_seconds,
        fetched_at: guide.fetched_at,
        refresh_error: guide.refresh_error,
        matched_channels: guide.matched_channels,
        lineup_channels: guide.lineup_channels,
        programmes,
        refresh_interval_seconds: guide::GUIDE_REFRESH_INTERVAL.as_secs(),
        checks,
    }))
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LiveTvGuideReadiness {
    pub(crate) advisory: bool,
    pub(crate) source: String,
    pub(crate) guide_hours: u8,
    pub(crate) freshness: crate::live_tv::GuideFreshness,
    pub(crate) age_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fetched_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_error: Option<String>,
    pub(crate) matched_channels: usize,
    pub(crate) lineup_channels: usize,
    pub(crate) programmes: usize,
    pub(crate) refresh_interval_seconds: u64,
    pub(crate) checks: Vec<LiveTvReadinessCheck>,
}

#[derive(Deserialize)]
struct WireError {
    code: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicLiveTvStart {
    #[serde(default)]
    playback: Option<LivePlaybackRequest>,
}

pub(crate) async fn start_session(
    Path(channel): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Option<Json<PublicLiveTvStart>>,
) -> Result<Json<LiveTvActivated>, ApiError> {
    let ingress_generation = state.serving.authority().admit().ok_or_else(|| {
        ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            crate::serving_fence::SERVING_FENCED_MESSAGE,
        )
    })?;
    let config = state.live_tv.config().await.map_err(api_error)?;
    if !config.enabled {
        return Err(api_error(LiveTvError::Disabled(
            "Live TV is disabled; an administrator can enable it in Settings → Developer".into(),
        )));
    }
    let mut playback = body.and_then(|Json(body)| body.playback);
    if let Some(request) = &playback {
        request.validate().map_err(|message| {
            ApiError::typed(StatusCode::BAD_REQUEST, "invalid_request", message)
        })?;
    }
    let owner_protocols = owner_snapshot(&state, &config, false, false)
        .await
        .map(|snapshot| snapshot.start_protocols)
        .unwrap_or_default();
    if !owner_protocols.contains(&2) {
        playback = None;
    }
    let request = LiveTvStartRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        source_node_id: state.node_id.clone(),
        user_id: user.id,
        user_name: user.username,
        request_id: uuid::Uuid::new_v4().to_string(),
        channel_id: channel,
        config_generation: config.generation,
        source_serving_generation: ingress_generation,
        playback,
    };
    let provisional = owner_start(&state, &config, &request).await?;
    let activation = LiveTvActivateRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        capability: provisional.capability.clone(),
        activation_token: provisional.activation_token.clone(),
        config_generation: config.generation,
        source_serving_generation: ingress_generation,
    };
    let activated = match owner_activate(&state, &config, &activation).await {
        Ok(activated) => activated,
        Err(error) => {
            let _ = owner_stop(&state, &config.owner_node_id, &provisional.capability).await;
            return Err(error);
        }
    };
    let current = state.live_tv.config().await.map_err(api_error)?;
    if current != config || !state.serving.authority().is_current(ingress_generation) {
        let _ = owner_stop(&state, &config.owner_node_id, &provisional.capability).await;
        return Err(api_error(LiveTvError::OwnerUnavailable(
            "Live TV changed while the session was starting; press Watch again".into(),
        )));
    }
    Ok(Json(activated))
}

pub(crate) async fn playlist(
    Path(capability): Path<String>,
    State(state): State<AppState>,
) -> Result<Response<Body>, ApiError> {
    owner_resource(&state, LiveTvResourceRequest::Playlist { capability }).await
}

pub(crate) async fn segment(
    Path((capability, segment)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Response<Body>, ApiError> {
    if segment == "init.mp4" {
        return owner_resource(&state, LiveTvResourceRequest::Init { capability }).await;
    }
    let (value, fragment) = segment
        .strip_prefix("segment-")
        .and_then(|value| {
            value
                .strip_suffix(".ts")
                .map(|value| (value, false))
                .or_else(|| value.strip_suffix(".m4s").map(|value| (value, true)))
        })
        .ok_or(ApiError::NotFound("live segment"))?;
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiError::NotFound("live segment"));
    }
    let sequence = value
        .parse()
        .map_err(|_| ApiError::NotFound("live segment"))?;
    let request = if fragment {
        LiveTvResourceRequest::Fragment {
            capability,
            sequence,
        }
    } else {
        LiveTvResourceRequest::Segment {
            capability,
            sequence,
        }
    };
    owner_resource(&state, request).await
}

pub(crate) async fn session_status(
    Path(capability): Path<String>,
    State(state): State<AppState>,
) -> Result<Response<Body>, ApiError> {
    owner_resource(&state, LiveTvResourceRequest::Status { capability }).await
}

pub(crate) async fn keepalive(
    Path(capability): Path<String>,
    State(state): State<AppState>,
) -> Result<Response<Body>, ApiError> {
    owner_resource(&state, LiveTvResourceRequest::Keepalive { capability }).await
}

pub(crate) async fn stop_session(
    Path(capability): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let owner = capability_owner(&capability).map_err(api_error)?;
    owner_stop(&state, &owner, &capability).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<LiveTvReadiness>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    Ok(Json(readiness_for_config(&state, &config, false).await))
}

pub(crate) async fn refresh_readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<LiveTvReadiness>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    Ok(Json(readiness_for_config(&state, &config, true).await))
}

pub(crate) async fn channels(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<LiveTvChannelsResponse>, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    if !config.enabled {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "live_tv_disabled",
            "Live TV is disabled; an administrator can enable it in Settings → Developer",
        ));
    }
    let snapshot = owner_snapshot(&state, &config, false, false)
        .await
        .map_err(api_error)?;
    Ok(Json(LiveTvChannelsResponse {
        freshness: snapshot.freshness,
        age_seconds: snapshot.age_seconds,
        last_success_at: snapshot.last_success_at,
        refresh_error: snapshot.refresh_error,
        channels: snapshot.channels,
    }))
}

pub(crate) async fn readiness_for_config(
    state: &AppState,
    config: &LiveTvConfig,
    force: bool,
) -> LiveTvReadiness {
    let mut checks = Vec::with_capacity(8);
    let static_result = config.validate_static();
    checks.push(LiveTvReadinessCheck {
        id: "configuration",
        ready: static_result.is_ok(),
        message: static_result
            .as_ref()
            .map(|()| "The saved address, owner, session limit, and output height are valid".into())
            .unwrap_or_else(|error| error.to_string()),
    });

    let protocol = state.membership.live_tv_protocol_pending_nodes().await;
    let protocol_ready = matches!(&protocol, Ok(nodes) if nodes.is_empty());
    checks.push(LiveTvReadinessCheck {
        id: "cluster_protocol",
        ready: protocol_ready,
        message: match protocol {
            Ok(nodes) if nodes.is_empty() => {
                "Every active serving node publishes the live-TV v1 protocol".to_owned()
            }
            Ok(nodes) => format!(
                "For consistent quality selection, start, restart, upgrade, or remove these nodes: {}. This check is advisory and does not block Live TV",
                nodes.join(", ")
            ),
            Err(error) => format!(
                "Cannot prove live-TV cluster compatibility: {error}. This check is advisory and does not block Live TV"
            ),
        },
    });

    let serving_ready = state.serving.is_ready();
    checks.push(LiveTvReadinessCheck {
        id: "serving_authority",
        ready: serving_ready,
        message: if serving_ready {
            "This node currently holds quorum serving authority".to_owned()
        } else {
            crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned()
        },
    });

    let transition_ready = config.admission_ready();
    checks.push(LiveTvReadinessCheck {
        id: "owner_transition",
        ready: transition_ready,
        message: if transition_ready {
            "There is no unresolved tuner-owner cleanup"
                .to_owned()
        } else {
            format!(
                "Prior owner {} must acknowledge cleanup, or an administrator must stop it and confirm recovery in Developer settings",
                config.transition_from_owner_node_id
            )
        },
    });

    if static_result.is_err() || !serving_ready || !transition_ready {
        return LiveTvReadiness {
            ready: false,
            enabled: config.enabled,
            owner_node_id: config.owner_node_id.clone(),
            generation: config.generation,
            checks,
            snapshot: None,
        };
    }

    let snapshot = owner_snapshot(state, config, force, true).await;
    let owner_ready = snapshot.is_ok();
    checks.push(LiveTvReadinessCheck {
        id: "owner_network",
        ready: owner_ready,
        message: snapshot
            .as_ref()
            .map(|_| {
                format!(
                    "Owner {} reached the configured HDHomeRun without redirects or proxies",
                    config.owner_node_id
                )
            })
            .unwrap_or_else(|error| error.to_string()),
    });
    let snapshot = snapshot.ok();
    if let Some(snapshot) = snapshot.as_ref() {
        checks.push(LiveTvReadinessCheck {
            id: "lineup",
            ready: snapshot.freshness == SnapshotFreshness::Fresh && !snapshot.channels.is_empty(),
            message: if snapshot.channels.is_empty() {
                "The HDHomeRun lineup is empty; complete a channel scan first".to_owned()
            } else {
                format!(
                    "{} sanitized channels loaded ({:?})",
                    snapshot.channels.len(),
                    snapshot.freshness
                )
            },
        });
        checks.push(LiveTvReadinessCheck {
            id: "session_limit",
            ready: config.max_sessions <= snapshot.device.tuner_count.min(4),
            message: format!(
                "plurx may use {} of {} reported tuners",
                config.max_sessions, snapshot.device.tuner_count
            ),
        });
        checks.push(LiveTvReadinessCheck {
            id: "ffmpeg_graph",
            ready: snapshot.ffmpeg_graph_ready,
            message: snapshot.ffmpeg_graph_message.clone(),
        });
        checks.push(LiveTvReadinessCheck {
            id: "drm_boundary",
            ready: true,
            message: "Only channels not tagged DRM are available; protected TV is never opened"
                .to_owned(),
        });
    }
    let ready = protocol_ready
        && owner_ready
        && checks
            .iter()
            .all(|check| check.ready || check.id == "drm_boundary");
    LiveTvReadiness {
        ready,
        enabled: config.enabled,
        owner_node_id: config.owner_node_id.clone(),
        generation: config.generation,
        checks,
        snapshot,
    }
}

async fn owner_start(
    state: &AppState,
    config: &LiveTvConfig,
    request: &LiveTvStartRequest,
) -> Result<crate::live_tv::LiveTvProvisional, ApiError> {
    if config.owner_node_id == state.node_id {
        return state
            .live_tv
            .start_local(request.clone())
            .await
            .map_err(api_error);
    }
    let (node_id, base) = owner_peer(state, &config.owner_node_id).await?;
    let (path, body) = if request.playback.is_some() {
        (
            START_V2_PATH,
            serde_json::to_vec(&LiveTvStartRequestV2::from(request))
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        )
    } else {
        (
            START_PATH,
            serde_json::to_vec(request).map_err(|error| ApiError::Internal(error.to_string()))?,
        )
    };
    let transport = PeerTransport::new(state.membership.clone());
    let mut last_transport = PeerTransportError::Unreachable;
    let total_deadline = deadline_after(START_EXCHANGE_TOTAL);
    for attempt in 0..2 {
        let attempt_deadline = total_deadline.min(deadline_after(START_EXCHANGE_ATTEMPT));
        match transport
            .request(
                &node_id,
                &base,
                reqwest::Method::POST,
                path,
                body.clone(),
                attempt_deadline,
                MAX_START_RESPONSE_BYTES,
                PeerAuthMode::ExactRequestAndResponse,
            )
            .await
        {
            Ok(response) if response.status.is_success() => {
                let provisional =
                    serde_json::from_slice::<crate::live_tv::LiveTvProvisional>(&response.body)
                        .map_err(|_| {
                            ApiError::typed(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "owner_unavailable",
                                "The tuner owner returned an invalid start response",
                            )
                        })?;
                if capability_owner(&provisional.capability).ok().as_deref()
                    != Some(config.owner_node_id.as_str())
                    || provisional.config_generation != config.generation
                    || provisional.channel.id != request.channel_id
                {
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "owner_unavailable",
                        "The tuner owner returned a mismatched start response",
                    ));
                }
                return Ok(provisional);
            }
            Ok(response) => return Err(wire_api_error(response.status, &response.body)),
            Err(error) => {
                last_transport = error;
                if attempt == 1 {
                    break;
                }
            }
        }
    }
    Err(api_error(peer_error(last_transport)))
}

async fn owner_activate(
    state: &AppState,
    config: &LiveTvConfig,
    request: &LiveTvActivateRequest,
) -> Result<LiveTvActivated, ApiError> {
    if config.owner_node_id == state.node_id {
        return state
            .live_tv
            .activate_local(request, &state.node_id)
            .await
            .map_err(api_error);
    }
    let (node_id, base) = owner_peer(state, &config.owner_node_id).await?;
    let body =
        serde_json::to_vec(request).map_err(|error| ApiError::Internal(error.to_string()))?;
    let transport = PeerTransport::new(state.membership.clone());
    let mut last_transport = PeerTransportError::Unreachable;
    for attempt in 0..2 {
        match transport
            .request(
                &node_id,
                &base,
                reqwest::Method::POST,
                ACTIVATE_PATH,
                body.clone(),
                deadline_after(CONTROL_EXCHANGE_DEADLINE),
                MAX_START_RESPONSE_BYTES,
                PeerAuthMode::ExactRequestAndResponse,
            )
            .await
        {
            Ok(response) if response.status.is_success() => {
                let mut activated = serde_json::from_slice::<LiveTvActivated>(&response.body)
                    .map_err(|_| {
                        ApiError::typed(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "owner_unavailable",
                            "The tuner owner returned an invalid activation response",
                        )
                    })?;
                if activated.session_id != request.capability {
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "owner_unavailable",
                        "The tuner owner returned a mismatched activation response",
                    ));
                }
                activated.playlist_url =
                    format!("/api/v1/live-tv/sessions/{}/index.m3u8", request.capability);
                return Ok(activated);
            }
            Ok(response) => return Err(wire_api_error(response.status, &response.body)),
            Err(error) => {
                last_transport = error;
                if attempt == 1 {
                    break;
                }
            }
        }
    }
    Err(api_error(peer_error(last_transport)))
}

async fn owner_resource(
    state: &AppState,
    request: LiveTvResourceRequest,
) -> Result<Response<Body>, ApiError> {
    if !state.serving.is_ready() {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "This node is not ready to route live TV",
        ));
    }
    let owner = capability_owner(request.capability()).map_err(api_error)?;
    if owner == state.node_id {
        return state
            .live_tv
            .resource_local(request)
            .await
            .map_err(api_error);
    }
    let (node_id, base) = owner_peer(state, &owner).await?;
    let max_body_bytes = match &request {
        LiveTvResourceRequest::Playlist { .. } => crate::live_tv::MAX_PLAYLIST_BYTES,
        LiveTvResourceRequest::Segment { .. }
        | LiveTvResourceRequest::Init { .. }
        | LiveTvResourceRequest::Fragment { .. } => crate::live_tv::MAX_SEGMENT_BYTES,
        LiveTvResourceRequest::Status { .. } => 32 * 1024,
        LiveTvResourceRequest::Keepalive { .. } => 1024,
    };
    let body =
        serde_json::to_vec(&request).map_err(|error| ApiError::Internal(error.to_string()))?;
    if !matches!(
        request,
        LiveTvResourceRequest::Segment { .. }
            | LiveTvResourceRequest::Init { .. }
            | LiveTvResourceRequest::Fragment { .. }
    ) {
        let response = PeerTransport::new(state.membership.clone())
            .request(
                &node_id,
                &base,
                reqwest::Method::POST,
                RESOURCE_PATH,
                body,
                deadline_after(RESOURCE_EXCHANGE_DEADLINE),
                max_body_bytes as usize,
                PeerAuthMode::ExactRequestAndResponse,
            )
            .await
            .map_err(|error| api_error(peer_error(error)))?;
        if !response.status.is_success() {
            return Err(wire_api_error(response.status, &response.body));
        }
        let content_type = if matches!(request, LiveTvResourceRequest::Playlist { .. }) {
            "application/vnd.apple.mpegurl"
        } else {
            "application/json"
        };
        return Response::builder()
            .status(response.status)
            .header(axum::http::header::CONTENT_TYPE, content_type)
            .header(axum::http::header::CACHE_CONTROL, "no-store")
            .body(Body::from(response.body))
            .map_err(|error| ApiError::Internal(error.to_string()));
    }
    let response = PeerTransport::new(state.membership.clone())
        .request_stream(
            &node_id,
            &base,
            reqwest::Method::POST,
            RESOURCE_PATH,
            body,
            deadline_after(RESOURCE_EXCHANGE_DEADLINE),
            PeerAuthMode::ExactRequest,
        )
        .await
        .map_err(|error| api_error(peer_error(error)))?;
    if !response.status().is_success() {
        return Err(ApiError::typed(
            StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            if response.status() == reqwest::StatusCode::GONE {
                "capability_expired"
            } else {
                "owner_unavailable"
            },
            "The tuner owner could not serve this live resource",
        ));
    }
    crate::media_sessions::relay_response_with_limits_counted_bounded(
        response,
        Duration::from_secs(60),
        Duration::from_secs(15),
        state.live_tv.relay_counter(),
        max_body_bytes,
    )
    .map_err(|error| api_error(peer_error(error)))
}

async fn owner_stop(state: &AppState, owner: &str, capability: &str) -> Result<(), ApiError> {
    if owner == state.node_id {
        return state
            .live_tv
            .stop_local(capability)
            .await
            .map_err(api_error);
    }
    let (node_id, base) = owner_peer(state, owner).await?;
    let body = serde_json::to_vec(&LiveTvStopRequest {
        expected_owner_node_id: owner.to_owned(),
        capability: capability.to_owned(),
    })
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    let response = PeerTransport::new(state.membership.clone())
        .request(
            &node_id,
            &base,
            reqwest::Method::POST,
            STOP_PATH,
            body,
            deadline_after(CONTROL_EXCHANGE_DEADLINE),
            1_024,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(|error| api_error(peer_error(error)))?;
    if response.status.is_success() {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "The tuner owner could not stop this live session",
        ))
    }
}

pub(crate) async fn drain_owner(
    state: &AppState,
    owner: &str,
    drain_before_generation: i64,
) -> Result<(), LiveTvError> {
    if owner == state.node_id {
        return state
            .live_tv
            .drain_before(drain_before_generation)
            .await
            .map(|_| ());
    }
    let (node_id, base) = owner_peer(state, owner).await.map_err(|_| {
        LiveTvError::OwnerUnavailable("the prior tuner owner is unreachable".into())
    })?;
    let request_nonce = uuid::Uuid::new_v4().to_string();
    let body = serde_json::to_vec(&crate::live_tv::LiveTvDrainRequest {
        expected_owner_node_id: owner.to_owned(),
        target_node_id: state.node_id.clone(),
        request_nonce: request_nonce.clone(),
        drain_before_generation,
    })
    .map_err(|error| LiveTvError::InvalidResponse(error.to_string()))?;
    let response = PeerTransport::new(state.membership.clone())
        .request(
            &node_id,
            &base,
            reqwest::Method::POST,
            crate::live_tv::DRAIN_PATH,
            body,
            deadline_after(DRAIN_EXCHANGE_DEADLINE),
            4 * 1024,
            PeerAuthMode::ExactRequest,
        )
        .await
        .map_err(peer_error)?;
    if !response.status.is_success() {
        return Err(LiveTvError::OwnerUnavailable(
            "the prior tuner owner refused the drain request".into(),
        ));
    }
    let ack = serde_json::from_slice::<LiveTvDrainAck>(&response.body).map_err(|_| {
        LiveTvError::OwnerUnavailable(
            "the prior tuner owner returned an invalid drain proof".into(),
        )
    })?;
    if ack.owner_node_id != owner
        || ack.target_node_id != state.node_id
        || ack.request_nonce != request_nonce
        || ack.drained_before_generation != drain_before_generation
    {
        return Err(LiveTvError::OwnerUnavailable(
            "the prior tuner owner returned a mismatched drain proof".into(),
        ));
    }
    let payload = ack.signing_payload()?;
    let authorized = state
        .membership
        .authorize_internal_peer_response(
            owner,
            &state.node_id,
            &request_nonce,
            crate::live_tv::DRAIN_PATH,
            &payload,
            &ack.signature,
        )
        .await
        .unwrap_or(false);
    if !authorized {
        return Err(LiveTvError::OwnerUnavailable(
            "the prior tuner owner's drain proof could not be authenticated".into(),
        ));
    }
    Ok(())
}

async fn owner_peer(state: &AppState, expected: &str) -> Result<(String, String), ApiError> {
    if !state.membership.is_replicated() {
        return Err(api_error(LiveTvError::OwnerUnavailable(
            "the selected owner is not this single-node server".into(),
        )));
    }
    state
        .membership
        .activity_peers()
        .await
        .map_err(|error| api_error(LiveTvError::OwnerUnavailable(error.to_string())))?
        .into_iter()
        .find(|peer| peer.node_id == expected && peer.reachable)
        .and_then(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .ok_or_else(|| {
            api_error(LiveTvError::OwnerUnavailable(
                "the selected tuner owner is not a reachable committed voter".into(),
            ))
        })
}

fn wire_api_error(status: reqwest::StatusCode, body: &[u8]) -> ApiError {
    let wire = serde_json::from_slice::<WireError>(body).ok();
    let code = wire
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("owner_unavailable")
        .to_owned();
    let message = wire
        .map(|error| sanitize_public_error(&error.message))
        .unwrap_or_else(|| "The tuner owner could not complete the request".into());
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    let stable = match code.as_str() {
        "live_tv_disabled" => "live_tv_disabled",
        "tuner_capacity" => "tuner_capacity",
        "tuner_unavailable" => "tuner_unavailable",
        "channel_not_found" => "channel_not_found",
        "drm_unsupported" => "drm_unsupported",
        "codec_unsupported" => "codec_unsupported",
        "startup_timeout" => "startup_timeout",
        "source_format_changed" => "source_format_changed",
        "stream_failed" => "stream_failed",
        "settings_conflict" => "settings_conflict",
        "capability_expired" => "capability_expired",
        _ => "owner_unavailable",
    };
    ApiError::typed(status, stable, message)
}

async fn owner_snapshot(
    state: &AppState,
    config: &LiveTvConfig,
    force: bool,
    probe_graph: bool,
) -> Result<LiveTvSnapshot, LiveTvError> {
    if !state.serving.is_ready() {
        return Err(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
        ));
    }
    if config.owner_node_id == state.node_id {
        if state.membership.is_replicated()
            && !state
                .membership
                .local_node_is_committed_voter()
                .await
                .unwrap_or(false)
        {
            return Err(LiveTvError::OwnerUnavailable(
                "the selected owner is not a committed voter".to_owned(),
            ));
        }
        return state
            .live_tv
            .local_snapshot(config, force, probe_graph)
            .await;
    }
    if !state.membership.is_replicated() {
        return Err(LiveTvError::OwnerUnavailable(
            "the selected owner is not this single-node server".to_owned(),
        ));
    }
    let peer = state
        .membership
        .activity_peers()
        .await
        .map_err(|error| LiveTvError::OwnerUnavailable(error.to_string()))?
        .into_iter()
        .find(|peer| peer.node_id == config.owner_node_id)
        .filter(|peer| peer.reachable)
        .and_then(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .ok_or_else(|| {
            LiveTvError::OwnerUnavailable(
                "the selected tuner owner is not a reachable committed voter".to_owned(),
            )
        })?;
    let body = serde_json::to_vec(&SnapshotRequest {
        generation: config.generation,
        force,
        probe_graph,
    })
    .map_err(|error| LiveTvError::InvalidResponse(error.to_string()))?;
    let response = PeerTransport::new(state.membership.clone())
        .request(
            &peer.0,
            &peer.1,
            reqwest::Method::POST,
            SNAPSHOT_PATH,
            body,
            deadline_after(SNAPSHOT_DEADLINE),
            MAX_SNAPSHOT_BYTES,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(peer_error)?;
    if !response.status.is_success() {
        return Err(LiveTvError::OwnerUnavailable(format!(
            "tuner owner returned HTTP {}",
            response.status.as_u16()
        )));
    }
    let snapshot = serde_json::from_slice::<LiveTvSnapshot>(&response.body).map_err(|_| {
        LiveTvError::InvalidResponse("tuner owner returned an invalid snapshot".to_owned())
    })?;
    if snapshot.generation != config.generation {
        return Err(LiveTvError::OwnerUnavailable(
            "tuner owner returned a stale configuration generation".to_owned(),
        ));
    }
    Ok(snapshot)
}

fn peer_error(error: PeerTransportError) -> LiveTvError {
    let message = match error {
        PeerTransportError::Unreachable => "the tuner owner is unreachable",
        PeerTransportError::TimedOut => "the tuner owner timed out",
        PeerTransportError::InvalidResponse => "the tuner owner returned an invalid response",
    };
    LiveTvError::OwnerUnavailable(message.to_owned())
}

pub(crate) fn api_error(error: LiveTvError) -> ApiError {
    let code = error.code();
    match error {
        LiveTvError::InvalidConfig(message) | LiveTvError::InvalidResponse(message) => {
            ApiError::BadRequest(message)
        }
        LiveTvError::DeviceUnavailable(message) | LiveTvError::OwnerUnavailable(message) => {
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                code,
                sanitize_public_error(&message),
            )
        }
        LiveTvError::Disabled(message) => {
            ApiError::typed(StatusCode::SERVICE_UNAVAILABLE, code, message)
        }
        LiveTvError::Capacity(message) | LiveTvError::TunerUnavailable(message) => {
            ApiError::typed(StatusCode::SERVICE_UNAVAILABLE, code, message)
        }
        LiveTvError::ChannelNotFound(message) => {
            ApiError::typed(StatusCode::NOT_FOUND, code, message)
        }
        LiveTvError::DrmUnsupported(message) | LiveTvError::CodecUnsupported(message) => {
            ApiError::typed(StatusCode::UNSUPPORTED_MEDIA_TYPE, code, message)
        }
        LiveTvError::StartupTimeout(message) => {
            ApiError::typed(StatusCode::REQUEST_TIMEOUT, code, message)
        }
        LiveTvError::StreamFailed(message) => ApiError::typed(
            StatusCode::BAD_GATEWAY,
            code,
            sanitize_public_error(&message),
        ),
        LiveTvError::Conflict(message) | LiveTvError::SourceFormatChanged(message) => {
            ApiError::typed(StatusCode::CONFLICT, code, message)
        }
        LiveTvError::CapabilityExpired(message) => ApiError::typed(StatusCode::GONE, code, message),
    }
}

fn sanitize_public_error(message: &str) -> String {
    if message.contains("http://") || message.contains("https://") {
        "The HDHomeRun owner could not complete the request; see owner logs for details".to_owned()
    } else {
        message
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn peer_transport_errors_have_stable_operator_messages() {
        assert_eq!(
            peer_error(PeerTransportError::TimedOut).to_string(),
            "the tuner owner timed out"
        );
    }

    #[test]
    fn public_device_errors_never_disclose_the_private_url() {
        let error = api_error(LiveTvError::DeviceUnavailable(
            "request failed for http://192.168.4.20/discover.json".to_owned(),
        ));
        let ApiError::Typed {
            message: rendered, ..
        } = error
        else {
            panic!("expected typed owner error");
        };
        assert!(!rendered.contains("192.168.4.20"), "{rendered}");
        assert!(!rendered.contains("http://"), "{rendered}");
    }

    #[test]
    fn owner_format_change_keeps_its_stable_recovery_code() {
        let error = wire_api_error(
            reqwest::StatusCode::CONFLICT,
            br#"{"code":"source_format_changed","message":"select a fresh route"}"#,
        );
        let ApiError::Typed { code, .. } = error else {
            panic!("expected typed owner error");
        };
        assert_eq!(code, "source_format_changed");
    }

    #[tokio::test]
    async fn public_error_mapping_redacts_connection_and_body_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve refused address");
        let refused = listener.local_addr().expect("refused address");
        drop(listener);
        let refused_url =
            reqwest::Url::parse(&format!("http://{refused}/discover.json")).expect("refused URL");
        let connection_error = crate::live_tv::fetch_json::<serde_json::Value>(
            &reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("client"),
            refused_url,
        )
        .await
        .expect_err("connection must fail");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("body listener");
        let address = listener.local_addr().expect("body address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("body connection");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n20\r\n{}",
                )
                .await
                .expect("truncated body");
        });
        let body_url =
            reqwest::Url::parse(&format!("http://{address}/lineup.json")).expect("body URL");
        let body_error = crate::live_tv::fetch_json::<serde_json::Value>(
            &reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("client"),
            body_url,
        )
        .await
        .expect_err("truncated body must fail");
        server.await.expect("body server");

        for error in [connection_error, body_error] {
            let ApiError::Typed { message, .. } = api_error(error) else {
                panic!("expected typed owner failure");
            };
            assert!(!message.contains("127.0.0.1"), "{message}");
            assert!(!message.contains("http://"), "{message}");
        }
    }
}
