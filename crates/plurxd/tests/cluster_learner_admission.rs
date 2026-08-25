//! Real-process proof for the wire-distinct learner admission protocol.
//!
//! Unit tests can prove token parsing, but only independently running daemons
//! prove that v1 still adds voters while v2 commits a member that never enters
//! the voter set and survives a restart from its v2 membership file.

#![cfg(unix)]

use std::fs::{File, Permissions};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use plurx_core::cluster::membership::join_token_digest;
use plurx_core::store::SqliteStore;
use serde_json::{json, Value};

fn canonical_tempdir() -> tempfile::TempDir {
    let root =
        std::fs::canonicalize(std::env::temp_dir()).expect("canonical system temporary directory");
    tempfile::tempdir_in(root).expect("learner admission root")
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

    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().expect("stop daemon");
            self.child.wait().expect("reap daemon");
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

async fn wait_ready(client: &reqwest::Client, daemon: &mut Daemon, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(120);
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

async fn roster(client: &reqwest::Client, base: &str, admin: &str) -> Option<Value> {
    client
        .get(format!("{base}/api/v1/cluster/nodes"))
        .bearer_auth(admin)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

async fn wait_for_roles(
    client: &reqwest::Client,
    daemon: &mut Daemon,
    base: &str,
    admin: &str,
    voters: usize,
    learners: usize,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        daemon.assert_running("waiting for committed roles");
        if let Some(body) = roster(client, base, admin).await {
            let nodes = body["nodes"].as_array().cloned().unwrap_or_default();
            let observed_voters = nodes.iter().filter(|node| node["role"] == "voter").count();
            let observed_learners = nodes
                .iter()
                .filter(|node| node["role"] == "learner")
                .count();
            if observed_voters == voters && observed_learners == learners {
                return body;
            }
        }
        assert!(Instant::now() < deadline, "role convergence timed out");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct NodeSpec {
    http: u16,
    raft: u16,
    api: u16,
    data: PathBuf,
    config: PathBuf,
    join_file: PathBuf,
}

fn joining_spec(root: &Path, ordinal: usize) -> NodeSpec {
    NodeSpec {
        http: free_port(),
        raft: free_port(),
        api: free_port(),
        data: root.join(format!("node-{ordinal}")),
        config: root.join(format!("node-{ordinal}.toml")),
        join_file: root.join(format!("node-{ordinal}.join")),
    }
}

fn write_joining_config(spec: &NodeSpec, token: &str) {
    std::fs::create_dir_all(&spec.data).expect("joining data directory");
    std::fs::write(&spec.join_file, format!("{token}\n")).expect("write join token");
    std::fs::set_permissions(&spec.join_file, Permissions::from_mode(0o600))
        .expect("protect join token");
    std::fs::write(
        &spec.config,
        format!(
            "[server]\nbind = \"127.0.0.1:{}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             artwork_url = \"http://127.0.0.1:{}\"\n\
             join_token_file = \"{}\"\n",
            spec.http,
            toml_path(&spec.data),
            spec.raft,
            spec.api,
            spec.http,
            toml_path(&spec.join_file),
        ),
    )
    .expect("write joining config");
}

async fn issue_token(client: &reqwest::Client, base: &str, admin: &str, learner: bool) -> Value {
    let endpoint = if learner {
        "learner-join-tokens"
    } else {
        "join-tokens"
    };
    let response = client
        .post(format!("{base}/api/v1/cluster/{endpoint}"))
        .bearer_auth(admin)
        .json(&json!({"expires_in_seconds": 600}))
        .send()
        .await
        .expect("issue join token");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("issued token JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_voters_admit_and_restart_a_fourth_process_as_a_learner() {
    let root = canonical_tempdir();
    let first = joining_spec(root.path(), 1);
    std::fs::create_dir_all(&first.data).expect("first data directory");
    drop(SqliteStore::open(&first.data.join("plurx.db")).expect("legacy SQLite source"));
    std::fs::write(
        &first.config,
        format!(
            "[server]\nbind = \"127.0.0.1:{}\"\n\
             [storage]\ndata_dir = \"{}\"\n\
             [cluster]\nraft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n\
             join_url = \"http://127.0.0.1:{}\"\n\
             artwork_url = \"http://127.0.0.1:{}\"\n",
            first.http,
            toml_path(&first.data),
            first.raft,
            first.api,
            first.http,
            first.http,
        ),
    )
    .expect("write first config");

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(8))
        .build()
        .expect("test client");
    let mut first_daemon = Daemon::spawn(&first.config, root.path().join("node-1.log"));
    wait_ready(&client, &mut first_daemon, first.http).await;
    let base = format!("http://127.0.0.1:{}", first.http);
    let setup = client
        .post(format!("{base}/api/v1/setup"))
        .json(&json!({"username":"owner","password":"learner-proof-password"}))
        .send()
        .await
        .expect("setup request");
    assert_eq!(setup.status(), StatusCode::OK);
    let admin = setup.json::<Value>().await.expect("setup JSON")["token"]
        .as_str()
        .expect("setup token")
        .to_owned();

    let mut voters = Vec::new();
    for ordinal in 2..=3 {
        let issued = issue_token(&client, &base, &admin, false).await;
        let token = issued["token"].as_str().expect("voter token");
        assert!(token.starts_with("plxjoin:v1:"));
        let spec = joining_spec(root.path(), ordinal);
        write_joining_config(&spec, token);
        let mut daemon = Daemon::spawn(
            &spec.config,
            root.path().join(format!("node-{ordinal}.log")),
        );
        wait_ready(&client, &mut daemon, spec.http).await;
        wait_for_roles(&client, &mut first_daemon, &base, &admin, ordinal, 0).await;
        assert!(!spec.join_file.exists(), "v1 token file was not consumed");
        voters.push((spec, daemon));
    }

    let issued = issue_token(&client, &base, &admin, true).await;
    let learner_token = issued["token"].as_str().expect("learner token").to_owned();
    assert!(learner_token.starts_with("plxjoin:v2:"));
    assert!(!learner_token.starts_with("plxjoin:v1:"));
    let learner = joining_spec(root.path(), 4);

    // The v1 endpoint must reject role confusion without consuming or
    // reserving the v2 credential. The same token then succeeds through the
    // learner-only endpoint during ordinary daemon startup.
    let confused = client
        .post(format!("{base}/api/v1/cluster/join/redeem"))
        .json(&json!({
            "token_digest": join_token_digest(&learner_token),
            "raft_id": issued["raft_id"],
            "node_id": "role-confusion",
            "hostname": "role-confusion",
            "raft_address": format!("127.0.0.1:{}", learner.raft),
            "api_address": format!("127.0.0.1:{}", learner.api),
            "http_base": format!("http://127.0.0.1:{}", learner.http),
            "schema_version": plurx_core::store::AUTH_SCHEMA_VERSION,
            "protocol_version": plurx_core::store::AUTH_PROTOCOL_VERSION
        }))
        .send()
        .await
        .expect("role-confusion request");
    assert_eq!(confused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        confused.json::<Value>().await.expect("confusion refusal")["code"],
        "join_token_invalid"
    );

    write_joining_config(&learner, &learner_token);
    let learner_log = root.path().join("node-4.log");
    let mut learner_daemon = Daemon::spawn(&learner.config, learner_log.clone());
    wait_ready(&client, &mut learner_daemon, learner.http).await;
    let roster = wait_for_roles(&client, &mut first_daemon, &base, &admin, 3, 1).await;
    assert_eq!(roster["availability"], "high_availability");
    let learner_row = roster["nodes"]
        .as_array()
        .expect("roster nodes")
        .iter()
        .find(|node| node["role"] == "learner")
        .expect("learner row");
    assert_eq!(learner_row["is_leader"], false);
    assert!(
        !learner.join_file.exists(),
        "v2 token file was not consumed"
    );

    let membership: Value = serde_json::from_slice(
        &std::fs::read(learner.data.join("membership.json")).expect("v2 membership file"),
    )
    .expect("v2 membership JSON");
    assert_eq!(membership["version"], 2);
    assert_eq!(membership["role"], "learner");

    learner_daemon.stop();
    learner_daemon = Daemon::spawn(&learner.config, learner_log);
    wait_ready(&client, &mut learner_daemon, learner.http).await;
    let restarted = wait_for_roles(&client, &mut first_daemon, &base, &admin, 3, 1).await;
    assert_eq!(
        restarted["nodes"]
            .as_array()
            .expect("restart roster")
            .iter()
            .filter(|node| node["role"] == "voter")
            .count(),
        3
    );

    drop(learner_daemon);
    drop(voters);
}
