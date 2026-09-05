//! Two-node Live TV acceptance: ingress relay, and no takeover without proof.
//!
//! A single node cannot show either property. The relay only exists when the
//! reader and the tuner owner are different machines, and "the owner is gone"
//! is not a state one process can be in. Both promises in the plan — that an
//! ingress serves a capability it does not own, and that elapsed time alone
//! never lets another node open the tuner — need two real daemons and a real
//! device to be worth anything.
//!
//! The device is an HDHomeRun-shaped fixture on this host's own private
//! address: discover.json and lineup.json on port 80, the stream on 5004,
//! which is how `pinned_url` addresses a real one. Binding 80 needs a
//! container or root, so this joins `cluster_activation` and `cluster_activity`
//! behind the `cluster-integration-tests` feature rather than being ignored:
//! anyone who asks for it gets a verdict, and an environment that cannot host
//! the fixture device says so instead of quietly passing.

#![cfg(unix)]

use std::fs::{File, Permissions};
use std::net::{TcpListener, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use plurx_core::store::SqliteStore;
use serde_json::{json, Value};
use tokio::net::TcpListener as TokioTcpListener;
use tokio_util::io::ReaderStream;

const GUIDE_NUMBER: &str = "7.1";
const DEVICE_PORT: u16 = 80;
const TUNER_PORT: u16 = 5004;

// ---------------------------------------------------------------------------
// Daemon harness. Copied from cluster_activity.rs rather than shared: these
// integration tests are separate crates, and a `tests/common` refactor would
// touch a passing 41 KB file for no behavioural gain.
// ---------------------------------------------------------------------------

fn canonical_tempdir() -> tempfile::TempDir {
    let root =
        std::fs::canonicalize(std::env::temp_dir()).expect("canonical system temporary directory");
    tempfile::tempdir_in(root).expect("two-node live TV root")
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
            .env("NO_COLOR", "1")
            .env("PLURX_TEST_SCHEDULER_TICK_MS", "250")
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

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

// ---------------------------------------------------------------------------
// The fixture device.
// ---------------------------------------------------------------------------

/// The address the daemon will accept as a device: private, and not loopback.
fn private_ipv4() -> String {
    let probe = UdpSocket::bind("0.0.0.0:0").expect("probe socket");
    probe.connect("192.0.2.1:80").expect("probe route"); // TEST-NET-1; nothing is sent
    let address = probe.local_addr().expect("probe address").ip().to_string();
    let octets: Vec<u8> = address
        .split('.')
        .map(|part| part.parse().expect("dotted quad"))
        .collect();
    let private = octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 169 && octets[1] == 254);
    assert!(
        private,
        "this host's address {address} is not one the daemon accepts as a tuner device"
    );
    address
}

#[derive(Clone)]
struct TunerState {
    address: String,
    opens: Arc<AtomicU64>,
}

async fn discover(State(state): State<TunerState>) -> axum::Json<Value> {
    let base = format!("http://{}:{DEVICE_PORT}", state.address);
    axum::Json(json!({
        "FriendlyName": "Fixture HDHomeRun",
        "ModelNumber": "HDFX-4K",
        "FirmwareName": "hdhomerun_fixture",
        "FirmwareVersion": "20260326",
        "DeviceID": "FIXTURE2",
        "TunerCount": 2,
        "BaseURL": base,
        "LineupURL": format!("{base}/lineup.json"),
    }))
}

async fn lineup(State(state): State<TunerState>) -> axum::Json<Value> {
    axum::Json(json!([{
        "GuideNumber": GUIDE_NUMBER,
        "GuideName": "Two Node Fixture",
        "URL": format!("http://{}:{TUNER_PORT}/auto/v{GUIDE_NUMBER}", state.address),
    }]))
}

/// The stream. Counting entries here is the whole point: the physical tuner is
/// opened by whoever owns it, once, and an ingress must never open a second.
async fn stream(State(state): State<TunerState>) -> Result<Body, StatusCode> {
    state.opens.fetch_add(1, Ordering::AcqRel);
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(Body::from_stream(ReaderStream::new(stdout)))
}

async fn serve_fixture_device(address: String, opens: Arc<AtomicU64>) {
    let state = TunerState { address, opens };
    let router = Router::new()
        .route("/discover.json", get(discover))
        .route("/lineup.json", get(lineup))
        .route(&format!("/auto/v{GUIDE_NUMBER}"), get(stream))
        .with_state(state);
    for port in [DEVICE_PORT, TUNER_PORT] {
        let listener = TokioTcpListener::bind(("0.0.0.0", port))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "the fixture device could not bind port {port}: {error}. \
                        This acceptance needs a container or root; run it by name."
                )
            });
        let app = router.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
    }
}

// ---------------------------------------------------------------------------

async fn settings(client: &reqwest::Client, base: &str, token: &str) -> Value {
    client
        .get(format!("{base}/api/v1/settings"))
        .bearer_auth(token)
        .send()
        .await
        .expect("settings request")
        .json::<Value>()
        .await
        .expect("settings JSON")
}

async fn put_settings(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = client
        .put(format!("{base}/api/v1/settings"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("settings write");
    let status = response.status();
    let value = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, value)
}

async fn wait_live_tv_ready(client: &reqwest::Client, base: &str, token: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let response = client
            .post(format!("{base}/api/v1/live-tv/readiness/refresh"))
            .bearer_auth(token)
            .send()
            .await
            .expect("readiness refresh");
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        if body["ready"].as_bool() == Some(true) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "live TV readiness never came up: {body}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Before Live TV is enabled the enablement check is expected to fail; the
/// device, configuration and graph checks are the ones that must come up.
async fn wait_live_tv_ready_for_configuration(client: &reqwest::Client, base: &str, token: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let body = client
            .post(format!("{base}/api/v1/live-tv/readiness/refresh"))
            .bearer_auth(token)
            .send()
            .await
            .expect("readiness refresh")
            .json::<Value>()
            .await
            .unwrap_or(Value::Null);
        let unresolved: Vec<String> = body["checks"]
            .as_array()
            .map(|checks| {
                checks
                    .iter()
                    .filter(|check| check["ready"].as_bool() != Some(true))
                    .map(|check| check["id"].as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        if body["ready"].as_bool() == Some(true)
            || (!unresolved.is_empty() && unresolved.iter().all(|id| id.contains("enabl")))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "configuration readiness never came up: {body}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ---------------------------------------------------------------------------
// One fixture device for the whole process. The daemon reads discover.json
// from port 80, so two tests cannot each own one; they share it and take turns.
// ---------------------------------------------------------------------------

struct Device {
    address: String,
    opens: Arc<AtomicU64>,
}

static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
static SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

async fn device() -> &'static Device {
    if let Some(existing) = DEVICE.get() {
        return existing;
    }
    let address = private_ipv4();
    let opens = Arc::new(AtomicU64::new(0));
    serve_fixture_device(address.clone(), Arc::clone(&opens)).await;
    DEVICE.get_or_init(|| Device { address, opens })
}

/// Every case configures the same physical device, so they run one at a time
/// and each starts from a known open count rather than inheriting one.
async fn exclusive_device() -> (&'static Device, tokio::sync::MutexGuard<'static, ()>) {
    let lock = SERIAL.get_or_init(|| tokio::sync::Mutex::new(()));
    let guard = lock.lock().await;
    let device = device().await;
    device.opens.store(0, Ordering::Release);
    (device, guard)
}

// ---------------------------------------------------------------------------

struct Cluster {
    _root: tempfile::TempDir,
    node_a: Daemon,
    node_b: Daemon,
    client: reqwest::Client,
    token: String,
    a_base: String,
    b_base: String,
    node_a_id: String,
    node_b_id: String,
}

impl Cluster {
    /// Two real voters, joined, with node A holding the local identity that
    /// every case below installs as the tuner owner.
    async fn start() -> Self {
        let root = canonical_tempdir();
        let mut ports = reserved_ports(6).into_iter();
        let mut a_http = ports.next().expect("A http");
        let mut a_raft = ports.next().expect("A raft");
        let mut a_api = ports.next().expect("A api");
        let mut b_http = ports.next().expect("B http");
        let mut b_raft = ports.next().expect("B raft");
        let mut b_api = ports.next().expect("B api");

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
                 join_url = \"http://127.0.0.1:{}\"\n",
                a_http.port,
                toml_path(&a_data),
                a_raft.port,
                a_api.port,
                a_http.port,
            ),
        )
        .expect("node A config");

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("test client");

        let a_port = a_http.port;
        a_http.release();
        a_raft.release();
        a_api.release();
        let mut node_a = Daemon::spawn(&a_config, root.path().join("node-a.log"), "plurx-live-a");
        wait_ready(&client, &mut node_a, a_port).await;
        let a_base = format!("http://127.0.0.1:{a_port}");

        let setup = client
            .post(format!("{a_base}/api/v1/setup"))
            .json(&json!({"username": "owner", "password": "two-node-live-tv-password"}))
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
            .json(&json!({"expires_in_seconds": 600}))
            .send()
            .await
            .expect("issue join token");
        assert_eq!(issued.status(), StatusCode::OK);
        let join_token = issued.json::<Value>().await.expect("join JSON")["token"]
            .as_str()
            .expect("join token")
            .to_owned();

        let b_data = root.path().join("node-b");
        std::fs::create_dir_all(&b_data).expect("node B data");
        let join_file = root.path().join("node-b.join");
        std::fs::write(&join_file, format!("{join_token}\n")).expect("join token file");
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
                 join_token_file = \"{}\"\n",
                b_http.port,
                toml_path(&b_data),
                b_raft.port,
                b_api.port,
                toml_path(&join_file),
            ),
        )
        .expect("node B config");
        let b_port = b_http.port;
        b_http.release();
        b_raft.release();
        b_api.release();
        let mut node_b = Daemon::spawn(&b_config, root.path().join("node-b.log"), "plurx-live-b");
        wait_ready(&client, &mut node_b, b_port).await;
        let b_base = format!("http://127.0.0.1:{b_port}");
        let roster = wait_for_two_voters(&client, &mut node_a, &a_base, &token).await;
        let node_a_id = roster["local_node_id"]
            .as_str()
            .expect("local node id")
            .to_owned();
        let node_b_id = roster["nodes"]
            .as_array()
            .expect("node rows")
            .iter()
            .filter_map(|node| node["node_id"].as_str())
            .find(|id| *id != node_a_id)
            .expect("peer node id")
            .to_owned();

        Self {
            _root: root,
            node_a,
            node_b,
            client,
            token,
            a_base,
            b_base,
            node_a_id,
            node_b_id,
        }
    }

    async fn generation(&self) -> Value {
        settings(&self.client, &self.a_base, &self.token).await["live_tv_config_generation"].clone()
    }

    async fn configure(&self, device: &str, owner: &str) {
        let (status, body) = put_settings(
            &self.client,
            &self.a_base,
            &self.token,
            json!({
                "live_tv_config_generation": self.generation().await,
                "live_tv_device_ipv4": device,
                "live_tv_owner_node_id": owner,
                "live_tv_max_sessions": 2,
                "live_tv_output_height": 720,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "configure refused: {body}");
    }

    async fn set_enabled(&self, enabled: bool) -> (StatusCode, Value) {
        put_settings(
            &self.client,
            &self.a_base,
            &self.token,
            json!({
                "live_tv_config_generation": self.generation().await,
                "live_tv_enabled": enabled,
            }),
        )
        .await
    }

    async fn enable(&self) {
        wait_live_tv_ready_for_configuration(&self.client, &self.a_base, &self.token).await;
        let (status, body) = self.set_enabled(true).await;
        assert_eq!(status, StatusCode::OK, "enable refused: {body}");
        wait_live_tv_ready(&self.client, &self.a_base, &self.token).await;
    }

    /// The channel id as the given node reports it. Asking the ingress is the
    /// point in the relay case: its lineup comes from the owner, not its own
    /// device access.
    async fn channel_id(&self, base: &str) -> String {
        let lineup = self
            .client
            .get(format!("{base}/api/v1/live-tv/channels"))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("lineup")
            .json::<Value>()
            .await
            .expect("lineup JSON");
        lineup["channels"]
            .as_array()
            .expect("channels")
            .iter()
            .find(|row| row["guide_number"] == GUIDE_NUMBER)
            .unwrap_or_else(|| panic!("lineup lacked the fixture channel: {lineup}"))["id"]
            .as_str()
            .expect("channel id")
            .to_owned()
    }

    async fn start_session(&self, base: &str, channel: &str) -> reqwest::Response {
        self.client
            .post(format!("{base}/api/v1/live-tv/channels/{channel}/sessions"))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("start request")
    }

    /// Read the capability through `base` until its window rolls once, fetching
    /// segments as a player would. A relayed playlist that never advances is a
    /// proxy of nothing.
    async fn read_until_rolled(&mut self, base: &str, capability: &str) -> usize {
        let playlist_url = format!("{base}/api/v1/live-tv/sessions/{capability}/index.m3u8");
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut first: Option<i64> = None;
        let mut latest = 0i64;
        let mut read = 0usize;
        loop {
            self.node_a.assert_running("reading live media");
            self.node_b.assert_running("reading live media");
            let response = self
                .client
                .get(&playlist_url)
                .send()
                .await
                .expect("playlist request");
            assert_eq!(response.status(), StatusCode::OK, "playlist refused");
            let text = response.text().await.expect("playlist body");
            if let Some(line) = text
                .lines()
                .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
            {
                let sequence: i64 = line.trim().parse().expect("media sequence");
                first.get_or_insert(sequence);
                latest = sequence;
            }
            let names: Vec<String> = text
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .map(|line| line.trim().to_owned())
                .collect();
            assert!(
                names.len() <= LISTED_SEGMENTS,
                "the window published {} segments; at most {LISTED_SEGMENTS} may be listed",
                names.len()
            );
            if let Some(name) = names.last() {
                let segment = self
                    .client
                    .get(format!(
                        "{base}/api/v1/live-tv/sessions/{capability}/{name}"
                    ))
                    .send()
                    .await
                    .expect("segment request");
                if segment.status() == StatusCode::OK
                    && !segment.bytes().await.expect("segment body").is_empty()
                {
                    read += 1;
                }
            }
            if latest - first.unwrap_or(0) >= LISTED_SEGMENTS as i64 {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "the window never rolled: sequence {latest}; node A: {}",
                self.node_a.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Wait until the capability stops being served, which is what draining
    /// looks like from outside. Returns how long it took.
    async fn wait_until_refused(
        &mut self,
        base: &str,
        capability: &str,
        budget: Duration,
    ) -> Duration {
        let url = format!("{base}/api/v1/live-tv/sessions/{capability}/index.m3u8");
        let started = Instant::now();
        while started.elapsed() < budget {
            self.node_b.assert_running("waiting for the drain");
            match self.client.get(&url).send().await {
                Ok(response) if !response.status().is_success() => return started.elapsed(),
                Ok(_) => {}
                Err(_) => return started.elapsed(),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!(
            "the capability was still served {}s after the drain began; node A: {}",
            budget.as_secs(),
            self.node_a.diagnostics()
        )
    }
}

const LISTED_SEGMENTS: usize = 6; // MAX_LISTED_SEGMENTS in crates/plurxd/src/live_tv.rs

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ingress_relays_a_capability_it_does_not_own_and_never_takes_the_tuner_over() {
    let (device, _serial) = exclusive_device().await;
    let mut cluster = Cluster::start().await;
    cluster
        .configure(&device.address, &cluster.node_a_id.clone())
        .await;
    cluster.enable().await;

    // Everything below is asked of node B, which does not own the device.
    let channel = cluster.channel_id(&cluster.b_base.clone()).await;
    let started = cluster
        .start_session(&cluster.b_base.clone(), &channel)
        .await;
    assert_eq!(
        started.status(),
        StatusCode::OK,
        "the ingress could not start a session on the owner's tuner; node B: {} node A: {}",
        cluster.node_b.diagnostics(),
        cluster.node_a.diagnostics()
    );
    let capability = started.json::<Value>().await.expect("start JSON")["session_id"]
        .as_str()
        .expect("capability")
        .to_owned();

    let read = cluster
        .read_until_rolled(&cluster.b_base.clone(), &capability)
        .await;
    assert!(
        read >= 3,
        "the ingress served {read} segments; a relay that returns no media is not a relay"
    );
    assert_eq!(
        device.opens.load(Ordering::Acquire),
        1,
        "the tuner was opened more than once for a single relayed session"
    );

    // The owner disappears. Elapsed time must never be enough for the ingress
    // to open the physical tuner itself.
    cluster.node_a.stop();
    tokio::time::sleep(Duration::from_secs(75)).await;
    cluster.node_b.assert_running("after the owner was lost");

    if let Ok(response) = cluster
        .client
        .post(format!(
            "{}/api/v1/live-tv/channels/{channel}/sessions",
            cluster.b_base
        ))
        .bearer_auth(&cluster.token)
        .send()
        .await
    {
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "the ingress started a session with the tuner owner gone: {}",
            response.text().await.unwrap_or_default()
        );
    }
    assert_eq!(
        device.opens.load(Ordering::Acquire),
        1,
        "the ingress opened the physical tuner after the owner was lost; elapsed time is not \
         proof that the old owner stopped, and automatic takeover is out of scope"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabling_live_tv_drains_the_session_and_refuses_the_next_one() {
    let (device, _serial) = exclusive_device().await;
    let mut cluster = Cluster::start().await;
    cluster
        .configure(&device.address, &cluster.node_a_id.clone())
        .await;
    cluster.enable().await;

    let channel = cluster.channel_id(&cluster.a_base.clone()).await;
    let started = cluster
        .start_session(&cluster.b_base.clone(), &channel)
        .await;
    assert_eq!(
        started.status(),
        StatusCode::OK,
        "start refused before disable"
    );
    let capability = started.json::<Value>().await.expect("start JSON")["session_id"]
        .as_str()
        .expect("capability")
        .to_owned();
    cluster
        .read_until_rolled(&cluster.b_base.clone(), &capability)
        .await;
    assert_eq!(device.opens.load(Ordering::Acquire), 1);

    // Disabling is the administrator's stop button. It has to end the live
    // session, not merely stop new ones, or the tuner stays open behind a
    // setting that claims Live TV is off.
    let (status, body) = cluster.set_enabled(false).await;
    assert_eq!(status, StatusCode::OK, "disable refused: {body}");
    let took = cluster
        .wait_until_refused(
            &cluster.b_base.clone(),
            &capability,
            Duration::from_secs(90),
        )
        .await;

    let refused = cluster
        .start_session(&cluster.b_base.clone(), &channel)
        .await;
    assert_ne!(
        refused.status(),
        StatusCode::OK,
        "a channel started while Live TV was disabled"
    );
    let refusal = refused.json::<Value>().await.unwrap_or(Value::Null);
    assert_eq!(
        refusal["code"].as_str(),
        Some("live_tv_disabled"),
        "the refusal must name the reason an operator can act on: {refusal}"
    );
    assert_eq!(
        device.opens.load(Ordering::Acquire),
        1,
        "disabling Live TV opened another tuner (drain took {took:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_the_owner_hands_the_tuner_to_the_new_node_exactly_once() {
    let (device, _serial) = exclusive_device().await;
    let mut cluster = Cluster::start().await;
    let node_a_id = cluster.node_a_id.clone();
    let node_b_id = cluster.node_b_id.clone();
    cluster.configure(&device.address, &node_a_id).await;
    cluster.enable().await;

    let channel = cluster.channel_id(&cluster.a_base.clone()).await;
    let started = cluster
        .start_session(&cluster.a_base.clone(), &channel)
        .await;
    assert_eq!(
        started.status(),
        StatusCode::OK,
        "start refused on the first owner"
    );
    let capability = started.json::<Value>().await.expect("start JSON")["session_id"]
        .as_str()
        .expect("capability")
        .to_owned();
    cluster
        .read_until_rolled(&cluster.a_base.clone(), &capability)
        .await;
    assert_eq!(device.opens.load(Ordering::Acquire), 1);

    // The documented order: disable, move the owner, re-enable. The previous
    // owner is alive here, so it can confirm its own cleanup — which is the
    // only thing that should let the new owner take the device.
    let (status, body) = cluster.set_enabled(false).await;
    assert_eq!(status, StatusCode::OK, "disable refused: {body}");
    cluster
        .wait_until_refused(
            &cluster.a_base.clone(),
            &capability,
            Duration::from_secs(90),
        )
        .await;
    cluster.configure(&device.address, &node_b_id).await;
    cluster.enable().await;

    let after = settings(
        &cluster.client,
        &cluster.a_base.clone(),
        &cluster.token.clone(),
    )
    .await;
    assert_eq!(
        after["live_tv_owner_node_id"].as_str(),
        Some(node_b_id.as_str()),
        "the owner did not move: {after}"
    );
    assert_eq!(
        after["live_tv_transition_from_owner_node_id"].as_str(),
        Some(""),
        "a confirmed drain must clear the transition, not leave it pending: {after}"
    );

    // Node A is now the ingress. The stream must come from the new owner.
    let channel = cluster.channel_id(&cluster.a_base.clone()).await;
    let moved = cluster
        .start_session(&cluster.a_base.clone(), &channel)
        .await;
    assert_eq!(
        moved.status(),
        StatusCode::OK,
        "the new owner could not open the device: {} / node B: {}",
        moved.text().await.unwrap_or_default(),
        cluster.node_b.diagnostics()
    );
    let capability = moved.json::<Value>().await.expect("start JSON")["session_id"]
        .as_str()
        .expect("capability")
        .to_owned();
    let read = cluster
        .read_until_rolled(&cluster.a_base.clone(), &capability)
        .await;
    assert!(read >= 3, "the moved owner served {read} segments");
    assert_eq!(
        device.opens.load(Ordering::Acquire),
        2,
        "the device was opened {} times across one owner move; the old owner must release it \
         and the new one must open it exactly once",
        device.opens.load(Ordering::Acquire)
    );
}
