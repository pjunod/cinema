//! Retained P7 failure evidence and the executable vendor-neutral proxy fixture.
//!
//! The fixture models application contracts, not deployment-product syntax:
//! readiness is `/readyz`, HLS paths receive backend affinity, connection and
//! drain bounds are explicit, and unsafe HTTP methods are sent at most once.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Method, Response, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use plurx_compat_plex::validation_axum as axum;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::topology::{resolve_build_sha, unix_ms};

pub const FAILURE_DRILL_ARTIFACT_SCHEMA_VERSION: u32 = 1;
pub const FAILURE_DRILL_WRITE_OPERATIONS: u64 = 64;
pub const LEADER_ELECTION_BUDGET_MILLIS: u64 = 10_000;
pub const PROXY_CONNECT_TIMEOUT_MILLIS: u64 = 2_000;
pub const PROXY_DRAIN_TIMEOUT_MILLIS: u64 = 75_000;
const EVIDENCE_SCOPE: &str = "semantic_ci";
const STICKY_COOKIE: &str = "plurx_backend";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterFailureDrillArtifact {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub build_sha: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub workload: FailureDrillWorkload,
    pub three_voter_baseline: ThreeVoterObservation,
    pub three_voter_plus_learner: LearnerDrillObservation,
    pub follower_loss: FailureDrillObservation,
    pub leader_loss: FailureDrillObservation,
    pub hls_backend_loss: ProxyDrillObservation,
    pub accepted_budgets: AcceptedBudgets,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDrillWorkload {
    pub id: String,
    pub write_operation: String,
    pub write_operations: u64,
    pub concurrency: u64,
    pub failure_point: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreeVoterObservation {
    pub voting_nodes: u64,
    pub voting_quorum: u64,
    pub voting_failure_tolerance: u64,
    pub write_operations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearnerDrillObservation {
    pub voting_nodes: u64,
    pub voting_quorum: u64,
    pub non_voting_replicas: u64,
    pub lagged_learner_left_rotation: bool,
    pub learner_reentered_rotation_after_catchup: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDrillObservation {
    pub target: String,
    pub initial_leader: u64,
    pub failed_node: u64,
    pub replacement_leader: u64,
    pub write_operations: u64,
    pub request_attempts: u64,
    pub request_errors: u64,
    pub raw_write_latency_millis: Vec<u64>,
    pub recovery_millis: u64,
    pub writes_preserved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyDrillObservation {
    pub readiness_path: String,
    pub hls_affinity: bool,
    pub segment_affinity: bool,
    pub hls_session_survived_backend_loss: bool,
    pub discontinuity_tags_after_failover: u64,
    pub unsafe_mutation_attempts: u64,
    pub unsafe_mutation_retries: u64,
    pub connect_timeout_millis: u64,
    pub drain_timeout_millis: u64,
    pub slow_response_millis: u64,
    pub graceful_drain_completed: bool,
    pub graceful_drain_millis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedBudgets {
    pub leader_election_recovery_millis: u64,
    pub proxy_connect_timeout_millis: u64,
    pub proxy_drain_timeout_millis: u64,
    pub unsafe_mutation_retries: u64,
}

pub(crate) fn write_semantic_artifact(
    output: &Path,
    started_at_unix_ms: i64,
    learner: LearnerDrillObservation,
    follower_loss: FailureDrillObservation,
    leader_loss: FailureDrillObservation,
    hls_backend_loss: ProxyDrillObservation,
) -> Result<()> {
    let artifact = ClusterFailureDrillArtifact {
        schema_version: FAILURE_DRILL_ARTIFACT_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE.to_owned(),
        build_sha: resolve_build_sha()?,
        started_at_unix_ms,
        finished_at_unix_ms: unix_ms()?,
        workload: FailureDrillWorkload {
            id: "split-loss-quorum-write-v1".to_owned(),
            write_operation: "quorum_acknowledged_put_setting".to_owned(),
            write_operations: FAILURE_DRILL_WRITE_OPERATIONS,
            concurrency: 1,
            failure_point: "after_32_acknowledged_writes".to_owned(),
        },
        three_voter_baseline: ThreeVoterObservation {
            voting_nodes: 3,
            voting_quorum: 2,
            voting_failure_tolerance: 1,
            write_operations: FAILURE_DRILL_WRITE_OPERATIONS,
        },
        three_voter_plus_learner: learner,
        follower_loss,
        leader_loss,
        hls_backend_loss,
        accepted_budgets: AcceptedBudgets {
            leader_election_recovery_millis: LEADER_ELECTION_BUDGET_MILLIS,
            proxy_connect_timeout_millis: PROXY_CONNECT_TIMEOUT_MILLIS,
            proxy_drain_timeout_millis: PROXY_DRAIN_TIMEOUT_MILLIS,
            unsafe_mutation_retries: 0,
        },
    };
    validate_failure_drill_artifact(&artifact)?;
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create failure-drill artifact directory {parent:?}"))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&artifact)?;
    bytes.push(b'\n');
    std::fs::write(output, bytes)
        .with_context(|| format!("write failure-drill artifact {output:?}"))?;
    println!("cluster-check: failure-drill artifact {}", output.display());
    Ok(())
}

pub fn validate_failure_drill_artifact(artifact: &ClusterFailureDrillArtifact) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-failure-drills.schema.json"
    ))
    .context("parse checked-in failure-drill schema")?;
    let validator =
        jsonschema::draft202012::new(&schema).context("compile failure-drill schema")?;
    let value = serde_json::to_value(artifact)?;
    if !validator.is_valid(&value) {
        bail!("failure-drill artifact does not satisfy its closed schema");
    }
    if artifact.schema_version != FAILURE_DRILL_ARTIFACT_SCHEMA_VERSION
        || artifact.evidence_scope != EVIDENCE_SCOPE
    {
        bail!("failure-drill artifact identity is not semantic schema v1");
    }
    if artifact.build_sha.len() != 40
        || !artifact
            .build_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("failure-drill build SHA must be a full lowercase Git commit");
    }
    if artifact.started_at_unix_ms > artifact.finished_at_unix_ms {
        bail!("failure-drill artifact finishes before it starts");
    }
    if artifact.workload.write_operations != FAILURE_DRILL_WRITE_OPERATIONS
        || artifact.workload.concurrency != 1
        || artifact.three_voter_baseline
            != (ThreeVoterObservation {
                voting_nodes: 3,
                voting_quorum: 2,
                voting_failure_tolerance: 1,
                write_operations: FAILURE_DRILL_WRITE_OPERATIONS,
            })
    {
        bail!("failure-drill artifact changed the fixed three-voter workload");
    }
    let learner = &artifact.three_voter_plus_learner;
    if learner.voting_nodes != 3
        || learner.voting_quorum != 2
        || learner.non_voting_replicas != 1
        || !learner.lagged_learner_left_rotation
        || !learner.learner_reentered_rotation_after_catchup
    {
        bail!("learner drill did not prove lag removal and catch-up re-entry");
    }
    validate_loss(&artifact.follower_loss, "follower")?;
    validate_loss(&artifact.leader_loss, "leader")?;
    if artifact.leader_loss.initial_leader == artifact.leader_loss.replacement_leader
        || artifact.leader_loss.recovery_millis
            > artifact.accepted_budgets.leader_election_recovery_millis
    {
        bail!("leader loss exceeded the accepted election recovery budget");
    }
    let proxy = &artifact.hls_backend_loss;
    if proxy.readiness_path != "/readyz"
        || !proxy.hls_affinity
        || !proxy.segment_affinity
        || !proxy.hls_session_survived_backend_loss
        || proxy.discontinuity_tags_after_failover != 1
        || proxy.unsafe_mutation_attempts != 1
        || proxy.unsafe_mutation_retries != 0
        || proxy.connect_timeout_millis != artifact.accepted_budgets.proxy_connect_timeout_millis
        || proxy.drain_timeout_millis != artifact.accepted_budgets.proxy_drain_timeout_millis
        || proxy.slow_response_millis <= PROXY_CONNECT_TIMEOUT_MILLIS
        || !proxy.graceful_drain_completed
        || proxy.graceful_drain_millis > proxy.drain_timeout_millis
        || artifact.accepted_budgets.unsafe_mutation_retries != 0
    {
        bail!("proxy drill did not preserve the readiness, affinity, and retry contract");
    }
    Ok(())
}

fn validate_loss(observation: &FailureDrillObservation, expected: &str) -> Result<()> {
    if observation.target != expected
        || observation.write_operations != FAILURE_DRILL_WRITE_OPERATIONS
        || observation.request_attempts != FAILURE_DRILL_WRITE_OPERATIONS
        || observation.request_errors != 0
        || observation.raw_write_latency_millis.len() != FAILURE_DRILL_WRITE_OPERATIONS as usize
        || !observation.writes_preserved
        || (expected == "follower" && observation.failed_node == observation.initial_leader)
        || (expected == "follower" && observation.replacement_leader != observation.initial_leader)
        || (expected == "leader" && observation.failed_node != observation.initial_leader)
    {
        bail!("{expected}-loss observation did not preserve the fixed write contract");
    }
    Ok(())
}

#[derive(Clone)]
struct BackendState {
    name: &'static str,
    playlist: &'static str,
    ready: Arc<AtomicBool>,
    fail_mutations: Arc<AtomicBool>,
    mutations: Arc<AtomicU64>,
    slow_started: Arc<Notify>,
}

struct TestServer {
    base_url: String,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn stop(self, maximum_drain: Duration) -> Result<(bool, u64)> {
        let started = std::time::Instant::now();
        self.shutdown.cancel();
        let mut task = self.task;
        let completed = match tokio::time::timeout(maximum_drain, &mut task).await {
            Ok(result) => {
                result.context("join local proxy fixture server")?;
                true
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                false
            }
        };
        Ok((completed, u64::try_from(started.elapsed().as_millis())?))
    }
}

async fn start_backend(state: BackendState) -> Result<TestServer> {
    let app = Router::new()
        .route("/readyz", get(backend_ready))
        .route("/hls/session/master.m3u8", get(backend_playlist))
        .route("/hls/session/segment-1.ts", get(backend_segment))
        .route("/hls/session/slow-segment.ts", get(backend_slow_segment))
        .route("/api/mutate", post(backend_mutation))
        .with_state(state);
    start_server(app).await
}

async fn backend_ready(State(state): State<BackendState>) -> StatusCode {
    if state.ready.load(Ordering::SeqCst) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn backend_playlist(State(state): State<BackendState>) -> Response<Body> {
    backend_response(&state, "application/vnd.apple.mpegurl", state.playlist)
}

async fn backend_segment(State(state): State<BackendState>) -> Response<Body> {
    backend_response(&state, "video/mp2t", "synthetic-segment")
}

async fn backend_slow_segment(State(state): State<BackendState>) -> Response<Body> {
    state.slow_started.notify_one();
    tokio::time::sleep(Duration::from_millis(PROXY_CONNECT_TIMEOUT_MILLIS + 250)).await;
    backend_response(&state, "video/mp2t", "synthetic-slow-segment")
}

async fn backend_mutation(State(state): State<BackendState>) -> Response<Body> {
    state.mutations.fetch_add(1, Ordering::SeqCst);
    if state.fail_mutations.load(Ordering::SeqCst) {
        Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header("x-backend", state.name)
            .body(Body::from("synthetic mutation failure"))
            .expect("static backend response")
    } else {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("x-backend", state.name)
            .body(Body::empty())
            .expect("static backend response")
    }
}

fn backend_response(
    state: &BackendState,
    content_type: &str,
    body: &'static str,
) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .header("x-backend", state.name)
        .body(Body::from(body))
        .expect("static backend response")
}

#[derive(Clone)]
struct ProxyState {
    backends: Arc<Vec<String>>,
    client: reqwest::Client,
}

async fn start_proxy(backends: Vec<String>) -> Result<TestServer> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(PROXY_CONNECT_TIMEOUT_MILLIS))
        .build()
        .context("build proxy fixture client")?;
    let app = Router::new()
        .fallback(proxy_request)
        .with_state(ProxyState {
            backends: Arc::new(backends),
            client,
        });
    start_server(app).await
}

async fn start_server(app: Router) -> Result<TestServer> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind local proxy fixture")?;
    let address = listener
        .local_addr()
        .context("read proxy fixture address")?;
    let shutdown = CancellationToken::new();
    let cancelled = shutdown.clone();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(cancelled.cancelled_owned())
            .await;
    });
    Ok(TestServer {
        base_url: format!("http://{address}"),
        shutdown,
        task,
    })
}

async fn proxy_request(State(state): State<ProxyState>, request: Request) -> Response<Body> {
    match proxy_request_inner(state, request).await {
        Ok(response) => response,
        Err(error) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(error.to_string()))
            .expect("static proxy error response"),
    }
}

async fn proxy_request_inner(state: ProxyState, request: Request) -> Result<Response<Body>> {
    let method = request.method().clone();
    let path_and_query = request
        .uri()
        .path_and_query()
        .map_or_else(|| request.uri().path().to_owned(), ToString::to_string);
    let hls = request.uri().path().starts_with("/hls/");
    let sticky = request
        .headers()
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(sticky_backend);
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .context("read proxy fixture request body")?;

    let mut candidates = Vec::with_capacity(state.backends.len());
    if hls {
        if let Some(index) = sticky.filter(|index| *index < state.backends.len()) {
            candidates.push(index);
        }
    }
    for index in 0..state.backends.len() {
        if !candidates.contains(&index) {
            candidates.push(index);
        }
    }

    let safe = matches!(method, Method::GET | Method::HEAD);
    let mut sent = 0_u64;
    for index in candidates {
        let backend = &state.backends[index];
        if !probe_backend_ready(&state.client, backend).await {
            continue;
        }
        if sent > 0 && !safe {
            break;
        }
        sent += 1;
        let response = state
            .client
            .request(method.clone(), format!("{backend}{path_and_query}"))
            .body(body.clone())
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) if safe => {
                if sent == state.backends.len() as u64 {
                    return Err(error).context("all ready proxy backends failed");
                }
                continue;
            }
            Err(error) => return Err(error).context("unsafe proxy request failed without retry"),
        };
        let status = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let backend_header = response.headers().get("x-backend").cloned();
        let bytes = response
            .bytes()
            .await
            .context("read proxy backend response")?;
        let mut result = Response::builder().status(status);
        if let Some(content_type) = content_type {
            result = result.header(CONTENT_TYPE, content_type);
        }
        if let Some(backend_header) = backend_header {
            result = result.header("x-backend", backend_header);
        }
        if hls {
            result = result.header(
                SET_COOKIE,
                format!("{STICKY_COOKIE}={index}; Path=/hls; HttpOnly; SameSite=Lax"),
            );
        }
        return result
            .body(Body::from(bytes))
            .context("build proxy fixture response");
    }
    bail!("no ready backend")
}

fn sticky_backend(cookie: &str) -> Option<usize> {
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == STICKY_COOKIE)
            .then(|| value.parse().ok())
            .flatten()
    })
}

async fn probe_backend_ready(client: &reqwest::Client, backend: &str) -> bool {
    client
        .get(format!("{backend}/readyz"))
        .send()
        .await
        .is_ok_and(|response| response.status() == StatusCode::OK)
}

pub(crate) async fn run_proxy_fixture_command() -> Result<()> {
    let observation = run_proxy_fixture().await?;
    println!("{}", serde_json::to_string_pretty(&observation)?);
    Ok(())
}

pub(crate) async fn run_proxy_fixture() -> Result<ProxyDrillObservation> {
    let backend_a_ready = Arc::new(AtomicBool::new(true));
    let backend_a_fail_mutations = Arc::new(AtomicBool::new(false));
    let backend_a_mutations = Arc::new(AtomicU64::new(0));
    let backend_b_mutations = Arc::new(AtomicU64::new(0));
    let backend_a_slow_started = Arc::new(Notify::new());
    let backend_a = start_backend(BackendState {
        name: "a",
        playlist: "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:40\n#EXTINF:4.0,\nsegment-1.ts\n",
        ready: Arc::clone(&backend_a_ready),
        fail_mutations: Arc::clone(&backend_a_fail_mutations),
        mutations: Arc::clone(&backend_a_mutations),
        slow_started: Arc::clone(&backend_a_slow_started),
    })
    .await?;
    let backend_b = start_backend(BackendState {
        name: "b",
        playlist: "#EXTM3U\n#EXT-X-DISCONTINUITY-SEQUENCE:1\n#EXT-X-MEDIA-SEQUENCE:41\n#EXT-X-DISCONTINUITY\n#EXTINF:4.0,\nsegment-1.ts\n",
        ready: Arc::new(AtomicBool::new(true)),
        fail_mutations: Arc::new(AtomicBool::new(false)),
        mutations: Arc::clone(&backend_b_mutations),
        slow_started: Arc::new(Notify::new()),
    })
    .await?;
    let proxy = start_proxy(vec![backend_a.base_url.clone(), backend_b.base_url.clone()]).await?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(PROXY_CONNECT_TIMEOUT_MILLIS))
        .build()
        .context("build proxy fixture driver")?;

    let first = client
        .get(format!("{}/hls/session/master.m3u8", proxy.base_url))
        .send()
        .await
        .context("request initial sticky playlist")?;
    let hls_affinity = first
        .headers()
        .get("x-backend")
        .and_then(|v| v.to_str().ok())
        == Some("a");
    let cookie = first
        .headers()
        .get(SET_COOKIE)
        .cloned()
        .context("initial HLS response omitted affinity cookie")?;
    let cookie = cookie
        .to_str()
        .context("affinity cookie was not text")?
        .split(';')
        .next()
        .context("affinity cookie was empty")?
        .to_owned();
    let segment = client
        .get(format!("{}/hls/session/segment-1.ts", proxy.base_url))
        .header(COOKIE, cookie.clone())
        .send()
        .await
        .context("request sticky segment")?;
    let segment_affinity = segment
        .headers()
        .get("x-backend")
        .and_then(|v| v.to_str().ok())
        == Some("a");

    // A failing unsafe request is sent once even though B is healthy. Its
    // status is returned as-is; the proxy never infers that the mutation was
    // uncommitted and never replays it.
    backend_a_fail_mutations.store(true, Ordering::SeqCst);
    let mutation = client
        .post(format!("{}/api/mutate", proxy.base_url))
        .body("one-shot")
        .send()
        .await
        .context("request failing unsafe mutation")?;
    if mutation.status() != StatusCode::SERVICE_UNAVAILABLE {
        bail!("proxy did not preserve the unsafe mutation failure status");
    }
    let unsafe_mutation_attempts =
        backend_a_mutations.load(Ordering::SeqCst) + backend_b_mutations.load(Ordering::SeqCst);
    let unsafe_mutation_retries = backend_b_mutations.load(Ordering::SeqCst);

    // A response may take longer than the connect budget once the connection
    // exists. Start one, begin a real graceful backend shutdown while it is in
    // flight, and require it to finish inside the 75-second drain ceiling.
    // The stopped listener then makes the sticky takeover exercise an actual
    // connection failure instead of only a synthetic unready response.
    let slow_client = client.clone();
    let slow_url = format!("{}/hls/session/slow-segment.ts", proxy.base_url);
    let slow_cookie = cookie.clone();
    let slow_request = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let response = slow_client
            .get(slow_url)
            .header(COOKIE, slow_cookie)
            .send()
            .await
            .context("request slow sticky segment")?;
        Ok::<_, anyhow::Error>((response, u64::try_from(started.elapsed().as_millis())?))
    });
    tokio::time::timeout(Duration::from_secs(3), backend_a_slow_started.notified())
        .await
        .context("slow backend response never started")?;
    let (graceful_drain_completed, graceful_drain_millis) = backend_a
        .stop(Duration::from_millis(PROXY_DRAIN_TIMEOUT_MILLIS))
        .await?;
    let (slow_response, slow_response_millis) =
        slow_request.await.context("join slow proxy request")??;
    if slow_response.status() != StatusCode::OK
        || slow_response
            .headers()
            .get("x-backend")
            .and_then(|value| value.to_str().ok())
            != Some("a")
        || slow_response_millis <= PROXY_CONNECT_TIMEOUT_MILLIS
    {
        bail!("a response longer than the connect budget did not drain from backend A");
    }

    let takeover = client
        .get(format!("{}/hls/session/master.m3u8", proxy.base_url))
        .header(COOKIE, cookie)
        .send()
        .await
        .context("request playlist after backend loss")?;
    let takeover_backend = takeover
        .headers()
        .get("x-backend")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let takeover_status = takeover.status();
    let takeover_body = takeover.text().await.context("read takeover playlist")?;
    let discontinuity_tags_after_failover = takeover_body
        .lines()
        .filter(|line| *line == "#EXT-X-DISCONTINUITY")
        .count() as u64;
    let hls_session_survived_backend_loss = takeover_status == StatusCode::OK
        && takeover_backend.as_deref() == Some("b")
        && discontinuity_tags_after_failover == 1;

    let (proxy_stopped, _) = proxy
        .stop(Duration::from_millis(PROXY_DRAIN_TIMEOUT_MILLIS))
        .await?;
    let (backend_b_stopped, _) = backend_b
        .stop(Duration::from_millis(PROXY_DRAIN_TIMEOUT_MILLIS))
        .await?;
    if !proxy_stopped || !backend_b_stopped {
        bail!("idle proxy fixture servers exceeded the drain bound");
    }

    let observation = ProxyDrillObservation {
        readiness_path: "/readyz".to_owned(),
        hls_affinity,
        segment_affinity,
        hls_session_survived_backend_loss,
        discontinuity_tags_after_failover,
        unsafe_mutation_attempts,
        unsafe_mutation_retries,
        connect_timeout_millis: PROXY_CONNECT_TIMEOUT_MILLIS,
        drain_timeout_millis: PROXY_DRAIN_TIMEOUT_MILLIS,
        slow_response_millis,
        graceful_drain_completed,
        graceful_drain_millis,
    };
    if !observation.hls_affinity
        || !observation.segment_affinity
        || !observation.hls_session_survived_backend_loss
        || observation.unsafe_mutation_attempts != 1
        || observation.unsafe_mutation_retries != 0
        || !observation.graceful_drain_completed
        || observation.graceful_drain_millis > PROXY_DRAIN_TIMEOUT_MILLIS
    {
        bail!("proxy fixture violated its application contract: {observation:?}");
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ClusterFailureDrillArtifact {
        ClusterFailureDrillArtifact {
            schema_version: 1,
            evidence_scope: EVIDENCE_SCOPE.to_owned(),
            build_sha: "a".repeat(40),
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: 2_000,
            workload: FailureDrillWorkload {
                id: "split-loss-quorum-write-v1".to_owned(),
                write_operation: "quorum_acknowledged_put_setting".to_owned(),
                write_operations: 64,
                concurrency: 1,
                failure_point: "after_32_acknowledged_writes".to_owned(),
            },
            three_voter_baseline: ThreeVoterObservation {
                voting_nodes: 3,
                voting_quorum: 2,
                voting_failure_tolerance: 1,
                write_operations: 64,
            },
            three_voter_plus_learner: LearnerDrillObservation {
                voting_nodes: 3,
                voting_quorum: 2,
                non_voting_replicas: 1,
                lagged_learner_left_rotation: true,
                learner_reentered_rotation_after_catchup: true,
            },
            follower_loss: FailureDrillObservation {
                target: "follower".to_owned(),
                initial_leader: 1,
                failed_node: 2,
                replacement_leader: 1,
                write_operations: 64,
                request_attempts: 64,
                request_errors: 0,
                raw_write_latency_millis: vec![1; 64],
                recovery_millis: 100,
                writes_preserved: true,
            },
            leader_loss: FailureDrillObservation {
                target: "leader".to_owned(),
                initial_leader: 1,
                failed_node: 1,
                replacement_leader: 2,
                write_operations: 64,
                request_attempts: 64,
                request_errors: 0,
                raw_write_latency_millis: vec![1; 64],
                recovery_millis: 1_000,
                writes_preserved: true,
            },
            hls_backend_loss: ProxyDrillObservation {
                readiness_path: "/readyz".to_owned(),
                hls_affinity: true,
                segment_affinity: true,
                hls_session_survived_backend_loss: true,
                discontinuity_tags_after_failover: 1,
                unsafe_mutation_attempts: 1,
                unsafe_mutation_retries: 0,
                connect_timeout_millis: 2_000,
                drain_timeout_millis: 75_000,
                slow_response_millis: 2_250,
                graceful_drain_completed: true,
                graceful_drain_millis: 2_250,
            },
            accepted_budgets: AcceptedBudgets {
                leader_election_recovery_millis: 10_000,
                proxy_connect_timeout_millis: 2_000,
                proxy_drain_timeout_millis: 75_000,
                unsafe_mutation_retries: 0,
            },
        }
    }

    #[test]
    fn artifact_validator_rejects_false_failure_claims() {
        validate_failure_drill_artifact(&fixture()).expect("valid fixture");
        let mut retried = fixture();
        retried.hls_backend_loss.unsafe_mutation_retries = 1;
        assert!(validate_failure_drill_artifact(&retried).is_err());
        let mut slow = fixture();
        slow.leader_loss.recovery_millis = 10_001;
        assert!(validate_failure_drill_artifact(&slow).is_err());
        let mut stale = fixture();
        stale.three_voter_plus_learner.lagged_learner_left_rotation = false;
        assert!(validate_failure_drill_artifact(&stale).is_err());
        let mut enlarged_budget = fixture();
        enlarged_budget
            .accepted_budgets
            .leader_election_recovery_millis = 60_000;
        assert!(validate_failure_drill_artifact(&enlarged_budget).is_err());
        let mut wrong_workload = fixture();
        wrong_workload.workload.id = "invented".to_owned();
        assert!(validate_failure_drill_artifact(&wrong_workload).is_err());
        let mut negative_time = fixture();
        negative_time.started_at_unix_ms = -1;
        assert!(validate_failure_drill_artifact(&negative_time).is_err());
        let mut zero_node = fixture();
        zero_node.follower_loss.failed_node = 0;
        assert!(validate_failure_drill_artifact(&zero_node).is_err());
        let mut changed_follower_leader = fixture();
        changed_follower_leader.follower_loss.replacement_leader = 3;
        assert!(validate_failure_drill_artifact(&changed_follower_leader).is_err());
    }

    #[test]
    fn schema_and_serde_are_closed_and_aligned() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-failure-drills.schema.json"
        ))
        .expect("parse failure-drill schema");
        jsonschema::draft202012::meta::validate(&schema)
            .expect("failure-drill schema satisfies Draft 2020-12");
        let validator =
            jsonschema::draft202012::new(&schema).expect("compile failure-drill schema");
        let value = serde_json::to_value(fixture()).expect("serialize fixture");
        assert!(validator.is_valid(&value));
        assert_eq!(
            schema.get("$id").and_then(serde_json::Value::as_str),
            Some("https://plurx.tv/schemas/cluster-failure-drills-v1.json")
        );
        let mut extra = value;
        extra
            .as_object_mut()
            .expect("artifact object")
            .insert("invented".to_owned(), serde_json::Value::Bool(true));
        assert!(!validator.is_valid(&extra));
    }
}
