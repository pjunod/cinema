//! Real-daemon proof for clustered activity aggregation.
//!
//! This belongs to `make cluster-check`: the ordinary handler regressions can
//! prove response shaping, but only separate daemons can prove that node A's
//! authenticated peer read observes work owned by node B.

#![cfg(unix)]

use std::fs::{File, Permissions};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::get;
use axum::Router;
use plurx_core::cluster::membership::UNKNOWN_HOSTNAME;
use plurx_core::store::SqliteStore;
use serde_json::{json, Value};
use tokio::net::TcpListener as TokioTcpListener;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

const FORWARD: u8 = 0;
const HTTP_ERROR: u8 = 1;
const HUNG: u8 = 2;
const AUTH_ADMISSION_BURST: u8 = 3;
const KEY_LOOKUP_BURST: u8 = 4;
const ACTIVITY_PATH: &str = "/_internal/v1/activity-snapshot";

fn canonical_tempdir() -> tempfile::TempDir {
    let root =
        std::fs::canonicalize(std::env::temp_dir()).expect("canonical system temporary directory");
    tempfile::tempdir_in(root).expect("cluster activity root")
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

    fn into_listener(mut self) -> TcpListener {
        self.listener.take().expect("reserved listener")
    }
}

fn reserved_ports(count: usize) -> Vec<ReservedPort> {
    (0..count).map(|_| ReservedPort::new()).collect()
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
            // The index-completion wait reads structured fields from this
            // file. ANSI styling splits `built=1` into control-coded pieces.
            .env("NO_COLOR", "1")
            .env("PLURX_TEST_SCHEDULER_TICK_MS", "250")
            // Both daemons share one machine, so without this they would share
            // one machine name — and on a container runner they would have
            // none at all, because `short_hostname` rejects a Docker container
            // id. Naming them apart makes the roster's names a test input
            // rather than a property of whichever box CI landed on.
            .env("PLURX_NODE_HOSTNAME", hostname)
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

#[derive(Clone)]
struct ActivityProxyState {
    mode: Arc<AtomicU8>,
    requests: Arc<AtomicU64>,
    collection: Arc<Mutex<Option<Arc<Barrier>>>>,
    target: String,
    client: reqwest::Client,
}

async fn activity_proxy(
    State(state): State<ActivityProxyState>,
    headers: HeaderMap,
) -> Response<Body> {
    state.requests.fetch_add(1, Ordering::AcqRel);
    let collection = state
        .collection
        .lock()
        .expect("Activity proxy collection lock")
        .clone();
    if let Some(barrier) = collection {
        // Collect the old eight-request burst onto one receiver admission
        // window. Fixed code sends one physical request, which is released by
        // this >=500 ms collection deadline instead.
        let _ = tokio::time::timeout(Duration::from_millis(500), barrier.wait()).await;
    }
    let mode = state.mode.load(Ordering::Acquire);
    match mode {
        HTTP_ERROR => return response(StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
        HUNG => {
            tokio::time::sleep(Duration::from_secs(10)).await;
            return response(StatusCode::SERVICE_UNAVAILABLE, Vec::new());
        }
        _ => {}
    }

    if mode == AUTH_ADMISSION_BURST {
        let responses = futures_util::future::join_all(
            (0..3).map(|_| forward_activity(&state, &headers, None, None)),
        )
        .await;
        let selected = responses
            .iter()
            .find(|(status, _)| *status == StatusCode::UNAUTHORIZED)
            .or_else(|| responses.first())
            .expect("three upstream Activity responses");
        return response(selected.0, selected.1.clone());
    }

    if mode == KEY_LOOKUP_BURST {
        let responses = futures_util::future::join_all((0..5).map(|index| {
            let state = state.clone();
            let headers = headers.clone();
            let node_id = format!("missing-activity-key-{index}");
            async move {
                forward_activity(
                    &state,
                    &headers,
                    Some(node_id.as_str()),
                    Some("00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
                )
                .await
            }
        }))
        .await;
        assert!(responses
            .iter()
            .all(|(status, _)| *status == StatusCode::UNAUTHORIZED));
        return response(StatusCode::UNAUTHORIZED, Vec::new());
    }

    let (status, body) = forward_activity(&state, &headers, None, None).await;
    response(status, body)
}

async fn forward_activity(
    state: &ActivityProxyState,
    headers: &HeaderMap,
    node_override: Option<&str>,
    signature_override: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut request = state
        .client
        .get(format!("{}{}", state.target, ACTIVITY_PATH));
    for name in [
        "x-plurx-cluster-node",
        "x-plurx-cluster-target",
        "x-plurx-cluster-time-ms",
        "x-plurx-cluster-signature",
    ] {
        let override_value = match name {
            "x-plurx-cluster-node" => node_override,
            "x-plurx-cluster-signature" => signature_override,
            _ => None,
        };
        if let Some(value) =
            override_value.or_else(|| headers.get(name).and_then(|value| value.to_str().ok()))
        {
            request = request.header(name, value.to_owned());
        }
    }
    match request.send().await {
        Ok(upstream) => {
            let status = upstream.status();
            let bytes = upstream
                .bytes()
                .await
                .map_or_else(|_| Vec::new(), |body| body.to_vec());
            (status, bytes)
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
    }
}

fn response(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(body))
        .expect("proxy response")
}

struct ActivityProxy {
    mode: Arc<AtomicU8>,
    requests: Arc<AtomicU64>,
    collection: Arc<Mutex<Option<Arc<Barrier>>>>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ActivityProxy {
    async fn start(listener: TcpListener, target_port: u16) -> Self {
        let mode = Arc::new(AtomicU8::new(FORWARD));
        let requests = Arc::new(AtomicU64::new(0));
        let collection = Arc::new(Mutex::new(None));
        let shutdown = CancellationToken::new();
        let state = ActivityProxyState {
            mode: Arc::clone(&mode),
            requests: Arc::clone(&requests),
            collection: Arc::clone(&collection),
            target: format!("http://127.0.0.1:{target_port}"),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("proxy client"),
        };
        let app = Router::new()
            .route(ACTIVITY_PATH, get(activity_proxy))
            .with_state(state);
        listener
            .set_nonblocking(true)
            .expect("activity proxy nonblocking");
        let listener = TokioTcpListener::from_std(listener).expect("activity proxy listener");
        let stopped = shutdown.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await;
        });
        Self {
            mode,
            requests,
            collection,
            shutdown,
            task,
        }
    }

    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::Release);
    }

    fn request_count(&self) -> u64 {
        self.requests.load(Ordering::Acquire)
    }

    fn begin_collection(&self, expected: usize) {
        *self
            .collection
            .lock()
            .expect("Activity proxy collection lock") = Some(Arc::new(Barrier::new(expected)));
    }

    fn end_collection(&self) {
        *self
            .collection
            .lock()
            .expect("Activity proxy collection lock") = None;
    }

    async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
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
        assert!(Instant::now() < deadline, "daemon readiness timed out");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_two_voters(
    client: &reqwest::Client,
    daemon: &mut Daemon,
    base: &str,
    token: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        daemon.assert_running("waiting for two voters");
        if let Ok(response) = client
            .get(format!("{base}/api/v1/cluster/nodes"))
            .bearer_auth(token)
            .send()
            .await
        {
            if let Ok(body) = response.json::<Value>().await {
                if body["nodes"]
                    .as_array()
                    .is_some_and(|nodes| nodes.len() == 2)
                {
                    return body;
                }
            }
        }
        assert!(Instant::now() < deadline, "two-voter join timed out");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_remote_file(
    client: &reqwest::Client,
    daemon: &mut Daemon,
    base: &str,
    token: &str,
    item_id: i64,
    file_id: i64,
) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        daemon.assert_running("waiting for the joined node to apply replicated state");
        if let Ok(response) = client
            .get(format!("{base}/api/v1/items/{item_id}"))
            .bearer_auth(token)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(body) = response.json::<Value>().await {
                    if body["files"]
                        .as_array()
                        .is_some_and(|files| files.iter().any(|file| file["id"] == file_id))
                    {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "joined node did not apply the fixture's replicated file state:\n{}",
            daemon.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_fragment_index(first: &mut Daemon, second: &mut Daemon) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        first.assert_running("waiting for the fragment index on the first voter");
        second.assert_running("waiting for the fragment index on the second voter");
        let first_diagnostics = first.diagnostics();
        let second_diagnostics = second.diagnostics();
        let pass_finished = |diagnostics: &str| {
            diagnostics.lines().any(|line| {
                let line = strip_ansi_csi(line);
                line.contains("fragment indexing pass finished") && line.contains("built=1")
            })
        };
        if pass_finished(&first_diagnostics) || pass_finished(&second_diagnostics) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fragment index was not built:\nfirst voter:\n{first_diagnostics}\n\
             second voter:\n{second_diagnostics}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_file_id(
    client: &reqwest::Client,
    daemon: &mut Daemon,
    base: &str,
    token: &str,
    item_id: i64,
) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running("waiting for the scanned file row");
        let observation = match client
            .get(format!("{base}/api/v1/items/{item_id}"))
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                match response.json::<Value>().await {
                    Ok(detail) => {
                        if let Some(file_id) = detail["files"]
                            .as_array()
                            .and_then(|files| files.first())
                            .and_then(|file| file["id"].as_i64())
                        {
                            return file_id;
                        }
                        format!("status={status}, detail={detail}")
                    }
                    Err(error) => format!("status={status}, invalid JSON: {error}"),
                }
            }
            Err(error) => format!("request failed: {error}"),
        };
        assert!(
            Instant::now() < deadline,
            "scan published item {item_id} without its file row ({observation})\nnode A log:\n{}",
            daemon.diagnostics(),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Remove terminal control sequences before matching structured log fields.
///
/// CI deliberately exports `CARGO_TERM_COLOR=always`, and the daemon's tracing
/// formatter can therefore put SGR escapes around both `built` and `=1` even
/// though stdout is redirected to a file. The message itself remains plain,
/// so only normalize candidate lines instead of copying the whole growing log
/// on every poll.
fn strip_ansi_csi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut plain = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                index += 1;
            }
            index = index.saturating_add(1);
        } else {
            plain.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(plain).expect("removing ASCII control sequences preserves UTF-8")
}

#[test]
fn fragment_index_completion_match_ignores_forced_terminal_color() {
    let line = "fragment indexing pass finished \x1b[3mattempted\x1b[0m\x1b[2m=\x1b[0m1 \
                \x1b[3mbuilt\x1b[0m\x1b[2m=\x1b[0m1";
    assert!(strip_ansi_csi(line).contains("built=1"));
}

fn write_av_fixture(path: &Path) {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=96x64:rate=2:duration=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=30",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            "-y",
            path.to_str().expect("fixture path"),
        ])
        .output()
        .expect("run ffmpeg fixture generator");
    assert!(
        output.status.success(),
        "ffmpeg fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn activity_detail(client: &reqwest::Client, base: &str, token: &str) -> Value {
    let response = client
        .get(format!("{base}/api/v1/activity/detail"))
        .bearer_auth(token)
        .send()
        .await
        .expect("activity detail request");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("activity detail JSON")
}

async fn metric_value(client: &reqwest::Client, base: &str, name: &str) -> u64 {
    let body = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request")
        .text()
        .await
        .expect("metrics text");
    body.lines()
        .find_map(|line| {
            let (metric, value) = line.split_once(' ')?;
            (metric == name).then(|| value.parse::<u64>().expect("integer counter"))
        })
        .unwrap_or_else(|| panic!("{name} was absent from metrics:\n{body}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_a_coalesces_activity_reads_and_reports_peer_failures_truthfully() {
    let root = canonical_tempdir();
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("media directory");
    let fixture = media.join("Cluster Activity Proof.mp4");
    write_av_fixture(&fixture);

    // Keep the whole set reserved at once so it contains no duplicates. The
    // proxy takes ownership of its listener; each daemon's three reservations
    // are released only immediately before that child is spawned.
    let mut ports = reserved_ports(7).into_iter();
    let mut a_http = ports.next().expect("node A HTTP reservation");
    let mut a_raft = ports.next().expect("node A Raft reservation");
    let mut a_api = ports.next().expect("node A API reservation");
    let mut b_http = ports.next().expect("node B HTTP reservation");
    let mut b_raft = ports.next().expect("node B Raft reservation");
    let mut b_api = ports.next().expect("node B API reservation");
    let proxy_port = ports.next().expect("proxy reservation");
    let proxy_port_number = proxy_port.port;
    let proxy = ActivityProxy::start(proxy_port.into_listener(), b_http.port).await;

    let a_data = root.path().join("node-a");
    std::fs::create_dir_all(&a_data).expect("node A data");
    drop(SqliteStore::open(&a_data.join("plurx.db")).expect("legacy SQLite source"));
    let a_config = root.path().join("node-a.toml");
    std::fs::write(
        &a_config,
        format!(
            "[server]\nbind = \"127.0.0.1:{}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             join_url = \"http://127.0.0.1:{}\"\n\
             artwork_url = \"http://127.0.0.1:{}\"\n",
            a_http.port,
            toml_path(&a_data),
            a_raft.port,
            a_api.port,
            a_http.port,
            a_http.port,
        ),
    )
    .expect("node A config");

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(8))
        .build()
        .expect("test client");
    let a_http_port = a_http.port;
    a_http.release();
    a_raft.release();
    a_api.release();
    let mut node_a = Daemon::spawn(&a_config, root.path().join("node-a.log"), "plurx-node-a");
    wait_ready(&client, &mut node_a, a_http_port).await;
    let a_base = format!("http://127.0.0.1:{a_http_port}");
    let setup = client
        .post(format!("{a_base}/api/v1/setup"))
        .json(&json!({"username":"owner","password":"cluster-proof-password"}))
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
        format!(
            "[server]\nbind = \"127.0.0.1:{}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             artwork_url = \"http://127.0.0.1:{}\"\n\
             join_token_file = \"{}\"\n",
            b_http.port,
            toml_path(&b_data),
            b_raft.port,
            b_api.port,
            proxy_port_number,
            toml_path(&join_file),
        ),
    )
    .expect("node B config");
    let b_http_port = b_http.port;
    b_http.release();
    b_raft.release();
    b_api.release();
    let mut node_b = Daemon::spawn(&b_config, root.path().join("node-b.log"), "plurx-node-b");
    wait_ready(&client, &mut node_b, b_http_port).await;
    let roster = wait_for_two_voters(&client, &mut node_a, &a_base, &token).await;
    let local_node = roster["local_node_id"].as_str().expect("local node id");
    let remote_node = roster["nodes"]
        .as_array()
        .expect("node rows")
        .iter()
        .find_map(|node| {
            let node_id = node["node_id"].as_str()?;
            (node_id != local_node).then_some(node_id.to_owned())
        })
        .expect("remote node id");

    let library = client
        .post(format!("{a_base}/api/v1/libraries"))
        .bearer_auth(&token)
        .json(&json!({
            "name":"Cluster proof",
            "kind":"movies",
            "paths":[media.to_string_lossy()]
        }))
        .send()
        .await
        .expect("create library");
    assert_eq!(library.status(), StatusCode::OK);
    let library_id = library.json::<Value>().await.expect("library JSON")["id"]
        .as_i64()
        .expect("library id");

    let deadline = Instant::now() + Duration::from_secs(60);
    let item_id = loop {
        let page = client
            .get(format!("{a_base}/api/v1/libraries/{library_id}/items"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("library items")
            .json::<Value>()
            .await
            .expect("library items JSON");
        if let Some(item_id) = page["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["id"].as_i64())
        {
            break item_id;
        }
        assert!(
            Instant::now() < deadline,
            "library scan did not publish fixture"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // The item row and its file association are committed separately. Linux
    // runners exposed the short interval where the list endpoint can publish
    // the item before its detail has a file; wait for the actual prerequisite.
    let file_id = wait_for_file_id(&client, &mut node_a, &a_base, &token, item_id).await;
    let b_base = format!("http://127.0.0.1:{b_http_port}");

    // Roster membership means the join was accepted; it does not mean the new
    // voter has already applied the schema and file rows needed by its
    // node-local VOD indexer. Make that prerequisite observable before waiting
    // on the indexer's completed-work signal.
    wait_for_remote_file(&client, &mut node_b, &b_base, &token, item_id, file_id).await;

    // VOD deliberately refuses to start until this prerequisite is durable.
    // The indexing lease is cluster-wide, so either voter may complete it;
    // waiting on node B alone makes scheduler timing decide the test verdict.
    wait_for_fragment_index(&mut node_a, &mut node_b).await;

    let hls = client
        .post(format!("{b_base}/api/v1/files/{file_id}/hls/sessions"))
        .bearer_auth(&token)
        .json(&json!({"playback_id":"cluster-activity-proof","copy":true}))
        .send()
        .await
        .expect("start node B HLS session");
    let hls_status = hls.status();
    let hls_body = hls
        .text()
        .await
        .unwrap_or_else(|error| format!("<cannot read response body: {error}>"));
    assert!(
        hls_status.is_success(),
        "HLS start failed: {hls_status}: {hls_body}\nnode A log:\n{}\nnode B log:\n{}",
        node_a.diagnostics(),
        node_b.diagnostics(),
    );

    let direct = client
        .get(format!(
            "{b_base}/api/v1/files/{file_id}/direct?token={token}"
        ))
        .header("range", "bytes=0-3")
        .send()
        .await
        .expect("start node B direct play");
    assert_eq!(direct.status(), StatusCode::PARTIAL_CONTENT);

    let deadline = Instant::now() + Duration::from_secs(10);
    let healthy = loop {
        let detail = activity_detail(&client, &a_base, &token).await;
        let methods = detail["deliveries"]
            .as_array()
            .expect("deliveries")
            .iter()
            .filter(|delivery| delivery["node_id"] == remote_node)
            .filter_map(|delivery| delivery["method"].as_str())
            .collect::<Vec<_>>();
        if methods.contains(&"direct") && methods.contains(&"hls-copy") {
            break detail;
        }
        assert!(
            Instant::now() < deadline,
            "node B deliveries never appeared: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        healthy["sessions"].as_array().is_some(),
        "sessions remains an array"
    );
    assert!(healthy["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .all(|session| session.get("node_id").is_none()));
    assert!(healthy["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .all(|node| node["status"] == "answered"));

    // A node id names no machine. The page is sent the roster's own short
    // hostnames so the Node column can say which box is serving the stream.
    // The two reads are compared rather than each being separately plausible.
    let roster_view = client
        .get(format!("{a_base}/api/v1/cluster/nodes"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("cluster roster request")
        .json::<Value>()
        .await
        .expect("cluster roster JSON");
    let named = healthy["node_hostnames"]
        .as_object()
        .expect("a clustered admin read carries the roster's machine names");
    for node in roster_view["nodes"].as_array().expect("roster nodes") {
        let node_id = node["node_id"].as_str().expect("roster node id");
        let hostname = node["hostname"].as_str().unwrap_or_default();
        // The local node is deliberately named from memory rather than from the
        // replicated row the heartbeat writes, so it is the one node the two
        // reads may disagree about — and only until the first heartbeat lands.
        if node_id == local_node {
            continue;
        }
        assert_eq!(
            named.get(node_id).and_then(Value::as_str),
            Some(hostname),
            "the page names {node_id} differently from the roster"
        );
    }
    // Both daemons were started with a name of their own, so nothing here
    // should have fallen back to the sentinel — and the sentinel is never a
    // published value in any case.
    assert_eq!(
        named.get(local_node).and_then(Value::as_str),
        Some("plurx-node-a")
    );
    assert_eq!(
        named.get(&remote_node).and_then(Value::as_str),
        Some("plurx-node-b")
    );
    assert!(!named.values().any(|hostname| hostname == UNKNOWN_HOSTNAME));
    // The invariant the column actually needs: every row it will draw has a
    // name to draw. A map that named some other node would satisfy the loop
    // above and still leave the operator reading a UUID.
    for delivery in healthy["deliveries"].as_array().expect("deliveries") {
        let node_id = delivery["node_id"].as_str().expect("delivery node id");
        assert!(
            named.contains_key(node_id),
            "a delivery on {node_id} has no machine name to render"
        );
    }

    // `GET /api/v1/cluster/nodes` is admin-only. The activity page is not, so
    // an ungated map here would make this page the one place an ordinary
    // household member can read the fleet's machine names.
    let created = client
        .post(format!("{a_base}/api/v1/users"))
        .bearer_auth(&token)
        .json(&json!({ "username": "household", "password": "longenough" }))
        .send()
        .await
        .expect("create household user");
    assert_eq!(created.status(), StatusCode::OK);
    let household = client
        .post(format!("{a_base}/api/v1/auth/login"))
        .json(&json!({ "username": "household", "password": "longenough" }))
        .send()
        .await
        .expect("household login")
        .json::<Value>()
        .await
        .expect("household login JSON")["token"]
        .as_str()
        .expect("household token")
        .to_owned();
    // Admin and household reads share one peer wave. The roster projection is
    // still made per caller, so sharing peer data cannot leak machine names.
    // Do not enable the eight-request receiver-admission barrier used by the
    // burst proof below: fixed code sends one peer request, so that barrier
    // can only consume 500 ms of the real two-second peer deadline. The
    // physical request count is sufficient to prove this two-caller wave was
    // coalesced without making Store scheduling part of the privacy verdict.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let before_privacy_wave = proxy.request_count();
    let (admin_view, household_view) = tokio::join!(
        activity_detail(&client, &a_base, &token),
        activity_detail(&client, &a_base, &household),
    );
    assert_eq!(
        proxy.request_count() - before_privacy_wave,
        1,
        "admin and household readers did not share one physical peer wave"
    );
    assert!(
        admin_view.get("node_hostnames").is_some(),
        "a clustered admin read always carries its per-request hostname projection"
    );
    // The field is present for every clustered admin read even when the roster
    // named nobody, so its absence here is the gate and not an empty roster.
    assert!(
        household_view.get("node_hostnames").is_none(),
        "a household member was sent the fleet's machine names"
    );
    // …and both callers still see the streams themselves, so the gate narrows
    // one field rather than changing the shared peer snapshot.
    for (reader, detail) in [("admin", &admin_view), ("household", &household_view)] {
        assert!(
            detail["deliveries"]
                .as_array()
                .expect("deliveries")
                .iter()
                .any(|delivery| delivery["node_id"] == remote_node),
            "{reader} lost node B's delivery during the shared privacy wave: {detail}\n\
             node A log:\n{}\nnode B log:\n{}",
            node_a.diagnostics(),
            node_b.diagnostics(),
        );
    }

    // Collect the burst that used to exceed the receiver's two-per-second
    // legacy authority guard. Fixed code performs one authenticated request;
    // every public caller receives that same answered snapshot.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    proxy.begin_collection(8);
    let before_burst = proxy.request_count();
    let auth_refusals_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_auth_admission_refusals_total",
    )
    .await;
    let key_refusals_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_key_lookup_refusals_total",
    )
    .await;
    let burst =
        futures_util::future::join_all((0..8).map(|_| activity_detail(&client, &a_base, &token)))
            .await;
    proxy.end_collection();
    assert_eq!(
        proxy.request_count() - before_burst,
        1,
        "eight public callers created more than one physical peer request"
    );
    for detail in &burst {
        assert!(detail["activity_nodes"]
            .as_array()
            .expect("activity nodes")
            .iter()
            .all(|node| node["status"] == "answered"));
        assert!(detail["deliveries"]
            .as_array()
            .expect("deliveries")
            .iter()
            .any(|delivery| delivery["node_id"] == remote_node));
    }
    assert_eq!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_auth_admission_refusals_total",
        )
        .await,
        auth_refusals_before,
        "the coalesced valid wave must not hit the receiver authority guard"
    );
    assert_eq!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_key_lookup_refusals_total",
        )
        .await,
        key_refusals_before,
        "the coalesced valid wave must not hit the receiver key-lookup guard"
    );

    // Summary and detail are two views of one process-wide peer wave. Expire
    // the preceding burst deliberately: response shaping and the receiver
    // metric reads above are outside the peer cache, so their wall-clock cost
    // must not decide whether this assertion starts from a live snapshot.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    proxy.begin_collection(2);
    let before_views = proxy.request_count();
    let (summary, _) = tokio::join!(
        async {
            client
                .get(format!("{a_base}/api/v1/activity"))
                .bearer_auth(&token)
                .send()
                .await
                .expect("Activity summary request")
        },
        activity_detail(&client, &a_base, &token),
    );
    proxy.end_collection();
    assert_eq!(summary.status(), StatusCode::OK);
    assert_eq!(
        proxy.request_count() - before_views,
        1,
        "summary and detail created more than one physical peer request"
    );

    // A later view beyond the completed-result TTL starts exactly one new
    // fanout. The paused-time unit pins the exact boundary itself.
    let after_views = proxy.request_count();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let _ = activity_detail(&client, &a_base, &token).await;
    assert_eq!(proxy.request_count(), after_views + 1);

    // Reproduce the receiver's actual per-sender refusal independently of the
    // fixed sender gate: the proxy duplicates one valid signed request three
    // times inside one receiver window. The public status and the receiver
    // counter must identify authentication, not transport.
    proxy.set(AUTH_ADMISSION_BURST);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let auth_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_auth_admission_refusals_total",
    )
    .await;
    let key_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_key_lookup_refusals_total",
    )
    .await;
    let authority_refused = activity_detail(&client, &a_base, &token).await;
    assert!(authority_refused["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "refused"));
    assert!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_auth_admission_refusals_total",
        )
        .await
            > auth_before
    );
    assert_eq!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_key_lookup_refusals_total",
        )
        .await,
        key_before
    );

    // Five forged cold-key envelopes consume the receiver's four admitted
    // misses and drive the separate global guard. They cannot reach the
    // verified-sender authority guard.
    proxy.set(KEY_LOOKUP_BURST);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let auth_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_auth_admission_refusals_total",
    )
    .await;
    let key_before = metric_value(
        &client,
        &b_base,
        "plurx_cluster_activity_key_lookup_refusals_total",
    )
    .await;
    let key_refused = activity_detail(&client, &a_base, &token).await;
    assert!(key_refused["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "refused"));
    assert_eq!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_auth_admission_refusals_total",
        )
        .await,
        auth_before
    );
    assert!(
        metric_value(
            &client,
            &b_base,
            "plurx_cluster_activity_key_lookup_refusals_total",
        )
        .await
            > key_before
    );

    proxy.set(HTTP_ERROR);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let http_error = activity_detail(&client, &a_base, &token).await;
    assert!(http_error["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "http_error"));
    assert!(http_error["deliveries"]
        .as_array()
        .expect("deliveries")
        .iter()
        .all(|delivery| delivery["node_id"] != remote_node));

    proxy.set(HUNG);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let started = Instant::now();
    let timed_out = activity_detail(&client, &a_base, &token).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1_500),
        "hung peer returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "hung peer exceeded the common bound: {elapsed:?}"
    );
    assert!(timed_out["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "timed_out"));

    // A stopped listener is the transport-only unreachable case. An HTTP 503
    // above must never be collapsed into this outcome.
    proxy.stop().await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let unreachable = activity_detail(&client, &a_base, &token).await;
    assert!(unreachable["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "unreachable"));

    drop(node_b);
    drop(node_a);
}
