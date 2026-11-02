//! The tray menu and control window as data. The StatusNotifierItem menu and
//! the GTK window both render exactly these entries, so one set of tests
//! covers both. Layout follows DESKTOP-TRAY-IMPLEMENTATION.md §4 with the
//! Linux labels: the server checkbox is the system service's boot policy and
//! the tray checkbox is this user's login preference; they are independent.
use crate::command::is_version;
use crate::protocol::{
    browser_url, ControlFailure, ErrorCode, ServiceState, StartupState, StatusV1, UpdateState,
    UpdateV1,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ShowWindow,
    OpenBrowser,
    ViewLogs,
    Start,
    Stop,
    Restart,
    ToggleServerStartup,
    ToggleTrayLogin,
    CheckForUpdates,
    OpenReleaseNotes(String),
    Upgrade(String),
    ShowUpgradeProgress,
    CancelUpgrade,
    RetryStatus,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    On,
    Off,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Header,
    Info,
    /// Explanatory prose belongs in the wrapping control window, not the panel menu.
    Detail,
    Command,
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub title: String,
    pub action: Option<Action>,
    pub enabled: bool,
    pub check: Option<Check>,
}

impl Entry {
    fn header(title: String) -> Self {
        Self {
            kind: Kind::Header,
            title,
            action: None,
            enabled: false,
            check: None,
        }
    }
    fn info(title: impl Into<String>) -> Self {
        Self {
            kind: Kind::Info,
            title: title.into(),
            action: None,
            enabled: false,
            check: None,
        }
    }
    fn command(title: impl Into<String>, action: Action, enabled: bool) -> Self {
        Self {
            kind: Kind::Command,
            title: title.into(),
            action: Some(action),
            enabled,
            check: None,
        }
    }
    fn checked(title: String, action: Action, enabled: bool, check: Check) -> Self {
        Self {
            check: Some(check),
            ..Self::command(title, action, enabled)
        }
    }
    const SEPARATOR: Self = Self {
        kind: Kind::Separator,
        title: String::new(),
        action: None,
        enabled: false,
        check: None,
    };
}

/// Whether a StatusNotifier host is showing tray items in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayHost {
    Unknown,
    Present,
    Absent,
}

/// Lifecycle management through the controller is available only on
/// systemd hosts; elsewhere the web app and the operator's own supervisor
/// remain the management path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Management {
    Supported,
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub status: Option<StatusV1>,
    pub status_failure: Option<ControlFailure>,
    pub busy: Option<Action>,
    pub update: Option<UpdateV1>,
    pub last_error: Option<String>,
    pub app_version: String,
    pub management: Management,
    pub tray_host: TrayHost,
}

impl Snapshot {
    pub fn new(app_version: impl Into<String>, management: Management) -> Self {
        Self {
            status: None,
            status_failure: None,
            busy: None,
            update: None,
            last_error: None,
            app_version: app_version.into(),
            management,
            tray_host: TrayHost::Unknown,
        }
    }

    /// A failed status read is Unknown, never Stopped.
    pub fn service_state(&self) -> ServiceState {
        match (&self.status, &self.status_failure) {
            (Some(s), None) => s.service_state,
            _ => ServiceState::Unknown,
        }
    }

    /// The transaction the controller's worker currently owns or that needs
    /// recovery, from the latest update poll or the status record.
    pub fn active_transaction(&self) -> Option<&str> {
        if let Some(u) = &self.update {
            if u.state.is_in_flight() || u.state == UpdateState::RecoveryRequired {
                if let Some(t) = &u.transaction_id {
                    return Some(t);
                }
            }
            if u.state.is_terminal() && u.state != UpdateState::RecoveryRequired {
                // A finished transaction no longer holds the controls even if
                // the status record still names it until the next poll.
                if u.transaction_id == self.status.as_ref().and_then(|s| s.transaction_id.clone()) {
                    return None;
                }
            }
        }
        self.status.as_ref()?.transaction_id.as_deref()
    }

    fn upgrade_in_flight(&self) -> bool {
        self.update.as_ref().is_some_and(|u| u.state.is_in_flight())
    }
}

pub fn progress_title(action: &Action) -> &'static str {
    match action {
        Action::Start => "Starting the server…",
        Action::Stop => "Stopping the server (waiting for a clean exit)…",
        Action::Restart => "Restarting the server…",
        Action::ToggleServerStartup => "Changing boot startup…",
        Action::ToggleTrayLogin => "Changing the login preference…",
        Action::CheckForUpdates => "Checking for updates…",
        Action::Upgrade(_) => "Preparing the upgrade…",
        _ => "Working…",
    }
}

/// A check mark is shown only for an observed effective state.
pub fn check_for(state: StartupState) -> Check {
    match state {
        StartupState::Enabled => Check::On,
        StartupState::Disabled | StartupState::Masked => Check::Off,
        _ => Check::Mixed,
    }
}

fn startup_title(base: &str, state: StartupState) -> String {
    match state.qualifier() {
        Some(q) => format!("{base} ({q})"),
        None => base.to_string(),
    }
}

pub fn build(s: &Snapshot) -> Vec<Entry> {
    let state = s.service_state();
    let idle = s.busy.is_none();
    let managed = s.management == Management::Supported;
    let transaction = s.active_transaction().is_some();
    let mut out = vec![Entry::header(format!("plurx — {}", state.title()))];

    out.push(Entry::info(format!(
        "Version {}",
        s.status
            .as_ref()
            .and_then(|st| st.version.as_deref())
            .unwrap_or("unknown")
    )));
    if let Some(busy) = &s.busy {
        out.push(Entry::info(progress_title(busy)));
    }
    if let Management::Unsupported(why) = &s.management {
        out.push(Entry::info(format!(
            "Automatic management is unavailable here: {why}. Use the web app and the service's own supervisor."
        )));
    }
    match (&s.last_error, &s.status_failure) {
        (Some(e), _) => out.push(Entry::info(e.clone())),
        (None, Some(f))
            if matches!(
                f.code(),
                Some(ErrorCode::Unsupported | ErrorCode::InvalidInstallation)
            ) =>
        {
            out.push(Entry::info(format!(
                "{} Use the web app and the service's own supervisor.",
                f.message()
            )));
        }
        (None, Some(f)) => out.push(Entry::info(f.message())),
        (None, None) => {}
    }
    if s.status.is_none() && s.status_failure.is_some() {
        out.push(Entry::command("Retry", Action::RetryStatus, idle));
    }

    out.push(Entry::command(
        "Open control window…",
        Action::ShowWindow,
        true,
    ));
    let has_origin = s
        .status
        .as_ref()
        .is_some_and(|st| browser_url(&st.browser_origin).is_some());
    out.push(Entry::command(
        "Open plurx…",
        Action::OpenBrowser,
        has_origin && state == ServiceState::Running,
    ));
    out.push(Entry::command(
        "View logs…",
        Action::ViewLogs,
        s.status.is_some(),
    ));
    out.push(Entry::SEPARATOR);

    // Start only for an observed Stopped server: starting/unknown is never an
    // invitation to start a second one.
    let lifecycle = idle && managed && !transaction && s.status.is_some();
    let running = matches!(state, ServiceState::Running | ServiceState::Unhealthy);
    out.push(Entry::command(
        "Start",
        Action::Start,
        lifecycle && state == ServiceState::Stopped,
    ));
    out.push(Entry::command("Stop…", Action::Stop, lifecycle && running));
    out.push(Entry::command(
        "Restart…",
        Action::Restart,
        lifecycle && running,
    ));

    let toggleable =
        |st: StartupState| matches!(st, StartupState::Enabled | StartupState::Disabled);
    match &s.status {
        Some(st) => {
            out.push(Entry::checked(
                startup_title("Start server at boot", st.server_startup.state),
                Action::ToggleServerStartup,
                idle && managed && toggleable(st.server_startup.state),
                check_for(st.server_startup.state),
            ));
            if let Some(detail) = &st.server_startup.detail {
                if !detail.is_empty() {
                    out.push(Entry {
                        kind: Kind::Detail,
                        ..Entry::info(detail.clone())
                    });
                }
            }
            out.push(Entry::checked(
                startup_title("Show tray app at login", st.tray_startup.state),
                Action::ToggleTrayLogin,
                idle && toggleable(st.tray_startup.state),
                check_for(st.tray_startup.state),
            ));
        }
        None => {
            out.push(Entry::checked(
                "Start server at boot".into(),
                Action::ToggleServerStartup,
                false,
                Check::Mixed,
            ));
            out.push(Entry::checked(
                "Show tray app at login".into(),
                Action::ToggleTrayLogin,
                false,
                Check::Mixed,
            ));
        }
    }
    out.push(Entry::SEPARATOR);
    out.extend(update_entries(s, idle && managed));
    out.push(Entry::SEPARATOR);
    out.push(Entry::command("Quit tray app", Action::Quit, true));
    out
}

fn update_entries(s: &Snapshot, idle: bool) -> Vec<Entry> {
    let mut out = Vec::new();
    if s.active_transaction().is_some() {
        let title = match &s.update {
            Some(u) if u.is_server_operation() => format!("Server operation: {}…", u.title()),
            Some(u) => format!("Upgrade: {}…", u.title()),
            None => "Upgrade in progress…".into(),
        };
        out.push(Entry::command(title, Action::ShowUpgradeProgress, true));
        if s.update
            .as_ref()
            .is_some_and(|u| !u.is_server_operation() && u.state.is_cancellable())
        {
            out.push(Entry::command(
                "Cancel upgrade",
                Action::CancelUpgrade,
                true,
            ));
        }
        return out;
    }
    if let Some(u) = &s.update {
        match u.state {
            UpdateState::Available => out.push(Entry::info(format!(
                "Update available: {}",
                u.version.as_deref().unwrap_or("unknown version")
            ))),
            UpdateState::Complete | UpdateState::Cancelled | UpdateState::Failed => {
                out.push(Entry::command(
                    if u.is_server_operation() {
                        format!("Last operation: {}…", u.title())
                    } else {
                        format!("Last upgrade: {}…", u.title())
                    },
                    Action::ShowUpgradeProgress,
                    true,
                ))
            }
            UpdateState::Idle if u.version.is_some() => {
                out.push(Entry::info("No update available"))
            }
            _ => {}
        }
    }
    out.push(Entry::command(
        "Check for updates…",
        Action::CheckForUpdates,
        idle && !s.upgrade_in_flight(),
    ));
    if let Some(u) = &s.update {
        if u.state == UpdateState::Available {
            if let Some(v) = u.version.as_deref().filter(|v| is_version(v)) {
                out.push(Entry::command(
                    format!("Release notes for {v}…"),
                    Action::OpenReleaseNotes(v.into()),
                    true,
                ));
                out.push(Entry::command(
                    format!("Upgrade to {v}…"),
                    Action::Upgrade(v.into()),
                    idle && s.status.is_some(),
                ));
            }
        }
    }
    out
}

/// §4: five seconds while a menu or window is visible, thirty otherwise, two
/// while a start or upgrade is being followed. None: nothing visible needs
/// polling (the tray host is gone and no window is open).
pub fn poll_interval(s: &Snapshot, visible: bool) -> Option<u64> {
    if s.upgrade_in_flight() || s.service_state() == ServiceState::Starting {
        return Some(2);
    }
    if visible {
        return Some(5);
    }
    if s.tray_host == TrayHost::Absent {
        return None;
    }
    Some(30)
}

pub fn find<'a>(entries: &'a [Entry], action: &Action) -> Option<&'a Entry> {
    entries.iter().find(|e| e.action.as_ref() == Some(action))
}

/// The explanatory line for the upgrade view, including recovery guidance.
pub fn upgrade_explanation(update: Option<&UpdateV1>) -> String {
    let Some(u) = update else {
        return "No upgrade has been started.".into();
    };
    if u.is_server_operation() {
        return u.title().to_owned();
    }
    let detail = u
        .detail
        .as_deref()
        .map(|d| format!(" {d}"))
        .unwrap_or_default();
    let version = u.version.as_deref().unwrap_or("the selected version");
    let text = match u.state {
        UpdateState::Prepared => format!("plurx {version} is downloaded and verified."),
        UpdateState::WaitingForIdle => "The server finishes current playback and recordings before it stops. You can still cancel.".into(),
        UpdateState::ShutdownCommitted | UpdateState::Installing | UpdateState::Restarting => {
            format!("The upgrade to {version} is running and continues even if you quit this app.")
        }
        UpdateState::Complete => format!("plurx {version} is installed and running."),
        UpdateState::Cancelled => "The upgrade was cancelled; the installed version is unchanged.".into(),
        UpdateState::Failed => "The upgrade did not complete. The previous version remains or was restored.".into(),
        UpdateState::RecoveryRequired => "The upgrade was interrupted and needs recovery before the server can start. Recovery runs before the next server start even without this app; if it cannot finish, run `plurx-control updates status` and follow the recovery instructions.".into(),
        other => format!("{}.", other.title()),
    };
    text + &detail
}

/// Fraction of the visible upgrade phases reached, for a progress bar.
pub fn upgrade_progress(state: UpdateState) -> Option<f64> {
    const PHASES: [UpdateState; 6] = [
        UpdateState::Prepared,
        UpdateState::WaitingForIdle,
        UpdateState::ShutdownCommitted,
        UpdateState::Installing,
        UpdateState::Restarting,
        UpdateState::Complete,
    ];
    PHASES
        .iter()
        .position(|p| *p == state)
        .map(|i| (i + 1) as f64 / PHASES.len() as f64)
}

/// Case-insensitive substring filter for the log viewer.
pub fn filter_logs<'a>(lines: &'a [String], query: &str) -> Vec<&'a str> {
    let needle = query.trim().to_lowercase();
    lines
        .iter()
        .map(String::as_str)
        .filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle))
        .collect()
}
