//! Separate-daemon durable sharing restart proof in a disposable CGNAT namespace.
//! The raw TCP proxy here is a fixture, not Tailscale or topology qualification.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)]
use plurx_core::store::SqliteStore;
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{
    fs::File,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct FixtureTask(Option<tokio::task::JoinHandle<()>>);
impl Drop for FixtureTask {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}
impl FixtureTask {
    async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
struct Daemon {
    child: Child,
    config: PathBuf,
    log: PathBuf,
    port: u16,
}
impl Daemon {
    fn spawn(config: PathBuf, log: PathBuf, port: u16) -> Self {
        let output = File::create(&log).expect("daemon log");
        let child = Command::new(env!("CARGO_BIN_EXE_plurxd"))
            .args(["--config", config.to_str().expect("config path"), "run"])
            .env("NO_COLOR", "1")
            .env("PLURX_NODE_HOSTNAME", format!("sharing-restart-{port}"))
            .stdin(Stdio::null())
            .stderr(output.try_clone().expect("stderr log"))
            .stdout(output)
            .spawn()
            .expect("start separate daemon");
        Self {
            child,
            config,
            log,
            port,
        }
    }
    fn stop(&mut self) {
        if self.child.try_wait().expect("child status").is_none() {
            self.child.kill().expect("stop fixture daemon");
        }
        self.child.wait().expect("reap fixture daemon");
    }
    fn restart(&mut self) {
        self.stop();
        *self = Self::spawn(self.config.clone(), self.log.clone(), self.port);
    }
    fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("child status") {
            panic!(
                "daemon exited {status}: {}",
                std::fs::read_to_string(&self.log).unwrap_or_default()
            );
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reserve() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("reserve fixture port")
}
fn config(root: &Path, name: &str, address: std::net::IpAddr) -> (PathBuf, u16, u16) {
    let sockets = [reserve(), reserve(), reserve(), reserve()];
    let ports = sockets
        .each_ref()
        .map(|s| s.local_addr().expect("port").port());
    let data = root.join(name);
    std::fs::create_dir_all(&data).expect("fixture data");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("initial SQLite import source"));
    let config = root.join(format!("{name}.toml"));
    let data = data
        .to_str()
        .expect("data path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    std::fs::write(&config, format!(
        "[server]\nname=\"{name}\"\nbind=\"127.0.0.1:{}\"\n[storage]\ndata_dir=\"{data}\"\n[cluster]\nraft_bind=\"127.0.0.1:{}\"\napi_bind=\"127.0.0.1:{}\"\nadvertise_host=\"127.0.0.1\"\n[sharing]\nbind=\"127.0.0.1:{}\"\n[sharing.egress]\nmode=\"local_address\"\naddress=\"{address}\"\n", ports[0], ports[1], ports[2], ports[3]
    )).expect("daemon config");
    (config, ports[0], ports[3])
}
async fn ready(client: &Client, daemon: &mut Daemon) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        daemon.assert_running();
        if client
            .get(daemon.url("/readyz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon readiness deadline: {}",
            std::fs::read_to_string(&daemon.log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn call(
    client: &Client,
    daemon: &Daemon,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Value {
    let mut request = client.request(method, daemon.url(path));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("fixture API request");
    let status = response.status();
    let value = response.json::<Value>().await.expect("fixture JSON");
    // Never print an invitation or credential-bearing API body on failure.
    if !status.is_success() {
        for line in std::fs::read_to_string(&daemon.log)
            .unwrap_or_default()
            .lines()
        {
            if line.contains("sharing rotation failed") {
                eprintln!("{line}");
            }
        }
    }
    assert!(
        status.is_success(),
        "{path}: HTTP {status}; code={}",
        value["code"]
    );
    value
}
async fn setup(client: &Client, daemon: &mut Daemon) -> String {
    ready(client, daemon).await;
    call(
        client,
        daemon,
        Method::POST,
        "/api/v1/setup",
        None,
        Some(json!({"username":"owner","password":"sharing-restart-fixture-password"})),
    )
    .await["token"]
        .as_str()
        .expect("admin token")
        .to_owned()
}
async fn sharing_status(client: &Client, daemon: &mut Daemon, token: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let status = call(
            client,
            daemon,
            Method::GET,
            "/api/v1/sharing/status",
            Some(token),
            None,
        )
        .await;
        if status["listener"] == "listening" && status["certificate"].is_object() {
            return status;
        }
        assert!(Instant::now() < deadline, "sharing listener deadline");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn import_state(client: &Client, daemon: &mut Daemon, token: &str, state: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let status = call(
            client,
            daemon,
            Method::GET,
            "/api/v1/sharing/imports",
            Some(token),
            None,
        )
        .await;
        if let Some(entry) = status["imports"]
            .as_array()
            .expect("imports")
            .iter()
            .find(|entry| entry["import"]["state"] == state)
        {
            return entry.clone();
        }
        assert!(Instant::now() < deadline, "import state {state} deadline");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
#[tokio::test]
#[ignore = "requires disposable CGNAT namespace; run explicitly with PLURX_SHARING_FIXTURE_IP"]
async fn sharing_separate_daemons_preserve_pending_pairing_and_rotation_across_restart() {
    let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit fixture address")
        .parse()
        .expect("fixture IP");
    assert!(
        matches!(address, std::net::IpAddr::V4(ip) if ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
    );
    let root = tempfile::tempdir().expect("disposable process state");
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("fixture admin client");
    let (source_config, source_port, source_tls) = config(root.path(), "source", address);
    let (recipient_config, recipient_port, _) = config(root.path(), "recipient", address);
    let mut source = Daemon::spawn(source_config, root.path().join("source.log"), source_port);
    let mut recipient = Daemon::spawn(
        recipient_config,
        root.path().join("recipient.log"),
        recipient_port,
    );
    let source_token = setup(&client, &mut source).await;
    let recipient_token = setup(&client, &mut recipient).await;
    for (daemon, token) in [(&source, &source_token), (&recipient, &recipient_token)] {
        call(
            &client,
            daemon,
            Method::PUT,
            "/api/v1/sharing/settings",
            Some(token),
            Some(json!({"enabled":true})),
        )
        .await;
    }
    let source_status = sharing_status(&client, &mut source, &source_token).await;
    let source_pin = source_status["certificate"]["spki_sha256"].clone();
    // A raw forwarding fixture keeps the production TLS handshake and pin in
    // the path. No host Tailscale configuration or firewall is changed.
    let listener = tokio::net::TcpListener::bind((address, 0))
        .await
        .expect("CGNAT fixture listener");
    let peer_port = listener.local_addr().expect("peer port").port();
    let proxy = FixtureTask(Some(tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut stream, _) = tokio::select! {
                accepted = listener.accept() => accepted.expect("fixture acceptance"),
                _ = connections.join_next(), if !connections.is_empty() => continue,
            };
            connections.spawn(async move {
                if let Ok(mut upstream) =
                    tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, source_tls))
                        .await
                {
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                }
            });
        }
    })));
    call(&client, &source, Method::PUT, "/api/v1/sharing/endpoints", Some(&source_token), Some(json!({"expected_revision":0,"endpoints":[{"ipv4":address.to_string(),"ts_fqdn":"source.fixture.ts.net","port":peer_port,"spki_sha256":source_pin}]}))).await;
    let media = root.path().join("media");
    std::fs::create_dir(&media).expect("empty library directory");
    let library = call(
        &client,
        &source,
        Method::POST,
        "/api/v1/libraries",
        Some(&source_token),
        Some(json!({"name":"Restart proof","kind":"movies","paths":[media]})),
    )
    .await;
    let invitation = call(
        &client,
        &source,
        Method::POST,
        "/api/v1/sharing/invitations",
        Some(&source_token),
        Some(json!({"library_ids":[library["id"].as_i64().expect("library ID").to_string()]})),
    )
    .await;
    call(
        &client,
        &recipient,
        Method::POST,
        "/api/v1/sharing/imports",
        Some(&recipient_token),
        Some(json!({"invitation":invitation["invitation"]})),
    )
    .await;
    let pending = import_state(&client, &mut recipient, &recipient_token, "pending").await;
    recipient.restart();
    ready(&client, &mut recipient).await;
    let pending_after = import_state(&client, &mut recipient, &recipient_token, "pending").await;
    assert_eq!(pending_after["pairing_code"], pending["pairing_code"]);
    assert_eq!(pending_after["import"]["id"], pending["import"]["id"]);
    assert_eq!(
        pending_after["import"]["source_server_id"],
        pending["import"]["source_server_id"]
    );
    let exports = call(
        &client,
        &source,
        Method::GET,
        "/api/v1/sharing/exports",
        Some(&source_token),
        None,
    )
    .await;
    let export = &exports["exports"][0];
    assert_eq!(export["pairing_code"], pending["pairing_code"]);
    let grant = export["grant"]["id"].as_str().expect("grant ID");
    call(&client, &source, Method::POST, &format!("/api/v1/sharing/exports/{grant}/approve"), Some(&source_token), Some(json!({"expected_mutation_generation":export["grant"]["mutation_generation"],"pairing_code":pending["pairing_code"]}))).await;
    let active = import_state(&client, &mut recipient, &recipient_token, "active").await;
    let import_id = active["import"]["id"].as_str().expect("import ID");
    call(
        &client,
        &recipient,
        Method::POST,
        &format!("/api/v1/sharing/imports/{import_id}/rotate"),
        Some(&recipient_token),
        Some(json!({})),
    )
    .await;
    // Publish a new authenticated manifest revision before restarting. The
    // recipient must recover its rotated credential to retrieve this revision;
    // its retained active label cannot satisfy this assertion.
    call(&client, &source, Method::PUT, "/api/v1/sharing/endpoints", Some(&source_token), Some(json!({"expected_revision":1,"endpoints":[{"ipv4":address.to_string(),"ts_fqdn":"source.fixture.ts.net","port":peer_port,"spki_sha256":source_pin}]}))).await;
    source.restart();
    recipient.restart();
    ready(&client, &mut source).await;
    ready(&client, &mut recipient).await;
    let recovered_source = sharing_status(&client, &mut source, &source_token).await;
    assert_eq!(recovered_source["certificate"]["spki_sha256"], source_pin);
    sharing_status(&client, &mut recipient, &recipient_token).await;
    let after = import_state(&client, &mut recipient, &recipient_token, "active").await;
    assert_eq!(after["import"]["id"], active["import"]["id"]);
    assert_eq!(
        after["import"]["remote_grant_id"],
        active["import"]["remote_grant_id"]
    );
    assert_eq!(
        after["import"]["catalogue_epoch"],
        active["import"]["catalogue_epoch"]
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let recovered = import_state(&client, &mut recipient, &recipient_token, "active").await;
        if recovered["import"]["observed_endpoint_revision"] == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recovered credential must authenticate the new manifest"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let exports_after = call(
        &client,
        &source,
        Method::GET,
        "/api/v1/sharing/exports",
        Some(&source_token),
        None,
    )
    .await;
    assert_eq!(
        exports_after["exports"].as_array().expect("exports").len(),
        1
    );
    assert_eq!(
        exports_after["exports"][0]["grant"]["id"],
        active["import"]["remote_grant_id"]
    );
    let renewed = sharing_status(&client, &mut source, &source_token).await;
    assert_eq!(renewed["certificate"]["spki_sha256"], source_pin);
    proxy.stop().await;
}
