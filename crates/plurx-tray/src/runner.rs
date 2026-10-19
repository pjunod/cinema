//! Runs the installed `plurx-control`: one argument vector per call, a
//! cleaned environment, bounded stdout (exactly one JSON response) and
//! bounded stderr (diagnostics shown only on failure). `run` blocks, so the
//! UI calls it from worker threads only; nothing here touches GTK or D-Bus.
use crate::command::Command;
use crate::protocol::{decode_response, ControlFailure, ControlResult, MAX_RESPONSE_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The controller the server package installs. The desktop package depends
/// on it; the UI never searches PATH.
pub const DEFAULT_CONTROLLER: &str = "/usr/bin/plurx-control";
pub const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

pub trait Control: Send + Sync {
    fn run(&self, command: &Command) -> Result<ControlResult, ControlFailure>;
}

#[derive(Debug, Clone)]
pub struct Runner {
    pub executable: PathBuf,
    pub installation: Option<PathBuf>,
}

impl Runner {
    pub fn new(executable: impl Into<PathBuf>, installation: Option<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            installation,
        }
    }

    pub fn argv(&self, command: &Command) -> Result<Vec<String>, ControlFailure> {
        let mut argv = Vec::new();
        if let Some(record) = &self.installation {
            argv.push("--installation".to_string());
            argv.push(record.to_string_lossy().into_owned());
        }
        let args = command
            .args()
            .map_err(|e| ControlFailure::Protocol(format!("refused invalid argument {e:?}")))?;
        argv.extend(args);
        Ok(argv)
    }
}

/// What the controller may inherit. It needs the user's identity and the
/// session bus (polkit's agent and systemd's D-Bus authorization are
/// per-session); it must not inherit proxies or an injected library path.
pub fn controller_environment(
    parent: impl Iterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    const KEEP: [&str; 8] = [
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_SESSION_ID",
    ];
    let mut kept: Vec<(String, String)> =
        parent.filter(|(k, _)| KEEP.contains(&k.as_str())).collect();
    kept.push(("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()));
    kept.sort();
    kept
}

impl Control for Runner {
    fn run(&self, command: &Command) -> Result<ControlResult, ControlFailure> {
        let argv = self.argv(command)?;
        execute(
            &self.executable,
            &argv,
            Duration::from_secs(command.timeout_secs()),
            !command.is_mutation(),
        )
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the desktop companion is not a daemon child; it starts only the fixed controller with a bounded argv"
)]
pub fn execute(
    executable: &Path,
    argv: &[String],
    timeout: Duration,
    kill_on_timeout: bool,
) -> Result<ControlResult, ControlFailure> {
    let mut child = std::process::Command::new(executable)
        .args(argv)
        .env_clear()
        .envs(controller_environment(std::env::vars()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ControlFailure::Launch(format!("{}: {e}", executable.display())))?;
    let stdout = bounded(child.stdout.take(), MAX_RESPONSE_BYTES);
    let stderr = bounded(child.stderr.take(), MAX_DIAGNOSTIC_BYTES);

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                if kill_on_timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(ControlFailure::TimedOut(timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(ControlFailure::Launch(e.to_string())),
        }
    };
    let (out, overflow) = stdout.finish();
    let (err, _) = stderr.finish();
    let diagnostics = String::from_utf8_lossy(&err).into_owned();
    let code = status.code().unwrap_or(-1);
    if overflow {
        return Err(ControlFailure::OutputTooLarge);
    }
    match decode_response(&out) {
        // The contract pairs success with exit zero; anything else is a
        // broken controller, not a success to act on.
        Ok(result) if code == 0 => Ok(result),
        Ok(_) => Err(ControlFailure::Exited {
            status: code,
            diagnostics,
        }),
        Err(ControlFailure::Controller(e)) => Err(ControlFailure::Controller(e)),
        Err(other) if out.is_empty() || code != 0 => {
            let _ = other;
            Err(ControlFailure::Exited {
                status: code,
                diagnostics,
            })
        }
        Err(other) => Err(other),
    }
}

/// Drains a pipe on its own thread, keeping at most `limit` bytes while still
/// reading (so the child never blocks on a full pipe).
struct Bounded {
    state: Arc<Mutex<(Vec<u8>, bool)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn bounded<R: Read + Send + 'static>(reader: Option<R>, limit: usize) -> Bounded {
    let state = Arc::new(Mutex::new((Vec::new(), false)));
    let thread = reader.map(|mut reader| {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 16 * 1024];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let Ok(mut s) = state.lock() else { break };
                let room = limit.saturating_sub(s.0.len());
                if n > room {
                    s.1 = true;
                    s.0.extend_from_slice(&chunk[..room]);
                } else {
                    s.0.extend_from_slice(&chunk[..n]);
                }
            }
        })
    });
    Bounded { state, thread }
}

impl Bounded {
    /// After the child exited. A descendant that inherited the pipe could hold
    /// it open, so the wait is bounded and only bytes already read count.
    fn finish(mut self) -> (Vec<u8>, bool) {
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        match self.state.lock() {
            Ok(s) => (s.0.clone(), s.1),
            Err(_) => (Vec::new(), true),
        }
    }
}
