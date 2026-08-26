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
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::get;
use axum::Router;
use plurx_core::store::SqliteStore;
use serde_json::{json, Value};
use tokio::net::TcpListener as TokioTcpListener;
use tokio_util::sync::CancellationToken;

const FORWARD: u8 = 0;
const UNREACHABLE: u8 = 1;
const HUNG: u8 = 2;
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
    fn spawn(config: &Path, log: PathBuf) -> Self {
        let output = File::create(&log).expect("daemon log");
        let errors = output.try_clone().expect("clone daemon log");
        let child = Command::new(env!("CARGO_BIN_EXE_plurxd"))
            .args(["--config", config.to_str().expect("config path"), "run"])
            // The index-completion wait reads structured fields from this
            // file. ANSI styling splits `built=1` into control-coded pieces.
            .env("NO_COLOR", "1")
            .env("PLURX_TEST_SCHEDULER_TICK_MS", "250")
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
    target: String,
    client: reqwest::Client,
}

async fn activity_proxy(
    State(state): State<ActivityProxyState>,
    headers: HeaderMap,
) -> Response<Body> {
    match state.mode.load(Ordering::Acquire) {
        UNREACHABLE => return response(StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
        HUNG => {
            tokio::time::sleep(Duration::from_secs(10)).await;
            return response(StatusCode::SERVICE_UNAVAILABLE, Vec::new());
        }
        _ => {}
    }

    let mut request = state
        .client
        .get(format!("{}{}", state.target, ACTIVITY_PATH));
    for name in [
        "x-plurx-cluster-node",
        "x-plurx-cluster-target",
        "x-plurx-cluster-time-ms",
        "x-plurx-cluster-signature",
    ] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            request = request.header(name, value);
        }
    }
    match request.send().await {
        Ok(upstream) => {
            let status = upstream.status();
            let bytes = upstream
                .bytes()
                .await
                .map_or_else(|_| Vec::new(), |body| body.to_vec());
            response(status, bytes)
        }
        Err(_) => response(StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
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
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ActivityProxy {
    async fn start(listener: TcpListener, target_port: u16) -> Self {
        let mode = Arc::new(AtomicU8::new(FORWARD));
        let shutdown = CancellationToken::new();
        let state = ActivityProxyState {
            mode: Arc::clone(&mode),
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
            shutdown,
            task,
        }
    }

    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::Release);
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

async fn wait_for_fragment_index(daemon: &mut Daemon) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        daemon.assert_running("waiting for the fragment index");
        let diagnostics = daemon.diagnostics();
        if diagnostics.lines().any(|line| {
            line.contains("fragment indexing pass finished") && line.contains("built=1")
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fragment index was not built:\n{diagnostics}"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_a_reports_node_b_delivery_and_bounded_peer_failures() {
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
    let mut node_a = Daemon::spawn(&a_config, root.path().join("node-a.log"));
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
    let mut node_b = Daemon::spawn(&b_config, root.path().join("node-b.log"));
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

    // VOD deliberately refuses to start until this node-local prerequisite is
    // durable. Wait on the scheduler's completed-work signal instead of racing
    // the scan with repeated session requests.
    wait_for_fragment_index(&mut node_b).await;

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

    proxy.set(UNREACHABLE);
    let unavailable = activity_detail(&client, &a_base, &token).await;
    assert!(unavailable["activity_nodes"]
        .as_array()
        .expect("activity nodes")
        .iter()
        .any(|node| node["node_id"] == remote_node && node["status"] == "unreachable"));
    assert!(unavailable["deliveries"]
        .as_array()
        .expect("deliveries")
        .iter()
        .all(|delivery| delivery["node_id"] != remote_node));

    proxy.set(HUNG);
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

    drop(node_b);
    drop(node_a);
    proxy.stop().await;
}
