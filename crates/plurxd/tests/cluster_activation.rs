//! Process-level proof that migration precedes every daemon side effect.

// A test, never a daemon child: the launcher rule in clippy.toml is for
// production code.
#![allow(clippy::disallowed_methods)]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use plurx_core::auth::hash_password;
use plurx_core::store::{SqliteStore, UserStore};

static PROCESS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn canonical_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::fs::canonicalize(std::env::temp_dir())?;
    tempfile::tempdir_in(root)
}

fn process_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK.blocking_lock()
}

fn free_ports() -> [u16; 3] {
    // Asking the kernel for port zero and immediately closing the listener is
    // only an availability probe. Repeating that probe may return the same
    // ephemeral port, which would make the daemon's HTTP listener race its
    // Hiqlite listeners and send an HTTP assertion to the wrong service.
    // Hold all three probes open until their distinct ports are collected.
    let listeners: [TcpListener; 3] = std::array::from_fn(|_| {
        TcpListener::bind("127.0.0.1:0").expect("bind test port reservation")
    });
    std::array::from_fn(|index| {
        listeners[index]
            .local_addr()
            .expect("reserved test port")
            .port()
    })
}

fn toml_string(value: &std::path::Path) -> String {
    value
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

struct Daemon(Child, tempfile::NamedTempFile);

impl Daemon {
    fn spawn(mut command: Command) -> Self {
        let capture = tempfile::NamedTempFile::new().expect("private child log capture");
        command
            .env("PLURX_LOG", "info")
            .env("PLURX_LOG_FORMAT", "json")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        // One append-only file description shared by both streams prevents
        // stdout and stderr from overwriting each other's evidence.
        let stdout = std::fs::OpenOptions::new()
            .append(true)
            .open(capture.path())
            .expect("append child stdout");
        let stderr = stdout.try_clone().expect("append child stderr");
        command.stdout(stdout).stderr(stderr);
        Self(command.spawn().expect("start captured daemon"), capture)
    }

    fn captured_logs(&self) -> Result<String, String> {
        std::fs::read(self.1.path())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| format!("reading owned child log: {error}"))
    }

    fn log_tail(&self) -> String {
        let logs = self.captured_logs().unwrap_or_else(|error| error);
        let mut tail: Vec<_> = logs.chars().rev().take(16_384).collect();
        tail.reverse();
        tail.into_iter().collect()
    }

    fn ensure_running(&mut self) -> Result<(), String> {
        match self.0.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!(
                "owned daemon {} exited before owned HTTP bind: {status}; logs:\n{}",
                self.0.id(),
                self.log_tail()
            )),
            Err(error) => Err(format!(
                "checking owned daemon {}: {error}; logs:\n{}",
                self.0.id(),
                self.log_tail()
            )),
        }
    }

    fn owned_bind_probe(&mut self, address: SocketAddr) -> Result<bool, String> {
        self.ensure_running()?;
        // A TCP connect alone can hit another host-network job's listener or
        // even a loopback self-connection. The successful bind event must come
        // from this child's fresh capture, not an earlier boot or a marker.
        let logs = self.captured_logs()?;
        let bound = has_listening_event(&logs, address)
            && TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok();
        self.ensure_running()?;
        Ok(bound)
    }

    fn stop(&mut self) {
        if self.0.try_wait().expect("daemon status").is_none() {
            #[cfg(unix)]
            {
                // SAFETY: the pid comes from the live child this guard owns.
                assert_eq!(
                    unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) },
                    0,
                    "signal daemon"
                );
            }
            #[cfg(not(unix))]
            self.0.kill().expect("stop daemon");
        }
        let status = self.0.wait().expect("wait daemon");
        #[cfg(unix)]
        assert!(
            status.success(),
            "daemon did not drain cleanly: {status}; logs:\n{}",
            self.log_tail()
        );
    }

    #[cfg(unix)]
    fn kill_ungracefully(&mut self) -> ExitStatus {
        if self.0.try_wait().expect("daemon status").is_none() {
            // SAFETY: the pid comes from the live child this guard owns.
            assert_eq!(
                unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGKILL) },
                0,
                "kill daemon without graceful shutdown"
            );
        }
        self.0.wait().expect("wait for killed daemon")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "owned daemon {} captured logs:\n{}",
                self.0.id(),
                self.log_tail()
            );
        }
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// What a bounded child run leaves behind: how it ended, and everything it said.
struct BoundedRun {
    status: ExitStatus,
    logs: String,
}

/// Wait for a child that is expected to exit on its own, without ever blocking
/// forever on one that does not.
///
/// `Command::output()` is the obvious call here and the wrong one: a daemon
/// whose failpoint never fires simply serves, so `output()` waits for the heat
/// death of the test runner and the failure reads as a hang rather than as the
/// broken assertion it is. Both pipes are drained on their own threads because
/// a bounded wait that never reads them deadlocks as soon as startup logging
/// fills the pipe buffer.
fn wait_bounded(mut child: Child, timeout: Duration) -> BoundedRun {
    let mut stdout_pipe = child.stdout.take().expect("piped child stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped child stderr");
    let read = |pipe: &mut dyn std::io::Read| {
        let mut buffer = String::new();
        let _ = pipe.read_to_string(&mut buffer);
        buffer
    };
    let stdout = std::thread::spawn(move || read(&mut stdout_pipe));
    let stderr = std::thread::spawn(move || read(&mut stderr_pipe));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status") {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().expect("wait for killed child");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let logs = format!(
        "{}{}",
        stdout.join().expect("read child stdout"),
        stderr.join().expect("read child stderr")
    );
    assert!(
        !timed_out,
        "child never exited within {timeout:?} and was killed; logs: {logs}"
    );
    BoundedRun { status, logs }
}

fn has_listening_event(logs: &str, address: SocketAddr) -> bool {
    logs.lines().any(|line| {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        event["level"].as_str() == Some("INFO")
            && event["target"].as_str() == Some("plurxd")
            && event["fields"]["message"].as_str() == Some("listening")
            && event["fields"]["addr"]
                .as_str()
                .and_then(|value| value.parse::<SocketAddr>().ok())
                == Some(address)
    })
}

fn wait_for_bind(daemon: &mut Daemon, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        match daemon.owned_bind_probe(address) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => panic!("{error}"),
        }
        assert!(
            Instant::now() < deadline,
            "owned daemon HTTP bind timed out at {address}; logs:\n{}",
            daemon.log_tail()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn only_the_exact_structured_owned_listening_event_counts() {
    let address = SocketAddr::from(([127, 0, 0, 1], 12345));
    let event = serde_json::json!({
        "level": "INFO",
        "target": "plurxd",
        "fields": { "message": "listening", "addr": address.to_string() }
    });
    assert!(has_listening_event(&event.to_string(), address));
    for (key, replacement) in [
        ("message", "not listening"),
        ("addr", "127.0.0.1:12346"),
        ("addr", "127.0.0.1:123450"),
    ] {
        let mut other = event.clone();
        other["fields"][key] = replacement.into();
        assert!(!has_listening_event(&other.to_string(), address));
    }
    let mut other_target = event.clone();
    other_target["target"] = "foreign".into();
    assert!(!has_listening_event(&other_target.to_string(), address));
    let mut other_level = event.clone();
    other_level["level"] = "WARN".into();
    assert!(!has_listening_event(&other_level.to_string(), address));
    assert!(!has_listening_event(
        "listening addr=127.0.0.1:12345\n{partial-json",
        address
    ));
}

#[cfg(unix)]
#[test]
fn foreign_listener_and_cached_marker_cannot_report_an_owned_child_bound() {
    let root = canonical_tempdir().expect("foreign listener fixture");
    let marker = root.path().join("hiqlite/activation.json");
    std::fs::create_dir_all(marker.parent().expect("marker parent")).expect("cached target");
    std::fs::write(&marker, b"cached previous boot marker").expect("cached marker");
    let foreign = TcpListener::bind("127.0.0.1:0").expect("real foreign listener");
    let address = foreign.local_addr().expect("foreign address");
    assert!(TcpStream::connect(address).is_ok());

    let sentinel = format!("fixture child alive without binding {address}");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            r#"printf '%s\n' "$CQ_CAPTURE_SENTINEL" >&2; exec sleep 60"#,
        ])
        .env("CQ_CAPTURE_SENTINEL", &sentinel);
    let mut child = Daemon::spawn(command);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !child
        .captured_logs()
        .expect("child logs")
        .contains(&sentinel)
    {
        assert!(!child.owned_bind_probe(address).expect("live child status"));
        assert!(
            Instant::now() < deadline,
            "fixture child did not write its sentinel"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.is_file());
    assert!(!child.owned_bind_probe(address).expect("live child status"));
    child.0.kill().expect("stop owned fixture child");
    child.0.wait().expect("wait for owned fixture child");
    let refusal = child
        .owned_bind_probe(address)
        .expect_err("exited child is fatal");
    assert!(
        refusal.contains("exited before owned HTTP bind"),
        "{refusal}"
    );
    assert!(
        refusal.contains(&sentinel),
        "lost child diagnostics: {refusal}"
    );
}

async fn wait_for_ready(daemon: &mut Daemon, port: u16) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = daemon.0.try_wait().expect("daemon status") {
            panic!("daemon exited before readiness: {status}");
        }
        let last = match client
            .get(format!("http://127.0.0.1:{port}/readyz"))
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if status.is_success() {
                    return;
                }
                format!("{status} {}", body.trim())
            }
            Err(error) => error.to_string(),
        };
        assert!(
            Instant::now() < deadline,
            "daemon readiness timed out; last /readyz answer was {last}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn login_status(port: u16, username: &str, password: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/auth/login"))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await
        .expect("login request")
        .status()
}

async fn login_token(port: u16, username: &str, password: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/auth/login"))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await
        .expect("login request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response
        .json::<serde_json::Value>()
        .await
        .expect("login response")
        .get("token")
        .and_then(serde_json::Value::as_str)
        .expect("login token")
        .to_owned()
}

#[test]
fn migration_quiescence_precedes_directory_cleanup_probes_and_http_bind() {
    let _process_test = process_test_guard();
    let root = canonical_tempdir().expect("activation process fixture");
    let data = root.path().join("data");
    std::fs::create_dir_all(data.join("sessions/live-session")).expect("session fixture");
    std::fs::write(
        data.join("sessions/live-session/owned-by-previous-boot"),
        b"preserve until migration completes",
    )
    .expect("session sentinel");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source"));

    let config_path = root.path().join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{server_port}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n",
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("activation config");

    let output = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ])
        .env("PLURX_CLUSTER_ACTIVATION_FAILPOINT", "after-quiescence")
        .output()
        .expect("run injected daemon");
    assert_eq!(output.status.code(), Some(86));
    assert!(String::from_utf8_lossy(&output.stderr).contains("rollback command: plurxd run"));
    assert!(
        data.join("sessions/live-session/owned-by-previous-boot")
            .exists(),
        "create_dirs session cleanup must not run before migration completes"
    );

    let listener = TcpListener::bind(("127.0.0.1", server_port))
        .expect("HTTP port must not have been bound before migration completed");
    drop(listener);
}

/// Signal registration is a pre-reachability invariant, not a timing bet.
///
/// The daemon raises SIGTERM itself immediately after binding its listener and
/// before `serve` can poll the graceful-shutdown future. Lazy registration dies
/// from the signal's default action; eager registration buffers it and exits 0
/// after the normal drain.
#[cfg(unix)]
#[test]
fn sigterm_is_registered_before_the_listener_can_become_reachable() {
    let root = canonical_tempdir().expect("shutdown registration fixture");
    let run = run_to_completion_with_failpoint(root.path(), "after-listener-bind");

    assert!(
        run.status.success(),
        "SIGTERM reached its default action instead of the graceful drain: {}; logs: {}",
        run.status,
        run.logs
    );
    // Exit 0 alone would also be produced by a failpoint that never fired, or
    // by any early startup return that happens to be clean. Requiring the
    // drain log is what keeps this a proof that the signal was received and
    // handled rather than a proof that the process ended tidily.
    assert!(
        run.logs.contains("shutdown signal received, draining"),
        "daemon exited 0 without draining on the injected SIGTERM; logs: {}",
        run.logs
    );
}

/// Registering a signal is not the same as answering one.
///
/// The daemon raises SIGTERM the moment store activation returns, which is
/// before `boot` runs and long before anything can bind. Installing the streams
/// early only keeps the signal off its default action; nothing polls them until
/// `serve` first polls the drain, so on its own the signal sits buffered while
/// the daemon probes hardware and binds a listener the operator has already
/// asked to go away. `log_startup` is the first thing `boot` does, so `plurxd
/// starting` in the log is exactly the evidence that startup ran on regardless.
#[cfg(unix)]
#[test]
fn a_signal_during_startup_stops_the_daemon_before_it_boots() {
    let root = canonical_tempdir().expect("startup shutdown fixture");
    let run = run_to_completion_with_failpoint(root.path(), "after-store-activation");

    assert!(
        run.status.success(),
        "daemon did not exit cleanly on a signal received during startup: {}; logs: {}",
        run.status,
        run.logs
    );
    assert!(
        run.logs.contains("shutdown signal received during"),
        "daemon exited 0 without recording the startup-window signal; logs: {}",
        run.logs
    );
    assert!(
        !run.logs.contains("plurxd starting"),
        "daemon booted and served despite a signal received before boot; logs: {}",
        run.logs
    );
}

/// Run the shipped daemon under a shutdown failpoint until it exits on its own.
#[cfg(unix)]
fn run_to_completion_with_failpoint(root: &std::path::Path, failpoint: &str) -> BoundedRun {
    let _process_test = process_test_guard();
    let data = root.join("data");
    std::fs::create_dir_all(&data).expect("data directory");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source"));

    let config_path = root.join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n",
            server_port,
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("shutdown failpoint config");

    let child = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ])
        .env("PLURX_SHUTDOWN_REGISTRATION_FAILPOINT", failpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run shutdown failpoint regression");

    wait_bounded(child, Duration::from_secs(60))
}

/// M2 treats configured cluster hosts as future M3 membership input.
///
/// Use documentation-only non-local addresses so this shipped-binary test
/// exits at the injected post-start boundary only when both one-voter
/// listeners replace those hosts with loopback. Preserving either configured
/// host makes voter startup fail before the failpoint can fire.
#[test]
fn m2_ignores_explicit_non_loopback_cluster_listener_hosts() {
    let _process_test = process_test_guard();
    let root = canonical_tempdir().expect("listener host fixture");
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).expect("data directory");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source"));

    let config_path = root.path().join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"192.0.2.40:{}\"\n\
             api_bind = \"198.51.100.40:{}\"\n\
             advertise_host = \"203.0.113.40\"\n",
            server_port,
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("listener host config");

    let output = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ])
        .env("PLURX_CLUSTER_ACTIVATION_FAILPOINT", "after-incoming")
        .output()
        .expect("run listener host regression");
    assert_eq!(
        output.status.code(),
        Some(86),
        "configured M3 hosts reached M2 listener startup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Store-backed maintenance must reach TLS through the real binary, while the
/// unsafe console reset is refused before it can become a second writer.
///
/// `rustls` panics rather than erroring when a process reaches TLS with no
/// process-level provider, and `plurxd run` only ever got one as a side effect
/// of `hiqlite::start_node`, which these commands never call. So both aborted on
/// every activated node while the library-level contract test passed, because
/// that harness installs a provider of its own. Only the shipped binary can tell
/// the two apart, which is why this drives `CARGO_BIN_EXE_plurxd`.
///
/// Asserting on the *absence* of the panic for metadata keeps that half about
/// the regression: reaching a reported error means TLS was negotiated.
#[test]
fn maintenance_commands_obey_their_activated_store_boundaries() {
    let _process_test = process_test_guard();
    let root = canonical_tempdir().expect("maintenance TLS fixture");
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).expect("data directory");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source"));

    let config_path = root.path().join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{server_port}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n",
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("maintenance config");

    let mut command = Command::new(env!("CARGO_BIN_EXE_plurxd"));
    command.args([
        "--config",
        config_path.to_str().expect("config path"),
        "run",
    ]);
    let mut daemon = Daemon::spawn(command);
    wait_for_bind(&mut daemon, server_port);
    assert!(data.join("hiqlite/activation.json").is_file());

    let reset = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "reset-password",
            "owner",
            "--password",
            "a-new-password",
        ])
        .output()
        .expect("run reset refusal");
    let reset_stderr = String::from_utf8_lossy(&reset.stderr);
    assert!(!reset.status.success());
    assert!(
        reset_stderr.contains("direct replicated Store write cannot invalidate"),
        "unsafe reset did not explain its refusal: {reset_stderr}"
    );
    assert!(!reset_stderr.contains("CryptoProvider"), "{reset_stderr}");

    let refresh = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "refresh-metadata",
        ])
        .output()
        .expect("run metadata maintenance command");
    let refresh_stderr = String::from_utf8_lossy(&refresh.stderr);
    assert!(
        !refresh_stderr.contains("CryptoProvider"),
        "refresh-metadata aborted before TLS: {refresh_stderr}"
    );
    assert!(!refresh_stderr.contains("panicked"), "{refresh_stderr}");
    assert!(
        !refresh_stderr.contains("is not activated"),
        "refresh-metadata refused an activated directory: {refresh_stderr}"
    );

    daemon.stop();
}

#[test]
fn subsequent_plurxd_run_reopens_the_completed_replicated_target() {
    let _process_test = process_test_guard();
    let root = canonical_tempdir().expect("daemon activation fixture");
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).expect("data directory");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source"));
    let source_before = std::fs::read(data.join("plurx.db")).expect("source bytes");

    let config_path = root.path().join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{server_port}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"127.0.0.1\"\n",
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("daemon config");

    let start = |activation_failpoint: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plurxd"));
        command.args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ]);
        if let Some(value) = activation_failpoint {
            command.env("PLURX_CLUSTER_ACTIVATION_FAILPOINT", value);
        }
        Daemon::spawn(command)
    };

    let mut first = start(None);
    wait_for_bind(&mut first, server_port);
    assert!(data.join("hiqlite/activation.json").is_file());
    let contender = Command::new(env!("CARGO_BIN_EXE_plurxd"))
        .args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ])
        .output()
        .expect("run competing daemon");
    assert!(!contender.status.success());
    // The genuine double-start, and the only condition entitled to this text.
    // Since #374 the contender waits out a departing owner before refusing, so
    // it reaches this only because `first` is still running and still holding
    // the lock. It must name that live process rather than blame its own.
    let refusal = String::from_utf8_lossy(&contender.stderr);
    assert!(
        refusal.contains("another plurxd process already owns the data directory"),
        "a second live daemon must still be refused as one: {refusal}"
    );
    assert!(
        !refusal.contains("still locked inside this plurxd process"),
        "a real second daemon was reported as this process's own leaked handle: {refusal}"
    );
    first.stop();

    // The variable is meaningful only while activation is pending. A stale or
    // misspelled value in a service definition must not take an already-active
    // server offline when no injection boundary can be reached.
    let mut second = start(Some("not-a-real-failpoint"));
    wait_for_bind(&mut second, server_port);
    assert!(data.join("hiqlite/activation.json").is_file());
    second.stop();

    assert_eq!(
        std::fs::read(data.join("plurx.db")).expect("retained source"),
        source_before,
        "subsequent replicated boots must not reopen or migrate retained SQLite"
    );
}

/// A one-voter node must recover current acknowledged state after SIGKILL.
///
/// Hiqlite treats a stale state-machine lock as proof that the SQLite state
/// machine may be inconsistent with its Raft log. The `auto-heal` path removes
/// and rebuilds that state machine from the retained local log/snapshot; merely
/// unlinking the lock would skip the reconstruction. The password change is
/// acknowledged after activation, so reading it after restart proves the
/// rebuild preserved state newer than the retained pre-activation SQLite file.
///
/// Both advertised forms are covered. A loopback literal and a real hostname
/// took different startup paths for as long as the repair triggered on
/// "the committed address looks like loopback" rather than on "it differs from
/// the configured one": the loopback form rebuilt on every boot and so
/// recovered by accident, while a node advertising a routable name settled
/// after its first restart and then had no repair left to run.
#[cfg(unix)]
#[tokio::test]
async fn activated_one_voter_rebuilds_current_state_after_sigkill() {
    let _process_test = PROCESS_TEST_LOCK.lock().await;
    for advertise_host in ["127.0.0.1", "localhost"] {
        sigkill_recovery_preserves_acknowledged_writes(advertise_host).await;
    }
}

#[cfg(unix)]
async fn sigkill_recovery_preserves_acknowledged_writes(advertise_host: &str) {
    let root = canonical_tempdir().expect("SIGKILL recovery fixture");
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).expect("data directory");
    let source = SqliteStore::open(&data.join("plurx.db")).expect("legacy SQLite source");
    let user = source
        .create_user(
            "recovery-owner",
            &hash_password("password-before-kill").expect("initial password hash"),
            true,
        )
        .await
        .expect("seed recovery user");
    let user_id = user.id;
    drop(source);

    let config_path = root.path().join("plurx.toml");
    let [server_port, raft_port, api_port] = free_ports();
    std::fs::write(
        &config_path,
        format!(
            "[server]\n\
             bind = \"127.0.0.1:{server_port}\"\n\
             [storage]\n\
             data_dir = \"{}\"\n\
             [cluster]\n\
             raft_bind = \"127.0.0.1:{}\"\n\
             api_bind = \"127.0.0.1:{}\"\n\
             advertise_host = \"{advertise_host}\"\n",
            toml_string(&data),
            raft_port,
            api_port,
        ),
    )
    .expect("SIGKILL recovery config");

    let start = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plurxd"));
        command.args([
            "--config",
            config_path.to_str().expect("config path"),
            "run",
        ]);
        Daemon::spawn(command)
    };

    let mut first = start();
    wait_for_bind(&mut first, server_port);
    wait_for_ready(&mut first, server_port).await;
    let token = login_token(server_port, "recovery-owner", "password-before-kill").await;
    let update = reqwest::Client::new()
        .put(format!(
            "http://127.0.0.1:{server_port}/api/v1/users/{user_id}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "password": "password-after-kill" }))
        .send()
        .await
        .expect("acknowledged daemon-owned password update");
    assert_eq!(update.status(), reqwest::StatusCode::OK);
    assert_eq!(
        login_status(server_port, "recovery-owner", "password-after-kill").await,
        reqwest::StatusCode::OK
    );

    let status = first.kill_ungracefully();
    assert!(!status.success(), "SIGKILL must be ungraceful: {status}");
    assert!(
        data.join("hiqlite/state_machine/lock").is_file(),
        "the regression requires Hiqlite's stale-lock recovery path"
    );

    let mut recovered = start();
    wait_for_bind(&mut recovered, server_port);
    wait_for_ready(&mut recovered, server_port).await;
    assert_eq!(
        login_status(server_port, "recovery-owner", "password-after-kill").await,
        reqwest::StatusCode::OK,
        "the acknowledged post-activation update must survive log replay"
    );
    assert_eq!(
        login_status(server_port, "recovery-owner", "password-before-kill").await,
        reqwest::StatusCode::UNAUTHORIZED,
        "recovery must not fall back to the retained pre-activation source"
    );
    recovered.stop();
}
