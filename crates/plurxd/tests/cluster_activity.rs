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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind test port")
        .local_addr()
        .expect("test port")
        .port()
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
    async fn start(port: u16, target_port: u16) -> Self {
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
        let listener = TokioTcpListener::bind(("127.0.0.1", port))
            .await
            .expect("activity proxy bind");
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

    let a_http = free_port();
    let a_raft = free_port();
    let a_api = free_port();
    let b_http = free_port();
    let b_raft = free_port();
    let b_api = free_port();
    let proxy_port = free_port();
    let proxy = ActivityProxy::start(proxy_port, b_http).await;

    let a_data = root.path().join("node-a");
    std::fs::create_dir_all(&a_data).expect("node A data");
    drop(SqliteStore::open(&a_data.join("plurx.db")).expect("legacy SQLite source"));
    let a_config = root.path().join("node-a.toml");
    std::fs::write(
        &a_config,
        format!(
            "[server]\nbind = \"127.0.0.1:{a_http}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{a_raft}\"\n\
             api_bind = \"127.0.0.1:{a_api}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             join_url = \"http://127.0.0.1:{a_http}\"\n\
             artwork_url = \"http://127.0.0.1:{a_http}\"\n",
            toml_path(&a_data),
        ),
    )
    .expect("node A config");

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(8))
        .build()
        .expect("test client");
    let mut node_a = Daemon::spawn(&a_config, root.path().join("node-a.log"));
    wait_ready(&client, &mut node_a, a_http).await;
    let a_base = format!("http://127.0.0.1:{a_http}");
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
            "[server]\nbind = \"127.0.0.1:{b_http}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{b_raft}\"\n\
             api_bind = \"127.0.0.1:{b_api}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             artwork_url = \"http://127.0.0.1:{proxy_port}\"\n\
             join_token_file = \"{}\"\n",
            toml_path(&b_data),
            toml_path(&join_file),
        ),
    )
    .expect("node B config");
    let mut node_b = Daemon::spawn(&b_config, root.path().join("node-b.log"));
    wait_ready(&client, &mut node_b, b_http).await;
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
    let file_id = loop {
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
            let detail = client
                .get(format!("{a_base}/api/v1/items/{item_id}"))
                .bearer_auth(&token)
                .send()
                .await
                .expect("item detail")
                .json::<Value>()
                .await
                .expect("item detail JSON");
            if let Some(file_id) = detail["files"]
                .as_array()
                .and_then(|files| files.first())
                .and_then(|file| file["id"].as_i64())
            {
                break file_id;
            }
        }
        assert!(
            Instant::now() < deadline,
            "library scan did not publish fixture with a file"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let b_base = format!("http://127.0.0.1:{b_http}");

    let hls = client
        .post(format!("{b_base}/api/v1/files/{file_id}/hls/sessions"))
        .bearer_auth(&token)
        .json(&json!({"playback_id":"cluster-activity-proof","copy":true}))
        .send()
        .await
        .expect("start node B HLS session");
    assert!(
        hls.status().is_success(),
        "HLS start failed: {}",
        hls.status()
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
