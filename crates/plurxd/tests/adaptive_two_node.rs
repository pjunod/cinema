//! Two-node adaptive placement (clock split plan §7).
//!
//! One process cannot show that a create admitted on node A is prepared and
//! adopted by node B under a cross-node grant, nor that the grant-time
//! cluster-clock policy sends a create back to A when it cannot be proved.
//! Each node advertises its HTTP base through a forwarding proxy, so the test
//! can stop one direction of clock probes and count the adaptive control
//! requests that actually reached B.
//!
//! The ingress cannot be put in maintenance to withhold its own quote:
//! maintenance refuses the create itself at A's front door. The test instead
//! asks A's own placement diagnostics for a rendition height whose ranking
//! puts B first, which is the same decision the create makes.
//!
//! Skips without `PLURX_FFMPEG`, which names the encoder both daemons and
//! the fixture generator use.

// A test, never a daemon child: the launcher rule in clippy.toml is for
// production code.
#![allow(clippy::disallowed_methods)]
#![cfg(unix)]

use std::collections::HashMap;
use std::fs::{File, Permissions};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use plurx_core::store::SqliteStore;
use serde_json::{json, Value};
use tokio::net::TcpListener as TokioTcpListener;
use tokio_util::sync::CancellationToken;

const CLOCK_PATH: &str = "/_internal/v1/clock";
const PREPARE_PATH: &str = "/internal/cluster/media/adaptive-attempts/prepare";
const ADOPT_PATH: &str = "/internal/cluster/media/adaptive-attempts/adopt";
const CANCEL_PATH: &str = "/internal/cluster/media/adaptive-attempts/cancel";
const HEIGHTS: [i64; 8] = [240, 288, 360, 432, 480, 540, 576, 640];

// ---------------------------------------------------------------------------
// Daemon harness, copied from cluster_activity.rs: integration tests are
// separate crates.
// ---------------------------------------------------------------------------

fn canonical_tempdir() -> tempfile::TempDir {
    let root =
        std::fs::canonicalize(std::env::temp_dir()).expect("canonical system temporary directory");
    tempfile::tempdir_in(root).expect("two-node adaptive root")
}

struct ReservedPort {
    listener: Option<TcpListener>,
    port: u16,
}

impl ReservedPort {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve test port");
        let port = listener.local_addr().expect("reserved test port").port();
        Self {
            listener: Some(listener),
            port,
        }
    }

    fn release(&mut self) {
        drop(self.listener.take());
    }
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    fn spawn(config: &Path, log: PathBuf, hostname: &str) -> Self {
        let output = File::create(&log).expect("daemon log");
        let errors = output.try_clone().expect("clone daemon log");
        let child = Command::new(env!("CARGO_BIN_EXE_plurxd"))
            .args(["--config", config.to_str().expect("config path"), "run"])
            .env("NO_COLOR", "1")
            .env("PLURX_TEST_SCHEDULER_TICK_MS", "250")
            .env("PLURX_NODE_HOSTNAME", hostname)
            .env("PLURX_HWACCEL", "software")
            .env(
                "PLURX_LOG",
                "info,plurxd::http::hls=debug,plurxd::transcode=debug,plurxd::media_sessions=debug",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(errors))
            .spawn()
            .expect("spawn plurxd");
        Self { child, log }
    }

    fn assert_running(&mut self, context: &str) {
        if let Some(status) = self.child.try_wait().expect("daemon status") {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            panic!("{context}: daemon exited {status}: {log}");
        }
    }

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(&self.log)
            .unwrap_or_else(|error| format!("<cannot read {}: {error}>", self.log.display()))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

async fn wait_ready(client: &reqwest::Client, daemon: &mut Daemon, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        daemon.assert_running("waiting for readiness");
        if client
            .get(format!("http://127.0.0.1:{port}/readyz"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon readiness timed out on port {port}; log: {}",
            daemon.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------------------------------------------------------------------------
// The advertised-base proxy: forwards everything, counts adaptive control
// requests by path, and can refuse clock probes in its direction only.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ProxyState {
    target: String,
    block_clock: Arc<AtomicBool>,
    hits: Arc<Mutex<HashMap<String, u64>>>,
    client: reqwest::Client,
}

struct PeerProxy {
    port: u16,
    state: ProxyState,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl PeerProxy {
    async fn start(target_port: u16) -> Self {
        let listener = TokioTcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy loopback");
        let port = listener.local_addr().expect("proxy address").port();
        let state = ProxyState {
            target: format!("http://127.0.0.1:{target_port}"),
            block_clock: Arc::new(AtomicBool::new(false)),
            hits: Arc::new(Mutex::new(HashMap::new())),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .expect("proxy client"),
        };
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let app = Router::new().fallback(forward).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .expect("advertised-base proxy");
        });
        Self {
            port,
            state,
            stop,
            task,
        }
    }

    fn block_clock(&self, blocked: bool) {
        self.state.block_clock.store(blocked, Ordering::SeqCst);
    }

    fn hits(&self, path: &str) -> u64 {
        self.state
            .hits
            .lock()
            .expect("proxy hits")
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for PeerProxy {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

async fn forward(State(state): State<ProxyState>, request: Request<Body>) -> Response {
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    *state
        .hits
        .lock()
        .expect("proxy hits")
        .entry(path.clone())
        .or_default() += 1;
    if path == CLOCK_PATH && state.block_clock.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(body) = to_bytes(body, 64 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    parts.headers.remove("host");
    parts.headers.remove("content-length");
    let Ok(upstream) = state
        .client
        .request(parts.method, format!("{}{}", state.target, parts.uri))
        .headers(parts.headers)
        .body(body)
        .send()
        .await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    headers.remove("content-length");
    headers.remove("transfer-encoding");
    let Ok(bytes) = upstream.bytes().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    (status, headers, bytes).into_response()
}

// ---------------------------------------------------------------------------
// The cluster.
// ---------------------------------------------------------------------------

struct Cluster {
    node_a: Daemon,
    node_b: Daemon,
    a_base: String,
    b_base: String,
    token: String,
    a_id: String,
    b_id: String,
    proxy_a: PeerProxy,
    proxy_b: PeerProxy,
    client: reqwest::Client,
    /// Heights already created: each create gets its own rendition, so no
    /// step depends on how an earlier step's rendition owner ended.
    used_heights: Vec<i64>,
    _root: tempfile::TempDir,
}

fn node_config(
    http: u16,
    data: &Path,
    raft: u16,
    api: u16,
    advertised: u16,
    join_token_file: Option<&Path>,
) -> String {
    let mut config = format!(
        "[server]\nbind = \"127.0.0.1:{http}\"\n\
         [storage]\ndata_dir = \"{}\"\n\
         [cluster]\nraft_bind = \"127.0.0.1:{raft}\"\n\
         api_bind = \"127.0.0.1:{api}\"\n\
         advertise_host = \"127.0.0.1\"\n\
         artwork_url = \"http://127.0.0.1:{advertised}\"\n",
        toml_path(data),
    );
    match join_token_file {
        Some(file) => config.push_str(&format!("join_token_file = \"{}\"\n", toml_path(file))),
        None => config.push_str(&format!("join_url = \"http://127.0.0.1:{http}\"\n")),
    }
    config
}

impl Cluster {
    async fn start(root: tempfile::TempDir) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(45))
            .build()
            .expect("test client");
        let mut reserved: Vec<ReservedPort> = (0..6).map(|_| ReservedPort::new()).collect();
        let [a_http, a_raft, a_api, b_http, b_raft, b_api] =
            std::array::from_fn(|index| reserved[index].port);
        let proxy_a = PeerProxy::start(a_http).await;
        let proxy_b = PeerProxy::start(b_http).await;

        let a_data = root.path().join("node-a");
        std::fs::create_dir_all(&a_data).expect("node A data");
        drop(SqliteStore::open(&a_data.join("plurx.db")).expect("legacy SQLite source"));
        let a_config = root.path().join("node-a.toml");
        std::fs::write(
            &a_config,
            node_config(a_http, &a_data, a_raft, a_api, proxy_a.port, None),
        )
        .expect("node A config");
        for port in &mut reserved[..3] {
            port.release();
        }
        let mut node_a = Daemon::spawn(&a_config, root.path().join("node-a.log"), "adaptive-a");
        wait_ready(&client, &mut node_a, a_http).await;
        let a_base = format!("http://127.0.0.1:{a_http}");
        let setup = client
            .post(format!("{a_base}/api/v1/setup"))
            .json(&json!({"username":"owner","password":"adaptive-two-node-password"}))
            .send()
            .await
            .expect("setup request");
        assert_eq!(setup.status(), StatusCode::OK);
        let token = setup.json::<Value>().await.expect("setup JSON")["token"]
            .as_str()
            .expect("setup token")
            .to_owned();
        let issued = client
            .post(format!("{a_base}/api/v1/cluster/join-tokens"))
            .bearer_auth(&token)
            .json(&json!({"expires_in_seconds":600}))
            .send()
            .await
            .expect("issue join token");
        assert_eq!(issued.status(), StatusCode::OK);
        let join_token = issued.json::<Value>().await.expect("join-token JSON")["token"]
            .as_str()
            .expect("join token")
            .to_owned();

        let b_data = root.path().join("node-b");
        std::fs::create_dir_all(&b_data).expect("node B data");
        let join_file = root.path().join("node-b.join");
        std::fs::write(&join_file, format!("{join_token}\n")).expect("write join token");
        std::fs::set_permissions(&join_file, Permissions::from_mode(0o600))
            .expect("protect join token");
        let b_config = root.path().join("node-b.toml");
        std::fs::write(
            &b_config,
            node_config(
                b_http,
                &b_data,
                b_raft,
                b_api,
                proxy_b.port,
                Some(&join_file),
            ),
        )
        .expect("node B config");
        for port in &mut reserved[3..] {
            port.release();
        }
        let mut node_b = Daemon::spawn(&b_config, root.path().join("node-b.log"), "adaptive-b");
        wait_ready(&client, &mut node_b, b_http).await;
        let b_base = format!("http://127.0.0.1:{b_http}");

        let deadline = Instant::now() + Duration::from_secs(90);
        let roster = loop {
            node_a.assert_running("waiting for two voters");
            if let Ok(response) = client
                .get(format!("{a_base}/api/v1/cluster/nodes"))
                .bearer_auth(&token)
                .send()
                .await
            {
                if let Ok(body) = response.json::<Value>().await {
                    if body["nodes"].as_array().is_some_and(|nodes| {
                        nodes.len() == 2
                            && nodes
                                .iter()
                                .all(|node| node["is_voter"].as_bool() == Some(true))
                    }) {
                        break body;
                    }
                }
            }
            assert!(Instant::now() < deadline, "two-voter join timed out");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let a_id = roster["local_node_id"]
            .as_str()
            .expect("local node id")
            .to_owned();
        let b_id = roster["nodes"]
            .as_array()
            .expect("node rows")
            .iter()
            .filter_map(|node| node["node_id"].as_str())
            .find(|id| *id != a_id)
            .expect("peer node id")
            .to_owned();
        Self {
            node_a,
            node_b,
            a_base,
            b_base,
            token,
            a_id,
            b_id,
            proxy_a,
            proxy_b,
            client,
            used_heights: Vec::new(),
            _root: root,
        }
    }

    fn logs(&self) -> String {
        format!(
            "\nnode A log:\n{}\nnode B log:\n{}",
            self.node_a.diagnostics(),
            self.node_b.diagnostics()
        )
    }

    async fn get_json(&self, base: &str, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{base}{path}"))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {path}: {error}"));
        assert!(
            response.status().is_success(),
            "GET {path}: {}",
            response.status()
        );
        response.json().await.expect("JSON body")
    }

    async fn metrics(&self, base: &str) -> String {
        self.client
            .get(format!("{base}/metrics"))
            .send()
            .await
            .expect("metrics request")
            .text()
            .await
            .expect("metrics body")
    }

    /// The Developer row's own reading of the grant-time policy.
    async fn grant_policy(&self, base: &str) -> String {
        let readiness = self.get_json(base, "/api/v1/developer/readiness").await;
        readiness["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["id"] == "adaptive_remote_placement")
            .expect("adaptive remote placement row")["requirements"]
            .as_array()
            .expect("requirements")
            .iter()
            .find(|row| row["id"] == "remote_grant_policy")
            .expect("grant policy requirement")["status"]
            .as_str()
            .expect("status")
            .to_owned()
    }

    async fn wait_grant_policy(&mut self, base: &str, wanted: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            self.node_a.assert_running("waiting for clock evidence");
            self.node_b.assert_running("waiting for clock evidence");
            if self.grant_policy(base).await == wanted {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{base} grant policy never became {wanted}{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// What A's placement sees right now for one height: its ranked offers
    /// and its cached view of B. Diagnostics for a failed assertion only.
    async fn placement_view(&self, file_id: i64, height: i64) -> String {
        let offers = self
            .client
            .post(format!("{}/api/v1/cluster/media/offers", self.a_base))
            .bearer_auth(&self.token)
            .json(&json!({"file_id": file_id, "target_height": height}))
            .send()
            .await
            .expect("diagnostic offers")
            .text()
            .await
            .unwrap_or_default();
        let directory = self.get_json(&self.a_base, "/api/v1/cluster/media").await;
        let b = directory["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|node| node["node_id"] == self.b_id.as_str()))
            .map(|node| json!({"quote": node["adaptive_worker_quote"], "protocol": node["protocol_version"]}));
        format!("\nA offers at {height}: {offers}\nA view of B: {b:?}\nA directory: remote_placement_ready={}", directory["remote_placement_ready"])
    }

    /// A height whose ranking on A puts B first while A is also eligible,
    /// once A holds B's fresh adaptive quote.
    async fn height_ranking_b_first(&mut self, file_id: i64) -> i64 {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.node_a.assert_running("waiting for B's quote");
            let directory = self.get_json(&self.a_base, "/api/v1/cluster/media").await;
            let quoted = directory["nodes"].as_array().is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node["node_id"] == self.b_id.as_str()
                        && node["adaptive_worker_quote"].is_object()
                })
            });
            if quoted {
                for height in HEIGHTS
                    .into_iter()
                    .filter(|height| !self.used_heights.contains(height))
                {
                    let response = self
                        .client
                        .post(format!("{}/api/v1/cluster/media/offers", self.a_base))
                        .bearer_auth(&self.token)
                        .json(&json!({"file_id": file_id, "target_height": height}))
                        .send()
                        .await
                        .expect("diagnostic offers");
                    if !response.status().is_success() {
                        continue;
                    }
                    let offers = response.json::<Value>().await.expect("offers JSON")["offers"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let eligible = |node: &str| {
                        offers.iter().any(|offer| {
                            offer["node_id"] == node
                                && offer["eligible"] == true
                                && offer["encoder"] == "software"
                        })
                    };
                    if offers
                        .first()
                        .is_some_and(|offer| offer["node_id"] == self.b_id.as_str())
                        && eligible(&self.b_id)
                        && eligible(&self.a_id)
                    {
                        return height;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "no rendition height ranks B first with both nodes eligible: {directory}{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Create on A until A's own counter for `outcome` moves. Between two
    /// idle nodes the ranking is decided by live measurements (recent speed,
    /// storage reads, spare threads), so B is not first on every create even
    /// when the diagnostics just said so. Every refused create is recorded.
    async fn create_until(
        &mut self,
        file_id: i64,
        playback: &str,
        outcome: &str,
        refused: &mut Vec<String>,
    ) -> i64 {
        for attempt in 0..6 {
            let height = self.height_ranking_b_first(file_id).await;
            let before = placements(&self.metrics(&self.a_base).await, outcome);
            self.used_heights.push(height);
            if let Err(error) = self
                .create_and_play(file_id, height, &format!("{playback}-{attempt}"))
                .await
            {
                refused.push(error);
            }
            if placements(&self.metrics(&self.a_base).await, outcome) > before {
                return height;
            }
        }
        panic!(
            "six creates never produced outcome {outcome} (refused: {refused:?}){}",
            self.logs()
        );
    }

    /// Create on A, then read the playlist and one listed segment through A.
    /// A refused create is returned, not panicked on, so every placement
    /// decision is still asserted when publication is what failed.
    async fn create_and_play(
        &mut self,
        file_id: i64,
        height: i64,
        playback: &str,
    ) -> Result<Value, String> {
        let response = self
            .client
            .post(format!(
                "{}/api/v1/files/{file_id}/hls/sessions",
                self.a_base
            ))
            .bearer_auth(&self.token)
            .json(&json!({"playback_id": playback, "height": height}))
            .send()
            .await
            .expect("HLS create on A");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status != StatusCode::OK {
            return Err(format!("create {playback} refused: {status} {body}"));
        }
        let started: Value = serde_json::from_str(&body).expect("start JSON");
        let playlist = started["playlist_url"].as_str().expect("playlist URL");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.node_a.assert_running("reading the adaptive rendition");
            self.node_b.assert_running("reading the adaptive rendition");
            let text = self
                .client
                .get(format!("{}{playlist}", self.a_base))
                .bearer_auth(&self.token)
                .send()
                .await
                .expect("playlist request")
                .text()
                .await
                .unwrap_or_default();
            if let Some(segment) = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
            {
                let base = playlist.rsplit_once('/').map_or("", |(base, _)| base);
                let url = if segment.starts_with('/') {
                    format!("{}{segment}", self.a_base)
                } else {
                    format!("{}{base}/{segment}", self.a_base)
                };
                let bytes = self
                    .client
                    .get(url)
                    .bearer_auth(&self.token)
                    .send()
                    .await
                    .expect("segment request")
                    .bytes()
                    .await
                    .expect("segment body");
                assert!(!bytes.is_empty(), "listed segment {segment} was empty");
                return Ok(started);
            }
            assert!(
                Instant::now() < deadline,
                "playlist never listed a segment: {text}{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn metric(text: &str, series: &str) -> u64 {
    text.lines()
        .find_map(|line| {
            line.strip_prefix(series)?
                .strip_prefix(' ')?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn continuous_refusals(text: &str, seam: &str) -> u64 {
    let prefix = format!("plurx_cluster_clock_continuous_refusals_total{{seam=\"{seam}\",");
    text.lines()
        .filter(|line| line.starts_with(&prefix))
        .filter_map(|line| line.rsplit_once(' ')?.1.trim().parse::<u64>().ok())
        .sum()
}

fn placements(text: &str, outcome: &str) -> u64 {
    metric(
        text,
        &format!("plurx_adaptive_remote_placements_total{{outcome=\"{outcome}\"}}"),
    )
}

fn write_fixture(ffmpeg: &str, path: &Path) {
    let output = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1280x720:rate=24:duration=8",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            "-y",
        ])
        .arg(path)
        .output()
        .expect("run ffmpeg fixture generator");
    assert!(
        output.status.success(),
        "ffmpeg fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn scan_fixture(cluster: &mut Cluster, media: &Path) -> i64 {
    let library = cluster
        .client
        .post(format!("{}/api/v1/libraries", cluster.a_base))
        .bearer_auth(&cluster.token)
        .json(&json!({"name":"Adaptive","kind":"movies","paths":[media.to_string_lossy()]}))
        .send()
        .await
        .expect("create library");
    assert_eq!(library.status(), StatusCode::OK);
    let library_id = library.json::<Value>().await.expect("library JSON")["id"]
        .as_i64()
        .expect("library id");
    let deadline = Instant::now() + Duration::from_secs(90);
    let file_id = loop {
        cluster.node_a.assert_running("waiting for the scan");
        let page = cluster
            .get_json(
                &cluster.a_base,
                &format!("/api/v1/libraries/{library_id}/items"),
            )
            .await;
        if let Some(item_id) = page["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["id"].as_i64())
        {
            let detail = cluster
                .get_json(&cluster.a_base, &format!("/api/v1/items/{item_id}"))
                .await;
            if let Some(file_id) = detail["files"]
                .as_array()
                .and_then(|files| files.first())
                .and_then(|file| file["id"].as_i64())
            {
                let replicated = cluster
                    .get_json(&cluster.b_base, &format!("/api/v1/items/{item_id}"))
                    .await;
                if replicated["files"]
                    .as_array()
                    .is_some_and(|files| files.iter().any(|file| file["id"] == file_id))
                {
                    break file_id;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the fixture never reached both voters{}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    // VOD refuses to start until the fragment index is durable; either
    // voter may hold the cluster-wide indexing lease.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let indexed = [cluster.node_a.diagnostics(), cluster.node_b.diagnostics()]
            .iter()
            .any(|log| {
                log.lines().any(|line| {
                    line.contains("fragment indexing pass finished") && line.contains("built=1")
                })
            });
        if indexed {
            return file_id;
        }
        assert!(
            Instant::now() < deadline,
            "fragment index was not built{}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adaptive_create_places_on_a_peer_and_falls_back_when_the_grant_cannot_be_proved() {
    let Ok(ffmpeg) = std::env::var("PLURX_FFMPEG") else {
        eprintln!("SKIP: PLURX_FFMPEG is unset; this test needs a real encoder");
        return;
    };
    let root = canonical_tempdir();
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("media directory");
    write_fixture(&ffmpeg, &media.join("Adaptive Two Node (2024).mkv"));
    let mut cluster = Cluster::start(root).await;

    let enabled = cluster
        .client
        .put(format!("{}/api/v1/settings", cluster.a_base))
        .bearer_auth(&cluster.token)
        .json(&json!({"cluster_media_pool_enabled": true}))
        .send()
        .await
        .expect("enable cross-node placement");
    assert_eq!(enabled.status(), StatusCode::OK);
    let file_id = scan_fixture(&mut cluster, &media).await;
    // One probe round in each direction (10 s interval).
    let (a_base, b_base) = (cluster.a_base.clone(), cluster.b_base.clone());
    cluster
        .wait_grant_policy(&a_base, "met", Duration::from_secs(60))
        .await;
    cluster
        .wait_grant_policy(&b_base, "met", Duration::from_secs(60))
        .await;

    // 1. Remote placement: A's ranking puts B first, so B receives Register,
    //    Grant and Adopt, and no accepted deadline is refused anywhere.
    let before_a = cluster.metrics(&a_base).await;
    let prepares = cluster.proxy_b.hits(PREPARE_PATH);
    let adopts = cluster.proxy_b.hits(ADOPT_PATH);
    let mut refused = Vec::new();
    cluster
        .create_until(file_id, "adaptive-two-node-remote", "placed", &mut refused)
        .await;
    let after_a = cluster.metrics(&a_base).await;
    let after_b = cluster.metrics(&b_base).await;
    assert_eq!(
        placements(&after_a, "placed"),
        placements(&before_a, "placed") + 1,
        "A placed the create on B{}",
        cluster.logs()
    );
    assert!(
        cluster.proxy_b.hits(PREPARE_PATH) >= prepares + 2,
        "B received Register and Grant"
    );
    assert!(
        cluster.proxy_b.hits(ADOPT_PATH) > adopts,
        "B received Adopt"
    );
    for (node, text) in [("A", &after_a), ("B", &after_b)] {
        assert_eq!(
            continuous_refusals(text, "entry"),
            0,
            "{node} refused an accepted deadline at entry"
        );
        assert_eq!(
            continuous_refusals(text, "grant"),
            0,
            "{node} refused a grant while the clock was bounded"
        );
    }

    // 2. A cannot prove its clock against B: B is passed over BEFORE Claim,
    //    receives no Register, and the create places on A.
    cluster.proxy_b.block_clock(true);
    cluster
        .wait_grant_policy(&a_base, "unmet", Duration::from_secs(60))
        .await;
    let before_a = cluster.metrics(&a_base).await;
    let prepares = cluster.proxy_b.hits(PREPARE_PATH);
    let height = cluster
        .create_until(
            file_id,
            "adaptive-two-node-skip",
            "clock_skipped",
            &mut refused,
        )
        .await;
    let view = cluster.placement_view(file_id, height).await;
    let after_a = cluster.metrics(&a_base).await;
    assert!(
        placements(&after_a, "clock_skipped") > placements(&before_a, "clock_skipped"),
        "A passed B over on clock evidence (refused so far: {refused:?}){view}{}",
        cluster.logs()
    );
    assert_eq!(
        placements(&after_a, "placed"),
        placements(&before_a, "placed")
    );
    assert_eq!(
        cluster.proxy_b.hits(PREPARE_PATH),
        prepares,
        "no Register reached B"
    );
    assert!(continuous_refusals(&after_a, "grant") > continuous_refusals(&before_a, "grant"));
    cluster.proxy_b.block_clock(false);
    cluster
        .wait_grant_policy(&a_base, "met", Duration::from_secs(60))
        .await;

    // 3. B cannot prove its clock against A while A still can: A registers
    //    on B, B refuses the grant with its typed 503, A fences the original
    //    attempt through B and places the create on itself.
    cluster.proxy_a.block_clock(true);
    cluster
        .wait_grant_policy(&b_base, "unmet", Duration::from_secs(60))
        .await;
    assert_eq!(cluster.grant_policy(&a_base).await, "met");
    let before_a = cluster.metrics(&a_base).await;
    let before_b = cluster.metrics(&b_base).await;
    let prepares = cluster.proxy_b.hits(PREPARE_PATH);
    let cancels = cluster.proxy_b.hits(CANCEL_PATH);
    cluster
        .create_until(
            file_id,
            "adaptive-two-node-refused",
            "grant_refused",
            &mut refused,
        )
        .await;
    let after_a = cluster.metrics(&a_base).await;
    let after_b = cluster.metrics(&b_base).await;
    assert_eq!(
        placements(&after_a, "grant_refused"),
        placements(&before_a, "grant_refused") + 1,
        "A retried after B refused the grant{}",
        cluster.logs()
    );
    assert_eq!(
        placements(&after_a, "placed"),
        placements(&before_a, "placed")
    );
    assert!(
        cluster.proxy_b.hits(PREPARE_PATH) >= prepares + 2,
        "B received Register and the refused Grant"
    );
    assert!(
        cluster.proxy_b.hits(CANCEL_PATH) > cancels,
        "A fenced the original attempt through B"
    );
    assert!(
        continuous_refusals(&after_b, "grant") > continuous_refusals(&before_b, "grant"),
        "B counted its grant-time refusal"
    );
    assert_eq!(continuous_refusals(&after_b, "entry"), 0);
    cluster.proxy_a.block_clock(false);
    // Every placement decision above held; each create must also have been
    // published and served.
    assert!(
        refused.is_empty(),
        "placement held but publication refused: {refused:?}{}",
        cluster.logs()
    );
}
