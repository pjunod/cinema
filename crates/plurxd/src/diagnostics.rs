//! Node-local incident projection: positive attribution, bounded memory, no control.
use crate::{
    http::{
        extract::AuthUser,
        peer_transport::{deadline_after, PeerAuthMode, PeerTransport},
    },
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, StreamExt};
use plurx_core::{
    diagnostics::{strict_json, Component, Event, VERSION},
    domain::{MediaSessionRoute, PlaybackEvent, User},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub(crate) const PEER_PATH: &str = "/_internal/v1/diagnostic-snapshot";
const HISTORY_BYTES: usize = 2 * 1024 * 1024;
const JOB_BYTES: usize = 2 * 1024 * 1024;
const JOB_TTL: Duration = Duration::from_secs(900);

struct Record {
    user: i64,
    at: i64,
    event: Event,
    bytes: usize,
    important: bool,
}
#[derive(Clone)]
struct Receipt {
    user: i64,
    correlation: String,
    node: String,
    at: Instant,
}
struct History {
    rows: VecDeque<Record>,
    bytes: usize,
    receipts: VecDeque<Receipt>,
    losses: HashMap<i64, u64>,
}
struct Job {
    user: i64,
    generation: String,
    created: Instant,
    cancel: CancellationToken,
    result: Mutex<Option<Projection>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct Jobs {
    entries: HashMap<String, Arc<Job>>,
    starts: VecDeque<(i64, Instant)>,
}

/// Lives with the existing telemetry owner. Callbacks never await IO or locks.
pub(crate) struct Recorder {
    run: String,
    started: Instant,
    sequence: AtomicU64,
    dropped: AtomicU64,
    history: Mutex<History>,
    jobs: Mutex<Jobs>,
    publisher: OnceLock<crate::diagnostics_delivery::Publisher>,
}
impl Default for Recorder {
    fn default() -> Self {
        Self {
            publisher: OnceLock::new(),
            run: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            history: Mutex::new(History {
                rows: VecDeque::new(),
                bytes: 0,
                receipts: VecDeque::new(),
                losses: HashMap::new(),
            }),
            jobs: Mutex::new(Jobs {
                entries: HashMap::new(),
                starts: VecDeque::new(),
            }),
        }
    }
}
fn generation(user: &User) -> String {
    hex::encode(Sha256::digest(user.password_hash.as_bytes()))
}
fn unix_ms() -> i64 {
    crate::state::clock_ms()
}
fn date(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

impl Recorder {
    pub(crate) fn configure_publisher(&self, config: plurx_core::config::SupportConfig) {
        let _ = self
            .publisher
            .set(crate::diagnostics_delivery::Publisher::new(config));
    }
    pub(crate) fn publisher(&self) -> Option<&crate::diagnostics_delivery::Publisher> {
        self.publisher.get()
    }
    pub(crate) fn observe(&self, raw: &PlaybackEvent) {
        let Some(user) = raw.user_id.filter(|id| *id > 0) else {
            return;
        };
        let mut facts = BTreeMap::new();
        if let Some(ms) = raw.ms.filter(|n| (0..=604_800_000).contains(n)) {
            facts.insert("duration_ms".into(), ms.into());
        }
        if let Some(height) = raw.height.filter(|n| (0..=16_384).contains(n)) {
            facts.insert("height".into(), height.into());
        }
        if let Some(mode) = raw
            .method
            .as_deref()
            .filter(|s| ["direct", "remux", "transcode"].contains(s))
        {
            facts.insert("mode".into(), mode.into());
        }
        // Session fields supplied by client beacons are evidence only. They
        // never create ownership receipts or choose peers.
        if let Some(session) = raw.session_id.as_deref().filter(|s| s.len() <= 256) {
            let safe = if session.starts_with("s-")
                && session.len() == 66
                && session[2..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                session.to_owned()
            } else {
                crate::transcode::session_log_id(session)
            };
            facts.insert("correlation".into(), safe.into());
        }
        let (kind, state) = match raw.event.as_str() {
            "session_start" => ("state", Some("starting")),
            "stall" => ("state", Some("stalled")),
            "stall_end" | "stall_recovery" => ("state", Some("recovered")),
            "suspend" => ("state", Some("paused")),
            "resume" => ("state", Some("playing")),
            "control_terminal" => ("terminal", Some("stopped")),
            "ttff" => ("sample", None),
            _ if raw.level.as_deref() == Some("error") => ("error", Some("failed")),
            _ => ("sample", None),
        };
        if let Some(state) = state {
            facts.insert("state".into(), state.into());
        }
        let event = Event {
            run_id: self.run.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed).to_string(),
            utc: date(raw.at_unix_ms),
            elapsed_ms: self.started.elapsed().as_millis().min(604_800_000) as u32,
            domain: "producer".into(),
            kind: kind.into(),
            facts,
        };
        if event.validate().is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let bytes = serde_json::to_vec(&event)
            .map(|v| v.len() + 1)
            .unwrap_or(2049);
        let Ok(mut history) = self.history.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let important = kind != "sample";
        let now = unix_ms();
        while history
            .rows
            .front()
            .is_some_and(|r| now.saturating_sub(r.at) > 600_000)
        {
            if let Some(r) = history.rows.pop_front() {
                history.bytes = history.bytes.saturating_sub(r.bytes);
            }
        }
        // Preserve a fifth of count capacity for actions/errors/terminals.
        while history.rows.len() >= if important { 10_000 } else { 8_000 }
            || history.bytes + bytes > HISTORY_BYTES
        {
            let index = history.rows.iter().position(|r| !r.important).unwrap_or(0);
            if let Some(r) = history.rows.remove(index) {
                history.bytes = history.bytes.saturating_sub(r.bytes);
                if history.losses.len() < 4096 || history.losses.contains_key(&r.user) {
                    let count = history.losses.entry(r.user).or_default();
                    *count = count.saturating_add(1);
                }
                self.dropped.fetch_add(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
        history.bytes += bytes;
        history.rows.push_back(Record {
            user,
            at: raw.at_unix_ms,
            event,
            bytes,
            important,
        });
    }
    pub(crate) fn route(&self, route: &MediaSessionRoute) {
        let Some(user) = route.principal.local_user_id() else {
            return;
        };
        if route.owner_node_id.is_empty() || route.owner_node_id.len() > 256 {
            return;
        }
        let correlation = crate::transcode::session_log_id(&route.session_id);
        let Ok(mut h) = self.history.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        h.receipts.retain(|r| {
            r.at.elapsed() < Duration::from_secs(86_400)
                && !(r.user == user
                    && r.correlation == correlation
                    && r.node == route.owner_node_id)
        });
        if h.receipts.len() >= 4096 {
            h.receipts.pop_front();
        }
        h.receipts.push_back(Receipt {
            user,
            correlation,
            node: route.owner_node_id.clone(),
            at: Instant::now(),
        });
    }
    fn receipt_nodes(
        &self,
        user: i64,
        correlation: Option<&str>,
    ) -> Result<BTreeSet<String>, StatusCode> {
        let h = self
            .history
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let selected = h.receipts.iter().filter(|r| {
            r.user == user
                && r.at.elapsed() < Duration::from_secs(86_400)
                && correlation.is_none_or(|s| s == r.correlation)
        });
        let nodes = selected.map(|r| r.node.clone()).collect::<BTreeSet<_>>();
        if correlation.is_some() && nodes.is_empty() {
            Err(StatusCode::NOT_FOUND)
        } else {
            Ok(nodes)
        }
    }
    fn local(&self, scope: &PeerScope) -> Result<Projection, StatusCode> {
        if scope.correlation.is_some() {
            self.receipt_nodes(scope.user_id, scope.correlation.as_deref())?;
        }
        let h = self
            .history
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let mut events: Vec<Event> = h
            .rows
            .iter()
            .rev()
            .filter(|r| {
                r.user == scope.user_id
                    && r.at >= scope.since_ms
                    && r.at <= scope.until_ms
                    && scope.correlation.as_ref().is_none_or(|s| {
                        r.event.facts.get("correlation").and_then(|v| v.as_str()) == Some(s)
                    })
            })
            .take(2000)
            .map(|r| r.event.clone())
            .collect();
        events.sort_by_key(|e| e.sequence.parse::<u64>().unwrap_or(0));
        Ok(Projection {
            version: VERSION.into(),
            state: "complete".into(),
            events,
            components: vec![Component {
                name: "server".into(),
                state: "complete".into(),
                reason: None,
            }],
            dropped_events: h
                .losses
                .get(&scope.user_id)
                .copied()
                .unwrap_or(0)
                .to_string(),
        })
    }
    fn cleanup(jobs: &mut Jobs) {
        jobs.entries.retain(|_, j| j.created.elapsed() < JOB_TTL);
        jobs.starts
            .retain(|(_, at)| at.elapsed() < Duration::from_secs(300));
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerScope {
    user_id: i64,
    generation: String,
    since_ms: i64,
    until_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Projection {
    version: String,
    state: String,
    events: Vec<Event>,
    components: Vec<Component>,
    dropped_events: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    incident_at_ms: i64,
    window_ms: u32,
    #[serde(default)]
    include_system: bool,
    #[serde(default)]
    correlation: Option<String>,
}

fn private(value: impl IntoResponse) -> Response {
    let mut r = value.into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    r
}
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .without_v07_checks()
        .route("/api/v1/diagnostics/capabilities", get(capabilities))
        .route("/api/v1/diagnostics/snapshots", post(create))
        .route(
            "/api/v1/diagnostics/snapshots/{id}",
            get(read).delete(erase),
        )
        .route(PEER_PATH, post(peer))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
pub(crate) fn eligible(method: &Method, path: &str) -> bool {
    (method == Method::GET && path == "/api/v1/diagnostics/capabilities")
        || (path == "/api/v1/diagnostics/snapshots" && method == Method::POST)
        || (path.starts_with("/api/v1/diagnostics/snapshots/")
            && (method == Method::GET || method == Method::DELETE))
        || (path == PEER_PATH && method == Method::POST)
}
async fn capabilities(State(state): State<AppState>) -> Response {
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let support = recorder
        .publisher()
        .map(|p| p.capabilities())
        .unwrap_or_else(|| serde_json::json!({"configured":false}));
    private(Json(
        serde_json::json!({"version":VERSION,"snapshots":true,"server_time_ms":unix_ms(),"max_window_ms":600000,"support":support}),
    ))
}
async fn create(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let request: Request =
        serde_json::from_value(strict_json(&body, 16 * 1024).map_err(|_| StatusCode::BAD_REQUEST)?)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    if request.window_ms > 600_000
        || request.incident_at_ms > unix_ms() + 120_000
        || request.incident_at_ms < unix_ms() - 900_000
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if request.include_system && !user.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }
    if request.correlation.as_ref().is_some_and(|s| {
        s.len() != 66
            || !s.starts_with("s-")
            || !s[2..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let scope = PeerScope {
        user_id: user.id,
        generation: generation(&user),
        since_ms: request
            .incident_at_ms
            .saturating_sub(i64::from(request.window_ms)),
        until_ms: request.incident_at_ms,
        correlation: request.correlation,
    };
    let nodes = recorder.receipt_nodes(user.id, scope.correlation.as_deref());
    let id = uuid::Uuid::new_v4().to_string();
    let job = Arc::new(Job {
        user: user.id,
        generation: scope.generation.clone(),
        created: Instant::now(),
        cancel: state.shutdown.child_token(),
        result: Mutex::new(None),
    });
    {
        let mut jobs = recorder
            .jobs
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        Recorder::cleanup(&mut jobs);
        if jobs.starts.len() >= 4096
            || jobs.entries.len() >= 32
            || jobs.entries.values().filter(|j| j.user == user.id).count() >= 2
            || jobs.starts.iter().filter(|(id, _)| *id == user.id).count() >= 3
        {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        jobs.starts.push_back((user.id, Instant::now()));
        jobs.entries.insert(id.clone(), Arc::clone(&job));
    }
    // The snapshot owner enforces physical retention even if no caller polls.
    let expiry_recorder = Arc::downgrade(&recorder);
    let expiry_job = Arc::clone(&job);
    let expiry_id = id.clone();
    tokio::spawn(async move {
        tokio::select! {
            _=tokio::time::sleep(JOB_TTL)=>{},
            _=expiry_job.cancel.cancelled()=>{},
        }
        expiry_job.cancel.cancel();
        if let Ok(mut result) = expiry_job.result.lock() {
            result.take();
        }
        if let Some(recorder) = expiry_recorder.upgrade() {
            if let Ok(mut jobs) = recorder.jobs.lock() {
                jobs.entries.remove(&expiry_id);
            }
        }
    });
    // Every peer future belongs to this task and is dropped on cancel/deadline.
    // No temporary files are needed, so ENOSPC cannot erase the local capture.
    tokio::spawn(async move {
        let collect = collect(&state, &recorder, &scope, nodes, request.include_system);
        let result = tokio::select! {
            _=job.cancel.cancelled()=>None,
            r=tokio::time::timeout(Duration::from_secs(5),collect)=>Some(r.unwrap_or_else(|_| unavailable("timeout"))),
        };
        if let Some(result) = result {
            if let Ok(mut slot) = job.result.lock() {
                if !job.cancel.is_cancelled() {
                    *slot = Some(result);
                }
            }
        }
    });
    Ok(private((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"id":id,"state":"pending","expires_in_ms":900000})),
    )))
}
fn unavailable(reason: &str) -> Projection {
    Projection {
        version: VERSION.into(),
        state: "unavailable".into(),
        events: Vec::new(),
        components: vec![Component {
            name: "server".into(),
            state: "unavailable".into(),
            reason: Some(reason.into()),
        }],
        dropped_events: "0".into(),
    }
}
async fn collect(
    state: &AppState,
    recorder: &Recorder,
    scope: &PeerScope,
    nodes: Result<BTreeSet<String>, StatusCode>,
    include_system: bool,
) -> Projection {
    let Ok(nodes) = nodes else {
        return unavailable("context_unavailable");
    };
    let mut result = recorder
        .local(scope)
        .unwrap_or_else(|_| unavailable("context_unavailable"));
    let remote = nodes
        .iter()
        .filter(|n| *n != &state.node_id)
        .take(7)
        .cloned()
        .collect::<BTreeSet<_>>();
    let omitted = nodes.len() > 8;
    let peers = state.membership.activity_peers().await.unwrap_or_default();
    let transport = PeerTransport::new(state.membership.clone());
    let requests: Vec<_> = remote
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, node)| {
            let peer = peers.iter().find(|p| p.node_id == node).cloned();
            let transport = transport.clone();
            let scope = scope.clone();
            async move {
                let peer = peer?;
                let base = peer.http_base?;
                let bytes = serde_json::to_vec(&scope).ok()?;
                let answer = transport
                    .request(
                        &peer.node_id,
                        &base,
                        reqwest::Method::POST,
                        PEER_PATH,
                        bytes,
                        deadline_after(Duration::from_secs(2)),
                        192 * 1024,
                        PeerAuthMode::ExactRequestAndMemberResponse,
                    )
                    .await
                    .ok()?;
                if !answer.status.is_success() {
                    return None;
                }
                let value = strict_json(&answer.body, 192 * 1024).ok()?;
                let mut projection: Projection = serde_json::from_value(value).ok()?;
                if projection.version != VERSION || projection.events.len() > 2000 {
                    return None;
                }
                for event in &mut projection.events {
                    event.validate().ok()?;
                    event
                        .facts
                        .insert("node".into(), format!("node-{}", index + 2).into());
                }
                Some(projection)
            }
        })
        .collect();
    let futures = stream::iter(requests).buffer_unordered(4);
    futures_util::pin_mut!(futures);
    let mut missing = 0;
    while let Some(peer) = futures.next().await {
        if let Some(peer) = peer {
            result.events.extend(peer.events);
        } else {
            missing += 1;
        }
    }
    if missing > 0 || omitted {
        result.state = "partial".into();
        result.components.push(Component {
            name: "workers".into(),
            state: "partial".into(),
            reason: Some(
                if omitted {
                    "node_limit"
                } else {
                    "peer_unavailable"
                }
                .into(),
            ),
        });
    } else {
        result.components.push(Component {
            name: "workers".into(),
            state: if remote.is_empty() {
                "unavailable"
            } else {
                "complete"
            }
            .into(),
            reason: remote.is_empty().then(|| "not_requested".into()),
        });
    }
    if include_system {
        // Deliberately typed, bounded system facts. General/cluster log text
        // stays behind the existing independent administrator export API.
        let mut facts = BTreeMap::new();
        let (slots, maximum) = state.transcode.hardware_slots().await;
        facts.insert(
            "active_producers".into(),
            (state.transcode.active_sessions().await as u64)
                .min(604_800_000)
                .into(),
        );
        facts.insert("hw_slots".into(), (slots as u64).min(604_800_000).into());
        facts.insert("hw_max".into(), (maximum as u64).min(604_800_000).into());
        facts.insert("app_version".into(), crate::version::SEMVER.into());
        facts.insert("build".into(), crate::version::BUILD.into());
        facts.insert("phase".into(), "readiness".into());
        facts.insert("observed".into(), state.serving.is_ready().into());
        result.events.push(Event {
            run_id: recorder.run.clone(),
            sequence: recorder
                .sequence
                .fetch_add(1, Ordering::Relaxed)
                .to_string(),
            utc: date(unix_ms()),
            elapsed_ms: recorder.started.elapsed().as_millis().min(604_800_000) as u32,
            domain: "coverage".into(),
            kind: "availability".into(),
            facts,
        });
    }
    while serde_json::to_vec(&result)
        .map(|v| v.len() > JOB_BYTES)
        .unwrap_or(true)
    {
        let dropped = if let Some(index) = result.events.iter().position(|e| e.kind == "sample") {
            Some(result.events.remove(index))
        } else if result.events.is_empty() {
            None
        } else {
            Some(result.events.remove(0))
        };
        if dropped.is_none() {
            return unavailable("overflow");
        }
        result.state = "partial".into();
        if !result
            .components
            .iter()
            .any(|c| c.reason.as_deref() == Some("overflow"))
        {
            if let Some(component) = result.components.iter_mut().find(|c| c.name == "server") {
                component.state = "partial".into();
                component.reason = Some("overflow".into());
            }
        }
    }
    result
}
fn owned_job(recorder: &Recorder, user: &User, id: &str) -> Result<Arc<Job>, StatusCode> {
    let mut jobs = recorder
        .jobs
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Recorder::cleanup(&mut jobs);
    let job = jobs.entries.get(id).ok_or(StatusCode::NOT_FOUND)?;
    if job.user != user.id || job.generation != generation(user) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Arc::clone(job))
}
async fn read(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let job = owned_job(&recorder, &user, &id)?;
    let slot = job
        .result
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(private(match slot.as_ref() {
        Some(value) => {
            Json(serde_json::to_value(value).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?)
        }
        None => Json(serde_json::json!({"version":VERSION,"state":"pending"})),
    }))
}
async fn erase(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let job = owned_job(&recorder, &user, &id)?;
    job.cancel.cancel();
    recorder
        .jobs
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .entries
        .remove(&id);
    Ok(private(StatusCode::NO_CONTENT))
}
async fn peer(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    crate::http::internal_media::authorize(&state, &headers, "POST", PEER_PATH, &body).await?;
    let scope: PeerScope =
        serde_json::from_value(strict_json(&body, 16 * 1024).map_err(|_| StatusCode::BAD_REQUEST)?)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    if scope.user_id <= 0
        || scope.until_ms < scope.since_ms
        || scope.until_ms - scope.since_ms > 600_000
        || scope.until_ms > unix_ms() + 120_000
        || scope.since_ms < unix_ms() - 900_000
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let user = state
        .store
        .get_user(scope.user_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if generation(&user) != scope.generation {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let recorder = crate::telemetry::diagnostics(Arc::clone(&state.store));
    let projection = recorder.local(&scope)?;
    let bytes = serde_json::to_vec(&projection).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if bytes.len() > 192 * 1024 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let auth = crate::http::peer_transport::exact_auth_from_headers(&headers)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = crate::http::peer_transport::signed_response_payload(200, &bytes);
    let signature = state
        .membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, PEER_PATH, &payload)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut response = private(bytes);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        crate::http::peer_transport::RESPONSE_SIGNATURE_HEADER,
        axum::http::HeaderValue::from_str(&signature)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positively_attributed_projection_never_releases_another_users_events_or_arbitrary_text() {
        let recorder = Recorder::default();
        for user in [1, 2] {
            let raw = PlaybackEvent {
                id: 0,
                at_unix_ms: unix_ms(),
                user_id: Some(user),
                session_id: None,
                file_id: None,
                event: "stall".into(),
                level: None,
                method: Some("transcode".into()),
                encoder: None,
                height: None,
                ms: Some(user),
                runway_ds: None,
                bandwidth_kbps: None,
                speed_recent: None,
                ahead_seconds: None,
                suspended: None,
                hold_reason: None,
                delivered_bps: None,
                readrate: None,
                detail: Some("CANARY_SECRET_/home/private?token=x".into()),
                attempt: None,
                reason: None,
                ua: None,
                extra: None,
            };
            recorder.observe(&raw);
        }
        let scope = PeerScope {
            user_id: 1,
            generation: "internal".into(),
            since_ms: unix_ms() - 1000,
            until_ms: unix_ms() + 1000,
            correlation: None,
        };
        let result = recorder.local(&scope).expect("own projection");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].facts["duration_ms"], 1);
        assert!(!serde_json::to_string(&result)
            .expect("JSON")
            .contains("CANARY"));
        let mut forged = scope;
        forged.correlation = Some(format!("s-{}", "0".repeat(64)));
        assert!(recorder.local(&forged).is_err());
    }
}

#[cfg(test)]
#[path = "diagnostics_snapshot_tests.rs"]
mod snapshot_lifecycle_tests;
