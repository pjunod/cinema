//! `plurx-tray-smoke-v1` receipt written by `--smoke-receipt` (task comment
//! 53 on t_e544e56f). The native qualification harness reads it alongside
//! its own independent `plurx-control status` before and after the run; the
//! receipt alone does not prove quit-versus-stop semantics.
use crate::model::{self, Action, Entry, Kind, Snapshot};
use crate::protocol::{ControlFailure, ServiceState};
use crate::session::{Eligibility, SessionFacts};
use serde::Serialize;
use std::io::Write;
use std::path::Path;

pub const SCHEMA: &str = "plurx-tray-smoke-v1";
pub const MAX_RECEIPT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeAction {
    Quit,
    Stop,
    Restart,
}

impl SmokeAction {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "quit" => Some(Self::Quit),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
    pub fn action(self) -> Option<Action> {
        match self {
            Self::Quit => None,
            Self::Stop => Some(Action::Stop),
            Self::Restart => Some(Action::Restart),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LinuxSession {
    pub eligible: bool,
    pub reason: String,
    pub session_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub remote: Option<bool>,
    pub active: Option<bool>,
    pub seat: Option<String>,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
}

impl LinuxSession {
    pub fn from(facts: &SessionFacts, eligibility: &Eligibility) -> Self {
        let s = facts.session.as_ref();
        Self {
            eligible: eligibility.eligible,
            reason: eligibility.reason.clone(),
            session_id: s.map(|s| s.id.clone()),
            kind: s.map(|s| s.kind.clone()),
            remote: s.map(|s| s.remote),
            active: s.map(|s| s.active),
            seat: s.map(|s| s.seat.clone()).filter(|v| !v.is_empty()),
            display: facts.display.clone(),
            wayland_display: facts.wayland_display.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Tray {
    pub watcher_present: bool,
    pub host_registered: Option<bool>,
    pub item_registered: bool,
    pub item_service: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Window {
    pub present: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Menu {
    pub titles: Vec<String>,
    pub enabled: Vec<String>,
}

impl Menu {
    pub fn from(entries: &[Entry]) -> Self {
        Self {
            titles: entries
                .iter()
                .filter(|e| e.kind != Kind::Separator)
                .map(|e| e.title.clone())
                .collect(),
            enabled: entries
                .iter()
                .filter(|e| e.kind == Kind::Command && e.enabled)
                .map(|e| e.title.clone())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FirstStatus {
    pub ok: bool,
    pub service_state: Option<String>,
    pub version: Option<String>,
    pub error_code: Option<String>,
    pub failure: Option<String>,
}

impl FirstStatus {
    pub fn from(snapshot: &Snapshot) -> Self {
        match (&snapshot.status, &snapshot.status_failure) {
            (Some(s), None) => Self {
                ok: true,
                service_state: Some(s.service_state.wire().into()),
                version: s.version.clone(),
                error_code: None,
                failure: None,
            },
            (_, failure) => Self {
                ok: false,
                service_state: None,
                version: None,
                error_code: failure
                    .as_ref()
                    .and_then(ControlFailure::code)
                    .and_then(|c| serde_json::to_value(c).ok())
                    .and_then(|v| v.as_str().map(str::to_string)),
                failure: failure.as_ref().map(ControlFailure::message),
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Controller {
    pub path: String,
    pub first_status: FirstStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub name: SmokeAction,
    pub ok: Option<bool>,
    pub service_state_after: Option<String>,
    pub error: Option<String>,
    pub ui_alive_after: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Quit {
    pub via: &'static str,
    pub controller_calls_during_quit: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Invariants {
    pub unknown_not_stopped: bool,
    pub start_only_when_stopped: bool,
    pub quit_sent_no_server_command: bool,
    pub ui_alive_after_stop: Option<bool>,
}

impl Invariants {
    pub fn all_hold(&self) -> bool {
        self.unknown_not_stopped
            && self.start_only_when_stopped
            && self.quit_sent_no_server_command
            && self.ui_alive_after_stop != Some(false)
    }
}

/// The menu invariants every observed snapshot must satisfy.
pub fn menu_invariants(snapshot: &Snapshot) -> (bool, bool) {
    let entries = model::build(snapshot);
    let start = model::find(&entries, &Action::Start).is_some_and(|e| e.enabled);
    let state = snapshot.service_state();
    let unknown_not_stopped = snapshot.status_failure.is_none()
        || (state == ServiceState::Unknown && entries[0].title.ends_with("Unknown") && !start);
    let start_only_when_stopped = !start || state == ServiceState::Stopped;
    (unknown_not_stopped, start_only_when_stopped)
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt<S: Serialize, T: Serialize> {
    pub schema: &'static str,
    pub platform: &'static str,
    pub app_version: String,
    pub app_build: String,
    pub arch: &'static str,
    pub ui_pid: u32,
    pub uid: u32,
    pub session: S,
    pub tray: T,
    pub window: Option<Window>,
    pub menu: Menu,
    pub controller: Controller,
    pub action: ActionResult,
    pub quit: Quit,
    pub invariants: Invariants,
    pub elapsed_ms: u128,
    /// Set when the run did not reach its end (timeout, startup failure).
    pub incomplete: Option<String>,
}

/// Writes atomically: a sibling temporary file, fsync, rename, then a
/// directory fsync. Refuses a relative path and a receipt over 64 KiB.
pub fn write_atomic(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, m.to_string());
    if !path.is_absolute() {
        return Err(invalid("receipt path must be absolute"));
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| invalid(&e.to_string()))?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid("receipt exceeds 64 KiB"));
    }
    let dir = path
        .parent()
        .ok_or_else(|| invalid("receipt has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("receipt has no file name"))?
        .to_string_lossy();
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    std::fs::File::open(dir)?.sync_all()
}
