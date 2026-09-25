//! Public HDHomeRun readiness and sanitized channel lineup.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use super::peer_transport::{
    deadline_after, PeerAuthMode, PeerResponse, PeerTransport, PeerTransportError,
};
use crate::live_tv::{
    capability_owner, guide, GuideWindow, LiveTvActivateRequest, LiveTvActivated, LiveTvConfig,
    LiveTvError, LiveTvGuide, LiveTvResourceRequest, LiveTvSessionStatus, LiveTvSnapshot,
    LiveTvStartRequest, LiveTvStartRequestV2, LiveTvStopRequest, SnapshotFreshness,
    SnapshotRequest, ACTIVATE_PATH, GUIDE_PATH, MAX_SNAPSHOT_BYTES, RESOURCE_PATH, RESUME_PATH,
    RETIRE_PATH, SNAPSHOT_PATH, START_PATH, START_STATE_PATH, START_V2_PATH, STOP_PATH,
};
use crate::live_tv_delivery::LivePlaybackRequest;
use crate::state::AppState;

const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(25);
const START_EXCHANGE_ATTEMPT: Duration = Duration::from_secs(20);
const CONTROL_EXCHANGE_DEADLINE: Duration = Duration::from_secs(5);
/// Everything a public start may spend, snapshot read through activation. The
/// clients give up at 45 s; a start that outlives that leaves a session with
/// nobody holding it, which is the whole failure this effort exists to end.
///
/// Deliberately 35 rather than 40: a start that fails after a provisional was
/// issued still has to stop it, and that stop is awaited on the response path
/// with its own budget — `CONTROL_EXCHANGE_DEADLINE` to a peer, or
/// `SESSION_DRAIN_TIMEOUT` locally. Thirty-five plus five is inside the
/// clients' forty-five; forty plus five is exactly on it.
///
/// The owner's own lifecycle constants are untouched — this bounds the
/// ingress's exchanges, not how long a tuner is given to feed.
const PUBLIC_START_DEADLINE: Duration = Duration::from_secs(35);
const RESOURCE_EXCHANGE_DEADLINE: Duration = Duration::from_secs(12);
const MAX_START_RESPONSE_BYTES: usize = 32 * 1024;
const GUIDE_EXCHANGE_DEADLINE: Duration = Duration::from_secs(25);
/// The widest window one request may ask for. It matches the configuration
/// ceiling rather than sitting under it: a client that can be configured to
/// keep a fortnight must be able to read a fortnight.
const MAX_GUIDE_REQUEST_HOURS: u16 = 336;

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
    /// What the whole path — this ingress *and* the owner it talks to —
    /// accepts on a start. The clients read this and nothing else before
    /// deciding whether to send a request id or use the recovery routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) protocols: Vec<u8>,
    pub(crate) channels: Vec<crate::live_tv::LiveTvChannel>,
}

#[derive(Deserialize)]
pub(crate) struct GuideQuery {
    from: Option<i64>,
    hours: Option<u16>,
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
        return Err(api_error(LiveTvError::Disabled(
            "Live TV is disabled; an administrator can enable it in Settings → Developer".into(),
        )));
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
    let local = state.live_tv.local_guide(config, window.clone()).await;
    if local.freshness == crate::live_tv::guide::GuideFreshness::Fresh {
        return local;
    }
    if let Some(cached) = state.live_tv.relayed_guide(config.generation).await {
        return cached.clipped(&window);
    }
    if state.membership.is_replicated() {
        if let Ok(peers) = state.membership.activity_peers().await {
            for peer in peers
                .into_iter()
                .filter(|p| p.reachable && p.node_id != state.node_id)
                .take(3)
            {
                let mut candidate = config.clone();
                candidate.owner_node_id = peer.node_id;
                if let Ok(guide) = relay_guide(state, &candidate).await {
                    if guide.freshness != crate::live_tv::guide::GuideFreshness::Unavailable {
                        state
                            .live_tv
                            .remember_relayed_guide(config.generation, guide.clone())
                            .await;
                        return guide.clipped(&window);
                    }
                }
            }
        }
    }
    local
}

/// Keep a sanitized durable guide copy on every serving node, including nodes
/// with no tuner network path. Source fetches have a separate cluster lease.
pub(crate) async fn guide_replica_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        if let Ok(config) = state.live_tv.config().await {
            if config.guide_fetches() && state.serving.is_ready() {
                let _ =
                    owner_guide(&state, &config, guide::refresh_window(config.guide_hours)).await;
            }
        }
        tokio::select! { _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_secs(60)) => {} }
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
        .ok_or_else(|| "the session server is not a reachable committed voter".to_owned())?;
    let body = serde_json::to_vec(&SnapshotRequest {
        generation: config.generation,
        force: false,
        probe_graph: false,
    })
    .map_err(|error| error.to_string())?;
    let response = state
        .live_tv_peers
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
        return Err("the session server does not serve a programme guide yet".to_owned());
    }
    if !response.status.is_success() {
        return Err(format!(
            "the session server returned HTTP {}",
            response.status.as_u16()
        ));
    }
    serde_json::from_slice::<LiveTvGuide>(&response.body)
        .map_err(|_| "the session server returned an invalid guide".to_owned())
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
            "a guide refresh runs on the session server; ask that node",
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
    // Four rows about the copy itself: how old it is, when the owner comes
    // back, whether a deploy would keep it, and what the last refresh said.
    // Advisory like every other row — none of them refuse a save.
    checks.push(LiveTvReadinessCheck {
        id: "guide_age",
        ready: guide.freshness == crate::live_tv::GuideFreshness::Fresh,
        message: match guide.fetched_at {
            Some(fetched_at) => format!(
                "The cached guide is {} seconds old (fetched at unix {fetched_at}).",
                guide.age_seconds
            ),
            None => "Nothing has been fetched yet on this node.".to_owned(),
        },
    });
    checks.push(LiveTvReadinessCheck {
        id: "guide_next_refresh",
        ready: guide.next_refresh_at.is_some(),
        message: match guide.next_refresh_at {
            Some(next) => format!(
                "The owner's refresh loop next runs in {} seconds (unix {next}); clients poll on that.",
                next.saturating_sub(now).max(0)
            ),
            None => "The owner's refresh loop has not completed a tick yet.".to_owned(),
        },
    });
    checks.push(match state.live_tv.guide_store_status() {
        Some((path, bytes, generation)) => LiveTvReadinessCheck {
            id: "guide_persisted",
            ready: true,
            message: format!(
                "A copy is on disk at {} ({bytes} bytes, settings generation {}), so a restart serves the previous guide while the first refresh runs.",
                path.display(),
                generation
                    .map(|generation| generation.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
        },
        None => LiveTvReadinessCheck {
            id: "guide_persisted",
            ready: false,
            message: if owner_is_local {
                "No copy is on disk yet. The first successful refresh writes one, and a restart before then starts the grid empty."
                    .to_owned()
            } else {
                "Only the session server keeps a copy on disk; this node relays the owner's answer."
                    .to_owned()
            },
        },
    });
    checks.push(LiveTvReadinessCheck {
        id: "guide_last_error",
        ready: guide.refresh_error.is_none(),
        message: match guide.refresh_error.as_deref() {
            Some(error) => format!("The last refresh under this configuration failed: {error}"),
            None => "No refresh under this configuration has reported an error.".to_owned(),
        },
    });
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
    pub(crate) guide_hours: u16,
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
    /// The client's durable identity for this start — 32 lower-case hex
    /// characters, the same id it persists as its hint. Replaying it joins the
    /// same owner session instead of taking a second tuner. Absent from an
    /// older client; the ingress then mints one.
    #[serde(default)]
    request_id: Option<String>,
}

/// 128 bits of hex. Not a UUID spelling, because the clients generate this
/// without `crypto.randomUUID` (unavailable on the LAN HTTP origin the web UI
/// runs on) and a hyphenless hex string is what all three can produce.
///
/// The owner's `validate_start_request` applies this same function to the id
/// it receives, so an id this surface accepts can never be one the owner
/// refuses. They were two functions once and did not agree.
use crate::live_tv::valid_request_id as valid_public_request_id;

fn parse_public_request_id(value: &str) -> Result<&str, ApiError> {
    valid_public_request_id(value)
        .then_some(value)
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "a live-TV request id is 32 lower-case hex characters",
            )
        })
}

/// What *this binary's public surface* accepts: body fields and recovery
/// routes. An owner advertising 3 says nothing about the ingress a client is
/// talking to, so what the client reads is the intersection.
const INGRESS_START_PROTOCOLS: &[u8] = &[1, 2, 3, 4];

fn negotiated_protocols(owner: &[u8]) -> Vec<u8> {
    INGRESS_START_PROTOCOLS
        .iter()
        .copied()
        .filter(|protocol| owner.contains(protocol))
        .collect()
}

/// Persist the server-issued namespace before returning it. An unknown v4 ID
/// never falls back to legacy admission, even after terminal-history GC.
pub(crate) async fn issue_start(
    Path(channel): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Option<Json<PublicLiveTvStart>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let deadline = deadline_after(PUBLIC_START_DEADLINE);
    let generation = state.serving.authority().admit().ok_or_else(|| {
        api_error(LiveTvError::OwnerUnavailable(
            crate::serving_fence::SERVING_FENCED_MESSAGE.into(),
        ))
    })?;
    let config = live_tv_enabled_config(&state).await?;
    let request_id = format!("v4_{}", uuid::Uuid::new_v4().simple());
    let config =
        select_start_worker(&state, config, user.id, &request_id, &channel, deadline).await?;
    let snapshot = owner_snapshot_within(&state, &config, false, false, deadline)
        .await
        .map_err(api_error)?;
    let playback = body.and_then(|Json(b)| b.playback);
    if let Some(p) = &playback {
        p.validate().map_err(ApiError::BadRequest)?;
    }
    let request = LiveTvStartRequest {
        expected_owner_node_id: config.owner_node_id,
        source_node_id: state.node_id,
        user_id: user.id,
        user_name: user.username,
        request_id,
        channel_id: channel,
        config_generation: config.generation,
        source_serving_generation: generation,
        playback,
    };
    let ticket = state
        .live_tv
        .resource_issue(&request, &snapshot.device.device_id)
        .await
        .map_err(api_error)?;
    Ok(Json(serde_json::json!({"request_id":ticket.request_id,
        "admission_expires_at_ms":ticket.admission_until_ms,"config_generation":ticket.generation})))
}

pub(crate) async fn start_session(
    Path(channel): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Option<Json<PublicLiveTvStart>>,
) -> Result<Json<LiveTvActivated>, ApiError> {
    // One budget for the whole public start. The stages below each take
    // `min(their own constant, what is left)`, so no combination of a slow
    // snapshot, two start attempts and two activation attempts can outlive the
    // clients' 45 s POST timeout and leave a session nobody is holding.
    let deadline = deadline_after(PUBLIC_START_DEADLINE);
    let ingress_generation = state.serving.authority().admit().ok_or_else(|| {
        ApiError::typed_detail(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            crate::serving_fence::SERVING_FENCED_MESSAGE,
            serde_json::json!({"retry": "now", "owner_decided": false}),
        )
    })?;
    let mut config = state.live_tv.config().await.map_err(api_error)?;
    if !config.enabled {
        return Err(api_error(LiveTvError::Disabled(
            "Live TV is disabled; an administrator can enable it in Settings → Developer".into(),
        )));
    }
    let Json(body) = body.unwrap_or(Json(PublicLiveTvStart {
        playback: None,
        request_id: None,
    }));
    let playback = body.playback;
    if let Some(request) = &playback {
        request.validate().map_err(|message| {
            ApiError::typed(StatusCode::BAD_REQUEST, "invalid_request", message)
        })?;
    }
    let mut client_request_id = match body.request_id.as_deref() {
        Some(value) => Some(parse_public_request_id(value)?.to_owned()),
        None => None,
    };
    let request_id = client_request_id
        .take()
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    config = select_start_worker(&state, config, user.id, &request_id, &channel, deadline).await?;
    let mut request = LiveTvStartRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        source_node_id: state.node_id.clone(),
        user_id: user.id,
        user_name: user.username,
        request_id: request_id.clone(),
        channel_id: channel,
        config_generation: config.generation,
        source_serving_generation: ingress_generation,
        playback,
    };
    // A failure here is deliberately *not* followed by a retire from this node.
    // The start may have been admitted on the owner, and the client's answer
    // says so: anything the owner did not decide leaves the client holding its
    // hint, and its replay carries the same id and joins that very session.
    // Fencing the id here turned that replay into a 409 — it removed the
    // recovery it was meant to provide. The client retires the hint itself on
    // its next press, and the owner reaps an unheld session at 45 s.
    let provisional = match owner_start_within(&state, &config, &request, deadline).await {
        Ok(response) => response,
        Err(error) => {
            // Concurrent admissions can select different candidates. The CAS
            // winner is the only assignment; dispatch there only when the
            // durable answer proves that this candidate never owned the start.
            let assigned = assigned_start_worker(&state, user.id, &request_id).await?;
            if let Some(worker) = assigned.filter(|worker| *worker != config.owner_node_id) {
                config.owner_node_id = worker;
                request.expected_owner_node_id = config.owner_node_id.clone();
                owner_start_within(&state, &config, &request, deadline).await?
            } else {
                return Err(error);
            }
        }
    };
    let activation = LiveTvActivateRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        capability: provisional.capability.clone(),
        activation_token: provisional.activation_token.clone(),
        config_generation: config.generation,
        source_serving_generation: ingress_generation,
    };
    let activated = match owner_activate_within(&state, &config, &activation, deadline).await {
        Ok(activated) => activated,
        Err(error) => {
            // The capability is stopped, which tombstones the request on the
            // owner; a replay of the same id gets that tombstone, which is an
            // owner-decided answer. Nothing else to fence.
            let _ = owner_stop(&state, &config.owner_node_id, &provisional.capability).await;
            return Err(error);
        }
    };
    let current = state.live_tv.config().await.map_err(api_error)?;
    if current.generation != config.generation
        || !current.enabled
        || !state.serving.authority().is_current(ingress_generation)
    {
        let _ = owner_stop(&state, &config.owner_node_id, &provisional.capability).await;
        return Err(ApiError::typed_detail(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "Live TV changed while the session was starting; press Watch again",
            serde_json::json!({"retry": "now", "owner_decided": false}),
        ));
    }
    Ok(Json(activated))
}

/// Stop whatever this request id produced and fence it. Idempotent: a client
/// that presses again after a no-answer sends this first.
pub(crate) async fn retire_start(
    Path(request_id): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let request_id = parse_public_request_id(&request_id)?.to_owned();
    let mut config = state.live_tv.config().await.map_err(api_error)?;
    let assigned = assigned_start_worker(&state, user.id, &request_id).await?;
    state
        .live_tv
        .resource_retire(user.id, &request_id)
        .await
        .map_err(api_error)?;
    let outcome = if let Some(worker) = assigned {
        config.owner_node_id = worker;
        owner_retire(&state, &config, user.id, &request_id)
            .await
            .unwrap_or(crate::live_tv::LiveTvRetireOutcome::Retired)
    } else {
        crate::live_tv::LiveTvRetireOutcome::Retired
    };
    Ok(Json(serde_json::json!({ "outcome": outcome })))
}

/// Rejoin the session this viewer's request id still owns. This is what makes
/// reopening the app after an unclean end show the picture instead of the list.
pub(crate) async fn resume_start(
    Path(request_id): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<crate::live_tv::LiveTvResumeAnswer>, ApiError> {
    let request_id = parse_public_request_id(&request_id)?.to_owned();
    let mut config = live_tv_enabled_config(&state).await?;
    if let Some(worker) = assigned_start_worker(&state, user.id, &request_id).await? {
        config.owner_node_id = worker;
    }
    Ok(Json(
        owner_resume(&state, &config, user.id, &request_id).await?,
    ))
}

/// A status read, never a capability. `unknown` is the honest answer for an id
/// this owner has never seen — which, after a restart, is every id.
pub(crate) async fn start_state(
    Path(request_id): Path<String>,
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<crate::live_tv::LiveTvStartState>, ApiError> {
    let request_id = parse_public_request_id(&request_id)?.to_owned();
    let ledger = state
        .live_tv
        .resource_snapshot(user.id, &request_id)
        .await
        .map_err(api_error)?;
    if let Some(start) = ledger.records.iter().find_map(|r| match r {
        plurx_core::live_tv_resource::Record::Start(s)
            if s.user_id == user.id && s.request_id == request_id =>
        {
            Some(s)
        }
        _ => None,
    }) {
        let lease_ended = start.ingest_id.as_ref().is_some_and(|id| !ledger.records.iter().any(|r|
            matches!(r, plurx_core::live_tv_resource::Record::Ingest(i) if i.id == *id && i.expires_at_ms > crate::live_tv::resource_now_ms())));
        if start.phase.terminal() || lease_ended {
            return Ok(Json(crate::live_tv::LiveTvStartState {
                state: "ended".into(),
                code: Some("capability_expired".into()),
            }));
        }
    }
    let mut config = live_tv_enabled_config(&state).await?;
    if let Some(worker) = assigned_start_worker(&state, user.id, &request_id).await? {
        config.owner_node_id = worker;
    }
    if config.owner_node_id == state.node_id {
        return Ok(Json(state.live_tv.start_state_local(user.id, &request_id)));
    }
    // Its own owner path, not a resume: a resume selects one session, cancels
    // the others, touches what it keeps and retires an id it has never seen.
    // Folding a status read into it meant looking at a start fenced it.
    let body = serde_json::to_vec(&crate::live_tv::LiveTvStartStateRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        user_id: user.id,
        request_id: request_id.clone(),
    })
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(Json(
        owner_recovery_exchange(
            &state,
            &config,
            START_STATE_PATH,
            body,
            "report start state yet",
        )
        .await?,
    ))
}

async fn assigned_start_worker(
    state: &AppState,
    user: i64,
    id: &str,
) -> Result<Option<String>, ApiError> {
    let snapshot = state
        .live_tv
        .resource_snapshot(user, id)
        .await
        .map_err(api_error)?;
    Ok(snapshot.records.into_iter().find_map(|r| match r {
        plurx_core::live_tv_resource::Record::Start(start)
            if start.user_id == user && start.request_id == id =>
        {
            start.worker.map(|w| w.node_id)
        }
        _ => None,
    }))
}

async fn select_start_worker(
    state: &AppState,
    mut config: LiveTvConfig,
    user: i64,
    id: &str,
    channel: &str,
    deadline: tokio::time::Instant,
) -> Result<LiveTvConfig, ApiError> {
    if let Some(worker) = assigned_start_worker(state, user, id).await? {
        config.owner_node_id = worker;
        return Ok(config);
    }
    let snapshot = state
        .live_tv
        .resource_snapshot(user, id)
        .await
        .map_err(api_error)?;
    if let Some(worker) = snapshot.records.iter().find_map(|r| match r {
        plurx_core::live_tv_resource::Record::Ingest(i)
            if i.channel_id == channel
                && i.generation == config.generation
                && i.expires_at_ms > crate::live_tv::resource_now_ms() =>
        {
            Some(&i.worker.node_id)
        }
        _ => None,
    }) {
        config.owner_node_id = worker.clone();
        return Ok(config);
    }
    let mut candidates = vec![state.node_id.clone()];
    if state.membership.is_replicated() {
        let mut peers = state
            .membership
            .activity_peers()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .into_iter()
            .filter(|p| p.reachable && p.http_base.is_some() && p.node_id != state.node_id)
            .map(|p| p.node_id)
            .collect::<Vec<_>>();
        peers.sort();
        candidates.extend(peers);
    }
    let mut last = LiveTvError::OwnerUnavailable("no reachable channel worker".into());
    for candidate in candidates.into_iter().take(3) {
        config.owner_node_id = candidate;
        match owner_snapshot_within(
            state,
            &config,
            false,
            false,
            deadline.min(deadline_after(std::time::Duration::from_secs(5))),
        )
        .await
        {
            Ok(snapshot) if snapshot.start_protocols.contains(&4) => return Ok(config),
            Ok(_) => {
                last = LiveTvError::OwnerUnavailable(
                    "the candidate needs the cluster resource protocol".into(),
                )
            }
            Err(error) => last = error,
        }
    }
    Err(api_error(last))
}

async fn live_tv_enabled_config(state: &AppState) -> Result<LiveTvConfig, ApiError> {
    let config = state.live_tv.config().await.map_err(api_error)?;
    if !config.enabled {
        return Err(api_error(LiveTvError::Disabled(
            "Live TV is disabled; an administrator can enable it in Settings → Developer".into(),
        )));
    }
    Ok(config)
}

/// HLS master for one activated viewer. The signed owner status already
/// contains the graph-specific proof reason, so the relay changes no internal
/// resource wire shape. Missing or malformed proof yields an honest NONE.
pub(crate) async fn master(
    Path(capability): Path<String>,
    State(state): State<AppState>,
) -> Result<Response<Body>, ApiError> {
    let response = owner_resource(&state, LiveTvResourceRequest::Status { capability }).await?;
    let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let status: LiveTvSessionStatus =
        serde_json::from_slice(&bytes).map_err(|error| ApiError::Internal(error.to_string()))?;
    let delivery = status.delivery;
    let services = delivery.as_ref().and_then(|plan| {
        plan.reasons
            .iter()
            .find(|reason| reason.code == "captions_advertised")
            .map(|reason| reason.explanation.as_str())
    });
    let mut body = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    let advertised = services == Some("CC1,SERVICE1");
    if advertised {
        body.push_str("#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"English CC1\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,INSTREAM-ID=\"CC1\"\n");
        body.push_str("#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"English 708\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,INSTREAM-ID=\"SERVICE1\"\n");
    }
    let bandwidth = delivery
        .and_then(|plan| plan.max_bitrate_bps)
        .unwrap_or(8_000_000)
        .saturating_add(192_000);
    body.push_str(&format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},CLOSED-CAPTIONS={}\nindex.m3u8\n",
        if advertised { "\"cc\"" } else { "NONE" },
    ));
    Ok(Response::builder()
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/vnd.apple.mpegurl",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("static master response headers"))
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
        return Err(api_error(LiveTvError::Disabled(
            "Live TV is disabled; an administrator can enable it in Settings → Developer".into(),
        )));
    }
    let snapshot = owner_snapshot(&state, &config, false, false)
        .await
        .map_err(api_error)?;
    Ok(Json(LiveTvChannelsResponse {
        freshness: snapshot.freshness,
        age_seconds: snapshot.age_seconds,
        last_success_at: snapshot.last_success_at,
        refresh_error: snapshot.refresh_error,
        protocols: negotiated_protocols(&snapshot.start_protocols),
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
            .map(|()| "The saved address, channel limit, and output height are valid".into())
            .unwrap_or_else(|error| error.to_string()),
    });

    // Start recovery — what a viewer needs for "reopen the app and the picture
    // is back" to work, and whether it is true right now. Advisory, like every
    // row here: nothing in this lane is gated in code, and this section is how
    // an operator finds out what is missing instead of a feature quietly
    // refusing.
    let negotiated = negotiated_protocols(
        &owner_snapshot(state, config, false, false)
            .await
            .map(|snapshot| snapshot.start_protocols)
            .unwrap_or_default(),
    );
    let recovery_ready = negotiated.contains(&3);
    checks.push(LiveTvReadinessCheck {
        id: "start_recovery",
        ready: recovery_ready,
        message: if recovery_ready {
            "This node and the session server both accept client start ids, so a client that is killed mid-stream gets its picture back on reopen instead of the channel list."
                .to_owned()
        } else {
            format!(
                "Start recovery needs protocol 3 on this node and on the session server; the negotiated set is {negotiated:?}. Until both sides have it, clients start normally and simply do not resume — restart or upgrade the older node. Advisory: nothing is blocked."
            )
        },
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

    checks.push(LiveTvReadinessCheck {
        id: "cluster_resource",
        ready: negotiated.contains(&4),
        message: if negotiated.contains(&4) {
            "A reachable worker supports durable channel assignment and server-issued start intents"
                .into()
        } else {
            "No reachable worker has reported the cluster resource protocol; upgrade the servers"
                .into()
        },
    });
    let mut nodes = vec![state.node_id.clone()];
    if let Ok(peers) = state.membership.activity_peers().await {
        nodes.extend(peers.into_iter().map(|peer| peer.node_id));
    }
    nodes.sort();
    nodes.dedup();
    let observations = futures_util::future::join_all(nodes.into_iter().map(|node| async move {
        let mut candidate = config.clone();
        candidate.owner_node_id = node.clone();
        let deadline = deadline_after(Duration::from_secs(5));
        let result = tokio::time::timeout_at(
            deadline,
            owner_snapshot_within(state, &candidate, force, true, deadline),
        )
        .await;
        match result {
            Ok(Ok(snapshot)) => (
                snapshot.start_protocols.contains(&4) && snapshot.ffmpeg_graph_ready,
                format!(
                    "{node}: device {}, {} tuners, protocol 4 {}, encoder {}",
                    snapshot.device.device_id,
                    snapshot.device.tuner_count,
                    snapshot.start_protocols.contains(&4),
                    snapshot.ffmpeg_graph_ready
                ),
            ),
            Ok(Err(error)) => (false, format!("{node}: {error}")),
            Err(_) => (false, format!("{node}: observation timed out")),
        }
    }))
    .await;
    checks.push(LiveTvReadinessCheck {
        id: "worker_observations",
        ready: observations.iter().any(|(ready, _)| *ready),
        message: observations
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("; "),
    });
    checks.push(LiveTvReadinessCheck { id: "clock_sync", ready: false,
        message: "Clock synchronization and shared DVR mount identity require operator verification; matching path strings are not proof".into() });
    if static_result.is_err() || !serving_ready {
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
                    "A cluster worker reached the configured HDHomeRun without redirects or proxies (ingress {})",
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
    let ready = owner_ready
        && checks
            .iter()
            // `start_recovery` is advisory in the strong sense: a fleet
            // without it plays perfectly well and simply does not resume, so
            // it must never turn the overall verdict red and pressure anyone
            // into treating Live TV as broken.
            .all(|check| {
                check.ready
                    || matches!(
                        check.id,
                        "drm_boundary" | "start_recovery" | "clock_sync" | "cluster_protocol"
                    )
            });
    LiveTvReadiness {
        ready,
        enabled: config.enabled,
        owner_node_id: config.owner_node_id.clone(),
        generation: config.generation,
        checks,
        snapshot,
    }
}

/// A stage deadline: its own constant, or whatever is left of the public
/// start's budget, whichever is sooner. Never past the budget.
fn stage_deadline(own: Duration, budget: tokio::time::Instant) -> tokio::time::Instant {
    budget.min(deadline_after(own))
}

fn budget_exhausted(budget: tokio::time::Instant) -> bool {
    budget <= tokio::time::Instant::now()
}

async fn owner_start_within(
    state: &AppState,
    config: &LiveTvConfig,
    request: &LiveTvStartRequest,
    budget: tokio::time::Instant,
) -> Result<crate::live_tv::LiveTvProvisional, ApiError> {
    if config.owner_node_id == state.node_id {
        return state
            .live_tv
            .start_local(request.clone())
            .await
            // Only the owner knows what is holding its tuners, so only the
            // owner can name them. A relayed refusal keeps the code and loses
            // the detail, which is the honest thing for an ingress to say.
            .map_err(|error| {
                capacity_error(
                    error,
                    state.live_tv.transport_holders(),
                    state.live_tv.watchable_channels(),
                )
            });
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
    let transport = &state.live_tv_peers;
    let mut last_transport = PeerTransportError::TimedOut;
    for attempt in 0..2 {
        if budget_exhausted(budget) {
            break;
        }
        let attempt_deadline = stage_deadline(START_EXCHANGE_ATTEMPT, budget);
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
                                "The session server returned an invalid start response",
                            )
                        })?;
                if capability_owner(&provisional.capability).ok().as_deref()
                    != Some(config.owner_node_id.as_str())
                    || provisional.config_generation != config.generation
                    || provisional.channel.id != request.channel_id
                {
                    return Err(ApiError::typed_detail(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "owner_unavailable",
                        "The session server returned a mismatched start response",
                        serde_json::json!({"retry": "now", "owner_decided": false}),
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
    Err(api_error_from(peer_error(last_transport), Decided::Ingress))
}

async fn owner_activate_within(
    state: &AppState,
    config: &LiveTvConfig,
    request: &LiveTvActivateRequest,
    budget: tokio::time::Instant,
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
    let transport = &state.live_tv_peers;
    let mut last_transport = PeerTransportError::TimedOut;
    for attempt in 0..2 {
        if budget_exhausted(budget) {
            break;
        }
        match transport
            .request(
                &node_id,
                &base,
                reqwest::Method::POST,
                ACTIVATE_PATH,
                body.clone(),
                stage_deadline(CONTROL_EXCHANGE_DEADLINE, budget),
                MAX_START_RESPONSE_BYTES,
                PeerAuthMode::ExactRequestAndResponse,
            )
            .await
        {
            Ok(response) if response.status.is_success() => {
                let mut activated = serde_json::from_slice::<LiveTvActivated>(&response.body)
                    .map_err(|_| {
                        ApiError::typed_detail(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "owner_unavailable",
                            "The session server returned an invalid activation response",
                            serde_json::json!({"retry": "now", "owner_decided": false}),
                        )
                    })?;
                if activated.session_id != request.capability {
                    return Err(ApiError::typed_detail(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "owner_unavailable",
                        "The session server returned a mismatched activation response",
                        serde_json::json!({"retry": "now", "owner_decided": false}),
                    ));
                }
                activated.playlist_url = format!(
                    "/api/v1/live-tv/sessions/{}/master.m3u8",
                    request.capability
                );
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
    Err(api_error_from(peer_error(last_transport), Decided::Ingress))
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
        let response = state
            .live_tv_peers
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
            .map_err(|error| api_error_from(peer_error(error), Decided::Ingress))?;
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
    let response = state
        .live_tv_peers
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
        .map_err(|error| api_error_from(peer_error(error), Decided::Ingress))?;
    if !response.status().is_success() {
        return Err(ApiError::typed(
            StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            if response.status() == reqwest::StatusCode::GONE {
                "capability_expired"
            } else {
                "owner_unavailable"
            },
            "The session server could not serve this live resource",
        ));
    }
    crate::media_sessions::relay_response_with_limits_counted_bounded(
        response,
        Duration::from_secs(60),
        Duration::from_secs(15),
        state.live_tv.relay_counter(),
        max_body_bytes,
    )
    .map_err(|error| api_error_from(peer_error(error), Decided::Ingress))
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
    let response = state
        .live_tv_peers
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
        .map_err(|error| api_error_from(peer_error(error), Decided::Ingress))?;
    if response.status.is_success() {
        Ok(())
    } else {
        Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "The session server could not stop this live session",
        ))
    }
}

/// Local on the owner, a signed peer exchange otherwise. An owner too old to
/// know these paths answers 404, which becomes a typed answer that proves
/// nothing about the tuner — exactly what the client's keep-the-hint rule
/// needs it to be.
async fn owner_retire(
    state: &AppState,
    config: &LiveTvConfig,
    user_id: i64,
    request_id: &str,
) -> Result<crate::live_tv::LiveTvRetireOutcome, ApiError> {
    if config.owner_node_id == state.node_id {
        return Ok(state.live_tv.retire_local(user_id, request_id).await);
    }
    let body = serde_json::to_vec(&crate::live_tv::LiveTvRetireRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        user_id,
        request_id: request_id.to_owned(),
    })
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    owner_recovery_exchange(state, config, RETIRE_PATH, body, "retire starts yet").await
}

async fn owner_resume(
    state: &AppState,
    config: &LiveTvConfig,
    user_id: i64,
    request_id: &str,
) -> Result<crate::live_tv::LiveTvResumeAnswer, ApiError> {
    if config.owner_node_id == state.node_id {
        return Ok(state.live_tv.resume_local(user_id, request_id).await);
    }
    let body = serde_json::to_vec(&crate::live_tv::LiveTvResumeRequest {
        expected_owner_node_id: config.owner_node_id.clone(),
        user_id,
        request_id: request_id.to_owned(),
    })
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    owner_recovery_exchange(state, config, RESUME_PATH, body, "resume starts yet").await
}

async fn owner_recovery_exchange<T: serde::de::DeserializeOwned>(
    state: &AppState,
    config: &LiveTvConfig,
    path: &'static str,
    body: Vec<u8>,
    missing: &str,
) -> Result<T, ApiError> {
    let (node_id, base) = owner_peer(state, &config.owner_node_id).await?;
    let response = state
        .live_tv_peers
        .request(
            &node_id,
            &base,
            reqwest::Method::POST,
            path,
            body,
            deadline_after(CONTROL_EXCHANGE_DEADLINE),
            MAX_START_RESPONSE_BYTES,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(|error| api_error_from(peer_error(error), Decided::Ingress))?;
    if response.status == reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError::typed_detail(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            format!("the session server does not {missing}"),
            serde_json::json!({"retry": "now", "owner_decided": false}),
        ));
    }
    if !response.status.is_success() {
        return Err(wire_api_error(response.status, &response.body));
    }
    serde_json::from_slice::<T>(&response.body).map_err(|_| {
        ApiError::typed_detail(
            StatusCode::SERVICE_UNAVAILABLE,
            "owner_unavailable",
            "The session server returned an invalid recovery response",
            serde_json::json!({"retry": "now", "owner_decided": false}),
        )
    })
}

/// Same order as `PEER_STATUS_CACHE_TTL` in `cluster_operations`, and one sixth
/// of the roster's own 30 s `NODE_REACHABLE_WINDOW_MS`.
///
/// What this bounds is the staleness this memory *adds* to the roster's, not
/// how soon a dead peer is noticed. The memory refills from `activity_peers`,
/// which keeps reporting a silent peer `reachable` for the whole 30 s window,
/// so a peer that stops heartbeating stays here for as long as the roster calls
/// it reachable plus at most this TTL. The TTL cannot make the memory notice a
/// dead peer before the roster does. What stops an ingress relaying at a dead
/// owner is the other rule: every `PeerTransportError` from an exchange drops
/// the entry, so the first failed request re-resolves.
pub(crate) const OWNER_PEER_TTL: Duration = Duration::from_secs(5);

/// Where the owner was last resolved to be. Only an address: which node owns a
/// capability is decided per request by `capability_owner`, and this is
/// consulted only after that decision has already named `node_id`.
struct CachedOwnerPeer {
    node_id: String,
    http_base: String,
    resolved_at: tokio::time::Instant,
}

/// One HTTP client for every Live TV ingress-to-owner exchange, plus a short
/// memory of where the owner is.
///
/// Nothing about a credential is cached: `PeerTransport` signs each request
/// and verifies each response from `membership` exactly as it did when every
/// call site built its own client. What is saved is the client (a connection
/// pool that used to be built and dropped per request) and the
/// `activity_peers` read behind it.
pub(crate) struct LiveTvPeers {
    transport: PeerTransport,
    owner: StdMutex<Option<CachedOwnerPeer>>,
    metrics: std::sync::Arc<LiveTvPeerMetrics>,
}

/// The counters alone, so the `/metrics` substate can read them without
/// holding this node's Live TV HTTP client.
#[derive(Default)]
pub(crate) struct LiveTvPeerMetrics {
    hits: AtomicU64,
    resolutions: AtomicU64,
    invalidations: AtomicU64,
}

impl LiveTvPeerMetrics {
    /// Three series an operator reads together: `hit` should dwarf `resolved`
    /// on a busy ingress, and a climbing `invalidated` is the owner moving or
    /// a peer refusing this node's signatures, not cache pressure.
    pub(crate) fn prometheus(&self) -> String {
        format!(
            "# HELP plurx_live_tv_owner_peer_total Live TV owner-address resolutions on an ingress, by outcome.\n\
             # TYPE plurx_live_tv_owner_peer_total counter\n\
             plurx_live_tv_owner_peer_total{{outcome=\"hit\"}} {}\n\
             plurx_live_tv_owner_peer_total{{outcome=\"resolved\"}} {}\n\
             plurx_live_tv_owner_peer_total{{outcome=\"invalidated\"}} {}\n",
            self.hits.load(Ordering::Acquire),
            self.resolutions.load(Ordering::Acquire),
            self.invalidations.load(Ordering::Acquire),
        )
    }
}

impl LiveTvPeers {
    pub(crate) fn new(
        membership: plurx_core::cluster::membership::MembershipManager,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            transport: PeerTransport::new(membership),
            owner: StdMutex::new(None),
            metrics: std::sync::Arc::new(LiveTvPeerMetrics::default()),
        })
    }

    pub(crate) fn metrics_handle(&self) -> std::sync::Arc<LiveTvPeerMetrics> {
        std::sync::Arc::clone(&self.metrics)
    }

    fn owner_slot(&self) -> std::sync::MutexGuard<'_, Option<CachedOwnerPeer>> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drop the remembered address. Called whenever an exchange proved the
    /// address wrong rather than merely unlucky — see `note_owner_exchange`.
    fn invalidate_owner(&self) {
        if self.owner_slot().take().is_some() {
            self.metrics.invalidations.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The owner's `(node_id, http_base)`, resolved at most once per
    /// `OWNER_PEER_TTL` or per invalidation.
    ///
    /// `expected` is the node a capability already named, so a cached entry
    /// for a different node is a miss rather than an answer: the cache maps a
    /// node id to a base and never chooses the owner.
    async fn owner_base<F, Fut, E>(&self, expected: &str, resolve: F) -> Result<(String, String), E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(String, String), E>>,
    {
        let now = tokio::time::Instant::now();
        {
            let cached = self.owner_slot();
            if let Some(entry) = cached.as_ref() {
                if entry.node_id == expected
                    && now.duration_since(entry.resolved_at) < OWNER_PEER_TTL
                {
                    let answer = (entry.node_id.clone(), entry.http_base.clone());
                    drop(cached);
                    self.metrics.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(answer);
                }
            }
        }
        let (node_id, http_base) = resolve().await?;
        self.metrics.resolutions.fetch_add(1, Ordering::Relaxed);
        *self.owner_slot() = Some(CachedOwnerPeer {
            node_id: node_id.clone(),
            http_base: http_base.clone(),
            resolved_at: tokio::time::Instant::now(),
        });
        Ok((node_id, http_base))
    }

    /// What an exchange proved about *where the owner is*.
    ///
    /// Every `PeerTransportError` drops the entry, for two different reasons:
    /// `Unreachable` and `TimedOut` say the address or its reachability
    /// changed, and `InvalidResponse` says a key or capability on the peer did
    /// — neither is something to keep routing at for the rest of the TTL. A
    /// 404 on an internal path is the third: the peer is an older build that
    /// does not serve this route, and re-resolving is the only way this node
    /// ever notices a newer peer taking over. Any other status means the owner
    /// answered from the address we had, which is exactly what the entry
    /// claims.
    fn note_owner_exchange(&self, outcome: Result<reqwest::StatusCode, PeerTransportError>) {
        match outcome {
            Err(_) | Ok(reqwest::StatusCode::NOT_FOUND) => self.invalidate_owner(),
            Ok(_) => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
    ) -> Result<PeerResponse, PeerTransportError> {
        let outcome = self
            .transport
            .request(
                expected_node_id,
                base,
                method,
                path,
                body,
                deadline,
                max_response_bytes,
                auth_mode,
            )
            .await;
        self.note_owner_exchange(
            outcome
                .as_ref()
                .map(|response| response.status)
                .map_err(|error| *error),
        );
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_stream(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        auth_mode: PeerAuthMode,
    ) -> Result<reqwest::Response, PeerTransportError> {
        let outcome = self
            .transport
            .request_stream(
                expected_node_id,
                base,
                method,
                path,
                body,
                deadline,
                auth_mode,
            )
            .await;
        self.note_owner_exchange(
            outcome
                .as_ref()
                .map(reqwest::Response::status)
                .map_err(|error| *error),
        );
        outcome
    }
}

/// Every failure here happens *before* a request leaves this node, so none of
/// them is the owner's verdict — and a client that reads one as a verdict
/// throws away the hint that is its only handle on a session the owner may
/// still be feeding. A fleet deploy fences peers for a fraction of a second
/// several times a night; that window lands here.
async fn owner_peer(state: &AppState, expected: &str) -> Result<(String, String), ApiError> {
    if !state.membership.is_replicated() {
        return Err(api_error_from(
            LiveTvError::OwnerUnavailable(
                "the selected owner is not this single-node server".into(),
            ),
            Decided::Ingress,
        ));
    }
    state
        .live_tv_peers
        .owner_base(expected, || resolve_owner_peer(state, expected))
        .await
}

/// The roster read the cache above stands in front of: one node-local hiqlite
/// `query_map` plus the in-process metrics read inside `activity_peers`.
async fn resolve_owner_peer(
    state: &AppState,
    expected: &str,
) -> Result<(String, String), ApiError> {
    state
        .membership
        .activity_peers()
        .await
        .map_err(|error| {
            api_error_from(
                LiveTvError::OwnerUnavailable(error.to_string()),
                Decided::Ingress,
            )
        })?
        .into_iter()
        .find(|peer| peer.node_id == expected && peer.reachable)
        .and_then(|peer| peer.http_base.map(|base| (peer.node_id, base)))
        .ok_or_else(|| {
            api_error_from(
                LiveTvError::OwnerUnavailable(
                    "the selected session server is not a reachable committed voter".into(),
                ),
                Decided::Ingress,
            )
        })
}

/// An error the owner signed, turned into the same envelope a locally minted
/// one gets. The two extra fields are functions of *where the body came from*
/// and *what the code is*, so they are produced here rather than added to the
/// internal wire: a body that arrived as a signed owner response is
/// owner-decided, whatever its code — including one this function folds into
/// `owner_unavailable`, because the owner still answered.
fn wire_api_error(status: reqwest::StatusCode, body: &[u8]) -> ApiError {
    let wire = serde_json::from_slice::<WireError>(body).ok();
    let code = wire
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("owner_unavailable")
        .to_owned();
    let message = wire
        .as_ref()
        .map(|error| sanitize_public_error(&error.message))
        .unwrap_or_else(|| "The session server could not complete the request".into());
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    // The owner is the only node that knows what holds its tuners, so an
    // ingress that dropped this field would make the "stop a recording and
    // watch" offer exist on one node and not the others. Re-parsed rather
    // than forwarded whole: a peer may not inject arbitrary fields, and this
    // one has a shape.
    let holders = serde_json::from_slice::<WireCapacityDetail>(body)
        .ok()
        .map(|detail| detail.holders)
        .filter(|holders| !holders.is_empty());
    let decided_by_owner = wire.is_some();
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
    let mut detail = serde_json::json!({
        "retry": retry_advice_for_code(stable),
        "owner_decided": decided_by_owner,
    });
    // The holders ride alongside the retry advice rather than replacing it: a
    // viewer refused for capacity needs both what to do and what is in the
    // way, and a client that reads only one of the two is not a reason to send
    // only one of the two.
    if stable == "tuner_capacity" {
        if let (Some(holders), Some(fields)) = (holders, detail.as_object_mut()) {
            fields.insert(
                "holders".to_owned(),
                serde_json::to_value(&holders).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    ApiError::typed_detail(status, stable, message, detail)
}

/// The one extra field a relayed capacity refusal may carry.
#[derive(Deserialize)]
struct WireCapacityDetail {
    #[serde(default)]
    holders: Vec<crate::live_tv::dvr::DvrHolder>,
}

async fn owner_snapshot(
    state: &AppState,
    config: &LiveTvConfig,
    force: bool,
    probe_graph: bool,
) -> Result<LiveTvSnapshot, LiveTvError> {
    let deadline = deadline_after(PUBLIC_START_DEADLINE);
    let mut candidate = config.clone();
    let mut nodes = vec![state.node_id.clone()];
    if state.membership.is_replicated() {
        if let Ok(peers) = state.membership.activity_peers().await {
            nodes.extend(
                peers
                    .into_iter()
                    .filter(|p| p.reachable && p.http_base.is_some() && p.node_id != state.node_id)
                    .map(|p| p.node_id),
            );
        }
    }
    let mut last = LiveTvError::OwnerUnavailable("no eligible tuner worker".into());
    for node in nodes.into_iter().take(3) {
        candidate.owner_node_id = node;
        match owner_snapshot_within(
            state,
            &candidate,
            force,
            probe_graph,
            deadline.min(deadline_after(Duration::from_secs(10))),
        )
        .await
        {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => last = error,
        }
    }
    Err(last)
}

async fn owner_snapshot_within(
    state: &AppState,
    config: &LiveTvConfig,
    force: bool,
    probe_graph: bool,
    budget: tokio::time::Instant,
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
        return tokio::time::timeout_at(
            budget,
            state.live_tv.local_snapshot(config, force, probe_graph),
        )
        .await
        .map_err(|_| LiveTvError::OwnerUnavailable("tuner observation timed out".into()))?;
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
                "the selected session server is not a reachable committed voter".to_owned(),
            )
        })?;
    let body = serde_json::to_vec(&SnapshotRequest {
        generation: config.generation,
        force,
        probe_graph,
    })
    .map_err(|error| LiveTvError::InvalidResponse(error.to_string()))?;
    let response = state
        .live_tv_peers
        .request(
            &peer.0,
            &peer.1,
            reqwest::Method::POST,
            SNAPSHOT_PATH,
            body,
            stage_deadline(SNAPSHOT_DEADLINE, budget),
            MAX_SNAPSHOT_BYTES,
            PeerAuthMode::ExactRequestAndResponse,
        )
        .await
        .map_err(peer_error)?;
    if !response.status.is_success() {
        return Err(LiveTvError::OwnerUnavailable(format!(
            "session server returned HTTP {}",
            response.status.as_u16()
        )));
    }
    let snapshot = serde_json::from_slice::<LiveTvSnapshot>(&response.body).map_err(|_| {
        LiveTvError::InvalidResponse("session server returned an invalid snapshot".to_owned())
    })?;
    if snapshot.generation != config.generation {
        return Err(LiveTvError::OwnerUnavailable(
            "session server returned a stale configuration generation".to_owned(),
        ));
    }
    Ok(snapshot)
}

fn peer_error(error: PeerTransportError) -> LiveTvError {
    let message = match error {
        PeerTransportError::Unreachable => "the session server is unreachable",
        PeerTransportError::TimedOut => "the session server timed out",
        PeerTransportError::InvalidResponse => "the session server returned an invalid response",
    };
    LiveTvError::OwnerUnavailable(message.to_owned())
}

/// Who decided the answer a client is about to read.
///
/// `Owner` means the session server looked at the request and refused it: the
/// client learns something about the tuner, and its hint is spent. `Ingress`
/// means this node refused before an owner ever saw it, or could not tell
/// whether one had — in which case a session may exist and the hint is the
/// only handle for retiring it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decided {
    Owner,
    Ingress,
}

/// What a client should do next with this code, as a word rather than a
/// number: `now` the next press may work, `later` this channel needs time,
/// `never` pressing again changes nothing.
fn retry_advice(error: &LiveTvError) -> &'static str {
    match error {
        LiveTvError::Disabled(_)
        | LiveTvError::ChannelNotFound(_)
        | LiveTvError::DrmUnsupported(_)
        | LiveTvError::CodecUnsupported(_) => "never",
        LiveTvError::InvalidConfig(_)
        | LiveTvError::InvalidResponse(_)
        | LiveTvError::DeviceUnavailable(_)
        | LiveTvError::Capacity(_)
        | LiveTvError::StartupTimeout(_) => "later",
        LiveTvError::OwnerUnavailable(_)
        | LiveTvError::TunerUnavailable(_)
        | LiveTvError::StreamFailed(_)
        | LiveTvError::Conflict(_)
        | LiveTvError::SourceFormatChanged(_)
        | LiveTvError::CapabilityExpired(_) => "now",
    }
}

/// The same advice keyed by a stable wire code, for an answer that arrived as
/// a signed owner response rather than as a local `LiveTvError`.
fn retry_advice_for_code(code: &str) -> &'static str {
    match code {
        "live_tv_disabled" | "channel_not_found" | "drm_unsupported" | "codec_unsupported" => {
            "never"
        }
        "tuner_capacity" | "startup_timeout" | "device_unavailable" | "invalid_settings" => "later",
        _ => "now",
    }
}

pub(crate) fn api_error(error: LiveTvError) -> ApiError {
    api_error_from(error, Decided::Owner)
}

/// Every Live TV error leaves here as a typed body with two extra flat fields.
/// `typed_detail` writes `code` and `message` last, so a client reading only
/// those two is unaffected by their presence.
pub(crate) fn api_error_from(error: LiveTvError, decided: Decided) -> ApiError {
    let code = error.code();
    let detail = serde_json::json!({
        "retry": retry_advice(&error),
        "owner_decided": decided == Decided::Owner,
    });
    let (status, message) = match &error {
        LiveTvError::InvalidConfig(message) | LiveTvError::InvalidResponse(message) => {
            (StatusCode::BAD_REQUEST, message.clone())
        }
        LiveTvError::DeviceUnavailable(message) | LiveTvError::OwnerUnavailable(message) => (
            StatusCode::SERVICE_UNAVAILABLE,
            sanitize_public_error(message),
        ),
        LiveTvError::Disabled(message)
        | LiveTvError::Capacity(message)
        | LiveTvError::TunerUnavailable(message) => {
            (StatusCode::SERVICE_UNAVAILABLE, message.clone())
        }
        LiveTvError::ChannelNotFound(message) => (StatusCode::NOT_FOUND, message.clone()),
        LiveTvError::DrmUnsupported(message) | LiveTvError::CodecUnsupported(message) => {
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, message.clone())
        }
        LiveTvError::StartupTimeout(message) => (StatusCode::REQUEST_TIMEOUT, message.clone()),
        LiveTvError::StreamFailed(message) => {
            (StatusCode::BAD_GATEWAY, sanitize_public_error(message))
        }
        LiveTvError::Conflict(message) | LiveTvError::SourceFormatChanged(message) => {
            (StatusCode::CONFLICT, message.clone())
        }
        LiveTvError::CapabilityExpired(message) => (StatusCode::GONE, message.clone()),
    };
    ApiError::typed_detail(status, code, message, detail)
}

/// The capacity refusal, told what is actually holding the tuners.
///
/// A viewer refused a tuner by their own recordings is owed more than "all
/// slots are in use": which channels are held, by which recordings, and until
/// when — so the client can offer to stop one instead of leaving them to guess
/// where their television went. A transport with two sinks is listed but not
/// offered as a single stop, because stopping one of two recordings on a
/// channel frees nothing.
///
/// The holders are added to the refusal the ordinary path already built rather
/// than replacing it, so `retry` and `owner_decided` are still there for a
/// client that reads those and not this.
///
/// `watchable` rides beside them (plan L-03 §3.4): channels someone is
/// already watching or recording that would take one more viewer, which cost
/// no tuner to join. Same row shape; an owner-local refusal only, like the
/// holders, because the signed relay shape does not change.
pub(crate) fn capacity_error(
    error: LiveTvError,
    holders: Vec<crate::live_tv::dvr::DvrHolder>,
    watchable: Vec<crate::live_tv::dvr::DvrHolder>,
) -> ApiError {
    let mut refusal = api_error(error);
    if holders.is_empty() && watchable.is_empty() {
        return refusal;
    }
    if let ApiError::TypedDetail { code, detail, .. } = &mut refusal {
        if *code == "tuner_capacity" {
            for (field, rows) in [("holders", holders), ("watchable", watchable)] {
                if !rows.is_empty() {
                    detail.insert(
                        field.to_owned(),
                        serde_json::to_value(&rows).unwrap_or(serde_json::Value::Null),
                    );
                }
            }
        }
    }
    refusal
}

fn sanitize_public_error(message: &str) -> String {
    if message.contains("http://") || message.contains("https://") {
        "The HDHomeRun worker could not complete the request; see owner logs for details".to_owned()
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

    async fn body_of(error: ApiError) -> serde_json::Value {
        let response = axum::response::IntoResponse::into_response(error);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn a_capacity_refusal_names_the_channels_a_viewer_could_join_instead() {
        let row = |channel: &str| crate::live_tv::dvr::DvrHolder {
            channel_id: channel.to_owned(),
            guide_number: channel.to_owned(),
            channel_name: "Fixture".to_owned(),
            sinks: Vec::new(),
        };
        let body = body_of(capacity_error(
            LiveTvError::Capacity("all 2 tuners plurx Live TV may use are in use".into()),
            Vec::new(),
            vec![row("2.1"), row("4.1")],
        ))
        .await;
        let watchable = body["detail"]["watchable"]
            .as_array()
            .or_else(|| body["watchable"].as_array())
            .unwrap_or_else(|| panic!("no watchable rows: {body}"));
        assert_eq!(watchable.len(), 2, "{body}");
        assert_eq!(watchable[0]["guide_number"], "2.1");
        assert!(
            body.get("holders").is_none() && body["detail"].get("holders").is_none(),
            "no recording holds a tuner, so there is nothing to offer to stop: {body}"
        );

        let other = body_of(capacity_error(
            LiveTvError::TunerUnavailable("the device refused".into()),
            Vec::new(),
            vec![row("2.1")],
        ))
        .await;
        assert!(
            !other.to_string().contains("watchable"),
            "only a capacity refusal offers another channel: {other}"
        );
    }

    #[test]
    fn a_client_reads_what_this_whole_path_accepts_not_what_the_owner_alone_does() {
        // An owner advertising 3 says nothing about the ingress a client is
        // actually talking to, and an older ingress rejects the new field and
        // has none of the recovery routes. The published list is the
        // intersection, which is the only thing that is true end to end.
        assert_eq!(negotiated_protocols(&[]), Vec::<u8>::new());
        assert_eq!(negotiated_protocols(&[1, 2]), vec![1, 2]);
        assert_eq!(negotiated_protocols(&[1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(
            negotiated_protocols(&[1, 2, 3, 4]),
            vec![1, 2, 3],
            "an owner newer than this ingress does not make this ingress newer"
        );
    }

    #[test]
    fn a_public_request_id_is_128_bits_of_hex_or_it_is_not_one() {
        assert!(valid_public_request_id(&"a".repeat(32)));
        assert!(valid_public_request_id("0123456789abcdef0123456789abcdef"));
        assert!(!valid_public_request_id(&"a".repeat(31)));
        assert!(!valid_public_request_id(&"a".repeat(33)));
        assert!(
            !valid_public_request_id("0123456789ABCDEF0123456789ABCDEF"),
            "one spelling, so two clients cannot key the same start differently"
        );
        assert!(!valid_public_request_id("0123456789abcdef0123456789abcde-"));
        assert!(
            !valid_public_request_id("../../etc/passwd/aaaaaaaaaaaaaaa"),
            "the id reaches a path segment and a registry key"
        );
        assert_eq!(
            uuid::Uuid::new_v4().simple().to_string().len(),
            32,
            "the id the ingress mints for an older client has the same spelling"
        );
        assert!(valid_public_request_id(
            &uuid::Uuid::new_v4().simple().to_string()
        ));
    }

    #[tokio::test]
    async fn every_live_tv_error_says_what_to_do_next_and_who_decided_it() {
        let owner = body_of(api_error(LiveTvError::TunerUnavailable(
            "every tuner is busy".into(),
        )))
        .await;
        assert_eq!(owner["code"], "tuner_unavailable");
        assert_eq!(owner["retry"], "now");
        assert_eq!(
            owner["owner_decided"], true,
            "the owner looked at this and refused it, so the client's hint is spent"
        );

        let ingress = body_of(api_error_from(
            LiveTvError::OwnerUnavailable("the session server timed out".into()),
            Decided::Ingress,
        ))
        .await;
        assert_eq!(ingress["code"], "owner_unavailable");
        assert_eq!(
            ingress["owner_decided"], false,
            "nobody knows whether a session exists, so the hint is the only handle"
        );

        assert_eq!(
            body_of(api_error(LiveTvError::StartupTimeout("slow".into()))).await["retry"],
            "later"
        );
        assert_eq!(
            body_of(api_error(LiveTvError::Disabled("off".into()))).await["retry"],
            "never"
        );
        assert_eq!(
            body_of(api_error(LiveTvError::ChannelNotFound("gone".into()))).await["retry"],
            "never"
        );
        assert_eq!(
            body_of(api_error(LiveTvError::Capacity("full".into()))).await["retry"],
            "later"
        );
    }

    #[tokio::test]
    async fn an_owner_that_answered_is_owner_decided_even_when_its_code_is_folded() {
        // The remote path carries the same two fields without any change to
        // the internal wire: they are functions of where the body came from.
        let answered = body_of(wire_api_error(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            br#"{"code":"tuner_unavailable","message":"every tuner is busy"}"#,
        ))
        .await;
        assert_eq!(answered["code"], "tuner_unavailable");
        assert_eq!(answered["retry"], "now");
        assert_eq!(answered["owner_decided"], true);

        let folded = body_of(wire_api_error(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            br#"{"code":"something_this_build_does_not_know","message":"nope"}"#,
        ))
        .await;
        assert_eq!(
            folded["code"], "owner_unavailable",
            "an unknown code folds to the safe one"
        );
        assert_eq!(
            folded["owner_decided"], true,
            "but the owner still answered, and that is what the field means"
        );

        let unparseable = body_of(wire_api_error(
            reqwest::StatusCode::BAD_GATEWAY,
            b"<html>a proxy ate it</html>",
        ))
        .await;
        assert_eq!(unparseable["code"], "owner_unavailable");
        assert_eq!(
            unparseable["owner_decided"], false,
            "a body that is not a signed owner error proves nothing"
        );
    }

    #[test]
    fn a_public_start_cannot_outlive_the_clients_own_timeout() {
        // The clients give up at 45 s. A start that outlives that leaves a
        // session with nobody holding it, which is the failure this whole
        // effort exists to end.
        //
        // The budget alone is not the answer: a start that fails after a
        // provisional was issued still has to stop it, and that stop is
        // awaited on the response path with a budget of its own. The sum is
        // what the client experiences, so the sum is what is asserted.
        const CLIENT_START_TIMEOUT: Duration = Duration::from_secs(45);
        let worst_case_cleanup =
            CONTROL_EXCHANGE_DEADLINE.max(crate::live_tv::SESSION_DRAIN_TIMEOUT);
        assert!(
            PUBLIC_START_DEADLINE + worst_case_cleanup < CLIENT_START_TIMEOUT,
            "budget {PUBLIC_START_DEADLINE:?} plus the awaited stop              {worst_case_cleanup:?} must land inside {CLIENT_START_TIMEOUT:?}"
        );
        assert!(PUBLIC_START_DEADLINE < CLIENT_START_TIMEOUT);
        assert!(
            START_EXCHANGE_ATTEMPT < PUBLIC_START_DEADLINE,
            "one attempt must never be able to consume the entire budget"
        );
        let budget = deadline_after(Duration::from_secs(1));
        assert!(
            stage_deadline(SNAPSHOT_DEADLINE, budget) <= budget,
            "no stage may reach past the public budget, however generous its own constant"
        );
        assert!(
            stage_deadline(Duration::from_millis(10), budget) < budget,
            "and a stage tighter than the budget keeps its own bound"
        );
    }

    /// A `LiveTvPeers` with no membership behind it. Every assertion below is
    /// about the cache in front of the roster read, so the resolver is a
    /// closure the test counts; the transport is never used.
    fn test_peers() -> std::sync::Arc<LiveTvPeers> {
        LiveTvPeers::new(plurx_core::cluster::membership::MembershipManager::unavailable())
    }

    async fn resolved(
        peers: &LiveTvPeers,
        expected: &str,
        base: &'static str,
        calls: &AtomicU64,
    ) -> Result<(String, String), ()> {
        peers
            .owner_base(expected, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok((expected.to_owned(), base.to_owned()))
            })
            .await
    }

    #[tokio::test(start_paused = true)]
    async fn relayed_requests_share_one_client_and_resolve_the_owner_once_per_ttl() {
        // Twenty relayed resources inside one second is an ordinary playlist
        // poll from four clients. Before this cache each one did its own
        // `activity_peers` read — a node-local SQL query plus an in-process
        // metrics read — and built its own `reqwest::Client`.
        let peers = test_peers();
        let calls = AtomicU64::new(0);
        for _ in 0..20 {
            let answer = resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
                .await
                .expect("resolution");
            assert_eq!(
                answer,
                ("owner-a".to_owned(), "http://owner-a:8080".to_owned())
            );
            tokio::time::advance(Duration::from_millis(50)).await;
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "one resolution for twenty relayed requests inside the TTL"
        );
        assert_eq!(peers.metrics.hits.load(Ordering::Relaxed), 19);
        assert_eq!(peers.metrics.resolutions.load(Ordering::Relaxed), 1);

        // And the TTL really is a bound, not a latch: past it the roster is
        // read again, because `reachable` is derived from 10 s heartbeats and
        // a five-second-old answer is the most this may assert.
        tokio::time::advance(OWNER_PEER_TTL).await;
        resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
            .await
            .expect("resolution");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_owner_exchange_invalidates_the_cached_peer() {
        // Each of the three transport failures says something different about
        // the peer, and all three say the address is no longer proven: an
        // unreachable or timed-out owner may have moved, and a response this
        // node cannot verify means a key or capability changed under it.
        for error in [
            PeerTransportError::Unreachable,
            PeerTransportError::TimedOut,
            PeerTransportError::InvalidResponse,
        ] {
            let peers = test_peers();
            let calls = AtomicU64::new(0);
            resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
                .await
                .expect("first resolution");
            peers.note_owner_exchange(Err(error));
            resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
                .await
                .expect("second resolution");
            assert_eq!(
                calls.load(Ordering::Relaxed),
                2,
                "{error:?} must re-resolve rather than keep routing at the old address"
            );
            assert_eq!(peers.metrics.invalidations.load(Ordering::Relaxed), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_owner_that_does_not_serve_an_internal_path_is_re_resolved_and_any_other_status_is_not(
    ) {
        // A 404 on an internal path is an older build answering, so the next
        // request looks for the owner again rather than spending the TTL on a
        // peer that cannot serve the route. Every other status — including the
        // 503 a busy owner returns and the 410 an expired capability gets — was
        // answered *from this address*, which is all the entry claims.
        let peers = test_peers();
        let calls = AtomicU64::new(0);
        resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
            .await
            .expect("first resolution");
        for status in [
            reqwest::StatusCode::OK,
            reqwest::StatusCode::GONE,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::CONFLICT,
        ] {
            peers.note_owner_exchange(Ok(status));
            resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
                .await
                .expect("cached");
            assert_eq!(calls.load(Ordering::Relaxed), 1, "{status} kept the entry");
        }
        peers.note_owner_exchange(Ok(reqwest::StatusCode::NOT_FOUND));
        resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
            .await
            .expect("re-resolution");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(peers.metrics.invalidations.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_owner_move_is_a_miss_and_never_answers_for_the_wrong_node() {
        // The capability names its owner before this is consulted, so an entry
        // for another node is not an answer at all. Returning the cached base
        // here would relay a capability to a node that does not hold it.
        let peers = test_peers();
        let calls = AtomicU64::new(0);
        resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
            .await
            .expect("owner A");
        let moved = resolved(&peers, "owner-b", "http://owner-b:8080", &calls)
            .await
            .expect("owner B");
        assert_eq!(moved.0, "owner-b");
        assert_eq!(moved.1, "http://owner-b:8080");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(peers.metrics.hits.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_resolution_is_not_remembered_as_an_address() {
        // A roster read that found no reachable owner must leave the cache
        // empty: caching the failure would turn one unreachable moment into
        // five seconds of them, and caching the *previous* address would route
        // at a node the roster has just stopped calling reachable.
        let peers = test_peers();
        let calls = AtomicU64::new(0);
        let failed: Result<(String, String), ()> = peers
            .owner_base("owner-a", || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Err(())
            })
            .await;
        assert!(failed.is_err());
        assert_eq!(peers.metrics.resolutions.load(Ordering::Relaxed), 0);
        resolved(&peers, "owner-a", "http://owner-a:8080", &calls)
            .await
            .expect("resolution after the failure");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn the_owner_peer_ttl_stays_inside_the_window_the_roster_derives_reachability_from() {
        // `reachable` is `now - last_seen_at <= 30 s`, refreshed by 10 s
        // heartbeats. The memory refills from that roster, so it can never
        // notice a silent peer sooner than the roster does; the TTL only bounds
        // how much staleness it adds on top (here at most 5 s over the 30 s
        // window). A dead owner is dropped from the memory by the first failed
        // exchange, which `a_failed_owner_exchange_invalidates_the_cached_peer`
        // pins. This guard keeps the added staleness small against the window.
        assert!(OWNER_PEER_TTL <= Duration::from_secs(30) / 6);
    }

    #[test]
    fn peer_transport_errors_have_stable_operator_messages() {
        assert_eq!(
            peer_error(PeerTransportError::TimedOut).to_string(),
            "the session server timed out"
        );
    }

    #[test]
    fn public_device_errors_never_disclose_the_private_url() {
        let error = api_error(LiveTvError::DeviceUnavailable(
            "request failed for http://10.42.4.20/discover.json".to_owned(),
        ));
        let ApiError::TypedDetail {
            message: rendered, ..
        } = error
        else {
            panic!("expected typed owner error");
        };
        assert!(!rendered.contains("10.42.4.20"), "{rendered}");
        assert!(!rendered.contains("http://"), "{rendered}");
    }

    #[test]
    fn owner_format_change_keeps_its_stable_recovery_code() {
        let error = wire_api_error(
            reqwest::StatusCode::CONFLICT,
            br#"{"code":"source_format_changed","message":"select a fresh route"}"#,
        );
        let ApiError::TypedDetail { code, .. } = error else {
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
            let ApiError::TypedDetail { message, .. } = api_error(error) else {
                panic!("expected typed owner failure");
            };
            assert!(!message.contains("127.0.0.1"), "{message}");
            assert!(!message.contains("http://"), "{message}");
        }
    }
}
