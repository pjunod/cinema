//! The tray's decisions, independent of GTK and D-Bus. User actions,
//! confirmations and finished controller calls go in; effects come out (run
//! this controller command on a worker, ask for confirmation, open a URL,
//! re-render). The Linux shell executes effects; tests assert on them. The
//! core never blocks, so the GTK main loop never waits on the controller.
use crate::command::{Choice, Command, MAX_LOG_LINES};
use crate::model::{self, Action, Management, Snapshot, TrayHost};
use crate::protocol::{
    browser_url, release_notes_url, ControlFailure, ControlResult, OperationKind, StartupState,
    UpdateState,
};

/// An explicitly opened control window remains available without a tray, but
/// indicator registration always needs both a proved local session and a host.
pub fn tray_host_for_session(
    eligibility: &crate::session::Eligibility,
    observed_host: TrayHost,
) -> TrayHost {
    if eligibility.eligible && observed_host == TrayHost::Present {
        TrayHost::Present
    } else {
        TrayHost::Absent
    }
}

/// Why a controller command was issued; returned with its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    Poll,
    PollUpdate(String),
    Lifecycle(OperationKind),
    ServerStartup,
    TrayStartup,
    Check,
    Prepare(String),
    Apply(String),
    Cancel(String),
    Logs,
}

/// A confirmation the user must give before an interruption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    Stop,
    Restart,
    Apply {
        version: String,
        transaction: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Run {
        purpose: Purpose,
        command: Command,
    },
    Confirm {
        pending: Pending,
        title: String,
        message: String,
        proceed: String,
    },
    OpenUrl(String),
    ShowLogs {
        lines: Vec<String>,
        truncated: bool,
    },
    ShowWindow,
    ShowUpgrade,
    ShowError(String),
    Render,
    Quit,
}

pub struct Core {
    pub snapshot: Snapshot,
    pub release_repository: String,
    polling: bool,
}

impl Core {
    pub fn new(app_version: &str, management: Management, release_repository: &str) -> Self {
        Self {
            snapshot: Snapshot::new(app_version, management),
            release_repository: release_repository.to_string(),
            polling: false,
        }
    }

    pub fn entries(&self) -> Vec<model::Entry> {
        model::build(&self.snapshot)
    }

    pub fn set_tray_host(&mut self, host: TrayHost) -> Vec<Effect> {
        let previous = self.snapshot.tray_host;
        self.snapshot.tray_host = host;
        let mut effects = vec![Effect::Render];
        if host == TrayHost::Absent && previous != TrayHost::Absent {
            effects.push(Effect::ShowWindow);
        }
        effects
    }

    /// One status poll; a poll already running absorbs the request.
    pub fn poll(&mut self) -> Vec<Effect> {
        if self.polling {
            return vec![];
        }
        self.polling = true;
        vec![Effect::Run {
            purpose: Purpose::Poll,
            command: Command::Status,
        }]
    }

    fn enabled(&self, action: &Action) -> bool {
        model::find(&self.entries(), action).is_some_and(|e| e.enabled)
    }

    fn begin(&mut self, action: Action, purpose: Purpose, command: Command) -> Vec<Effect> {
        self.snapshot.busy = Some(action);
        self.snapshot.last_error = None;
        vec![Effect::Render, Effect::Run { purpose, command }]
    }

    pub fn act(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::ShowWindow => {
                let mut effects = vec![Effect::ShowWindow];
                effects.extend(self.poll());
                effects
            }
            Action::ShowUpgradeProgress => vec![Effect::ShowUpgrade],
            Action::RetryStatus => self.poll(),
            // Quitting the companion never sends a server command.
            Action::Quit => vec![Effect::Quit],
            Action::OpenBrowser => match self
                .snapshot
                .status
                .as_ref()
                .and_then(|s| browser_url(&s.browser_origin))
            {
                Some(url) => vec![Effect::OpenUrl(url.to_string())],
                None => vec![Effect::ShowError(
                    "This installation has no usable browser address.".into(),
                )],
            },
            Action::OpenReleaseNotes(version) => {
                match release_notes_url(&self.release_repository, &version) {
                    Some(url) => vec![Effect::OpenUrl(url)],
                    None => vec![Effect::ShowError(
                        "Release notes are unavailable for this build.".into(),
                    )],
                }
            }
            Action::ViewLogs => vec![Effect::Run {
                purpose: Purpose::Logs,
                command: Command::Logs(MAX_LOG_LINES),
            }],
            Action::Start if self.enabled(&Action::Start) => self.begin(
                Action::Start,
                Purpose::Lifecycle(OperationKind::Start),
                Command::Start,
            ),
            Action::Stop if self.enabled(&Action::Stop) => vec![Effect::Confirm {
                pending: Pending::Stop,
                title: "Stop the plurx server?".into(),
                message: "Playback and recordings on this server end once they can stop safely. Boot startup is unchanged, and the tray stays available to start it again.".into(),
                proceed: "Stop".into(),
            }],
            Action::Restart if self.enabled(&Action::Restart) => vec![Effect::Confirm {
                pending: Pending::Restart,
                title: "Restart the plurx server?".into(),
                message: "Playback and recordings are interrupted while the server restarts. Startup settings are unchanged.".into(),
                proceed: "Restart".into(),
            }],
            Action::ToggleServerStartup if self.enabled(&Action::ToggleServerStartup) => {
                let Some(choice) = self.toggle_choice(|s| s.server_startup.state) else {
                    return vec![];
                };
                self.begin(
                    Action::ToggleServerStartup,
                    Purpose::ServerStartup,
                    Command::StartupSetServer(choice),
                )
            }
            Action::ToggleTrayLogin if self.enabled(&Action::ToggleTrayLogin) => {
                let Some(choice) = self.toggle_choice(|s| s.tray_startup.state) else {
                    return vec![];
                };
                self.begin(
                    Action::ToggleTrayLogin,
                    Purpose::TrayStartup,
                    Command::StartupSetTray(choice),
                )
            }
            Action::CheckForUpdates if self.enabled(&Action::CheckForUpdates) => self.begin(
                Action::CheckForUpdates,
                Purpose::Check,
                Command::UpdatesCheck,
            ),
            Action::Upgrade(version) if self.enabled(&Action::Upgrade(version.clone())) => self
                .begin(
                    Action::Upgrade(version.clone()),
                    Purpose::Prepare(version.clone()),
                    Command::UpdatesPrepare(version),
                ),
            Action::CancelUpgrade => match &self.snapshot.update {
                Some(u) if u.state.is_cancellable() => match &u.transaction_id {
                    Some(t) => vec![Effect::Run {
                        purpose: Purpose::Cancel(t.clone()),
                        command: Command::UpdatesCancel(t.clone()),
                    }],
                    None => vec![],
                },
                _ => vec![],
            },
            // A disabled entry, or busy: nothing happens.
            _ => vec![],
        }
    }

    fn toggle_choice(
        &self,
        read: impl Fn(&crate::protocol::StatusV1) -> StartupState,
    ) -> Option<Choice> {
        match read(self.snapshot.status.as_ref()?) {
            StartupState::Enabled => Some(Choice::Off),
            StartupState::Disabled => Some(Choice::On),
            _ => None,
        }
    }

    pub fn confirmed(&mut self, pending: Pending, accepted: bool) -> Vec<Effect> {
        match (pending, accepted) {
            (Pending::Stop, true) if self.enabled(&Action::Stop) => self.begin(
                Action::Stop,
                Purpose::Lifecycle(OperationKind::Stop),
                Command::Stop,
            ),
            (Pending::Restart, true) if self.enabled(&Action::Restart) => self.begin(
                Action::Restart,
                Purpose::Lifecycle(OperationKind::Restart),
                Command::Restart,
            ),
            (Pending::Apply { transaction, .. }, true) => {
                self.snapshot.busy = Some(Action::ShowUpgradeProgress);
                vec![
                    Effect::Render,
                    Effect::Run {
                        purpose: Purpose::Apply(transaction.clone()),
                        command: Command::UpdatesApply(transaction),
                    },
                ]
            }
            // Declining the named upgrade releases the verified preparation.
            (Pending::Apply { transaction, .. }, false) => vec![Effect::Run {
                purpose: Purpose::Cancel(transaction.clone()),
                command: Command::UpdatesCancel(transaction),
            }],
            _ => vec![],
        }
    }

    fn fail(&mut self, failure: &ControlFailure) -> Vec<Effect> {
        let message = failure.message();
        self.snapshot.last_error = Some(if failure.is_authorization() {
            format!("{message} Nothing was changed.")
        } else {
            message.clone()
        });
        vec![Effect::ShowError(message)]
    }

    fn unexpected(&mut self, purpose: &Purpose, result: &ControlResult) -> Vec<Effect> {
        self.fail(&ControlFailure::Protocol(format!(
            "{purpose:?} returned a {} result",
            result.kind()
        )))
    }

    pub fn completed(
        &mut self,
        purpose: Purpose,
        result: Result<ControlResult, ControlFailure>,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        match purpose {
            Purpose::Poll => {
                self.polling = false;
                match result {
                    Ok(ControlResult::Status(status)) => {
                        self.snapshot.status = Some(status);
                        self.snapshot.status_failure = None;
                    }
                    Ok(other) => {
                        self.snapshot.status_failure = Some(ControlFailure::Protocol(format!(
                            "status returned a {} result",
                            other.kind()
                        )))
                    }
                    Err(failure) => self.snapshot.status_failure = Some(failure),
                }
                if let Some(t) = self.snapshot.active_transaction().map(str::to_string) {
                    effects.push(Effect::Run {
                        purpose: Purpose::PollUpdate(t.clone()),
                        command: Command::UpdatesStatus(Some(t)),
                    });
                }
                effects.push(Effect::Render);
                return effects;
            }
            Purpose::PollUpdate(_) => {
                if let Ok(ControlResult::Update(update)) = result {
                    let terminal = update.state.is_terminal();
                    let changed =
                        self.snapshot.update.as_ref().map(|u| u.state) != Some(update.state);
                    self.snapshot.update = Some(update);
                    if terminal && changed {
                        effects.push(Effect::ShowUpgrade);
                    }
                }
                effects.push(Effect::Render);
                return effects;
            }
            Purpose::Logs => {
                return match result {
                    Ok(ControlResult::Logs(logs)) => vec![Effect::ShowLogs {
                        lines: logs.lines,
                        truncated: logs.truncated,
                    }],
                    Ok(other) => self.unexpected(&Purpose::Logs, &other),
                    Err(f) => {
                        let mut e = self.fail(&f);
                        e.push(Effect::Render);
                        e
                    }
                };
            }
            _ => {}
        }

        self.snapshot.busy = None;
        match (&purpose, result) {
            (Purpose::Lifecycle(op), Ok(ControlResult::Operation(o))) if o.operation == *op => {
                if let Some(s) = self.snapshot.status.as_mut() {
                    s.service_state = o.service_state;
                }
            }
            (Purpose::ServerStartup | Purpose::TrayStartup, Ok(ControlResult::Startup(s))) => {
                // Apply the observed result, not the requested one.
                if let Some(status) = self.snapshot.status.as_mut() {
                    status.server_startup = s.server;
                    status.tray_startup = s.tray;
                }
            }
            (Purpose::Check, Ok(ControlResult::Update(u))) => self.snapshot.update = Some(u),
            (Purpose::Prepare(version), Ok(ControlResult::Update(u))) => {
                let ok = u.state == UpdateState::Prepared && u.version.as_deref() == Some(version);
                let transaction = u.transaction_id.clone();
                self.snapshot.update = Some(u);
                match (ok, transaction) {
                    (true, Some(transaction)) => {
                        let from = self
                            .snapshot
                            .status
                            .as_ref()
                            .and_then(|s| s.version.clone())
                            .unwrap_or_else(|| "the installed version".into());
                        effects.push(Effect::Render);
                        effects.push(Effect::Confirm {
                            pending: Pending::Apply {
                                version: version.clone(),
                                transaction,
                            },
                            title: format!("Upgrade plurx to {version}?"),
                            message: format!("plurx {version} was downloaded and its signature verified. Upgrading from {from} stops the server once playback and recordings can stop safely, installs {version} and starts it again. If installation or the new version fails its checks, plurx reports that recovery is required. It does not automatically restore the previous version."),
                            proceed: format!("Upgrade to {version}"),
                        });
                        return effects;
                    }
                    _ => effects.extend(self.fail(&ControlFailure::Protocol(format!(
                        "prepare did not produce a verified {version} transaction"
                    )))),
                }
            }
            (Purpose::Apply(_) | Purpose::Cancel(_), Ok(ControlResult::Update(u))) => {
                self.snapshot.update = Some(u);
                effects.push(Effect::ShowUpgrade);
            }
            (p, Ok(other)) => effects.extend(self.unexpected(p, &other)),
            (_, Err(failure)) => effects.extend(self.fail(&failure)),
        }
        effects.push(Effect::Render);
        // Never take the operation's own answer as final: re-read status.
        effects.extend(self.poll());
        effects
    }
}

/// Linux lifecycle management is supported only under systemd (PID 1 is
/// systemd when /run/systemd/system exists).
pub fn management_for(systemd_booted: bool) -> Management {
    if systemd_booted {
        Management::Supported
    } else {
        Management::Unsupported("this system does not run systemd".into())
    }
}
