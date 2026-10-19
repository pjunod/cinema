//! The finite controller command set (CONTROL-CONTRACT.md "CLI boundary").
//! Every invocation is an argument vector; values that reach argv (a version,
//! a transaction id) are validated against the exact shapes the controller
//! accepts, so nothing from a server, release note or record becomes an
//! option or a path.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    On,
    Off,
}

impl Choice {
    fn wire(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Status,
    Start,
    Stop,
    Restart,
    StartupGet,
    StartupSetServer(Choice),
    StartupSetTray(Choice),
    UpdatesCheck,
    UpdatesPrepare(String),
    UpdatesApply(String),
    UpdatesStatus(Option<String>),
    UpdatesCancel(String),
    Logs(u32),
}

pub const DEFAULT_LOG_LINES: u32 = 100;
pub const MAX_LOG_LINES: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    Version(String),
    Transaction(String),
    Lines(u32),
}

impl Command {
    /// Arguments after the executable. Stop, restart and apply carry
    /// `--confirm-interruption` because the UI issues them only after the user
    /// confirmed the named interruption.
    pub fn args(&self) -> Result<Vec<String>, Invalid> {
        let v = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect();
        Ok(match self {
            Self::Status => v(&["status"]),
            Self::Start => v(&["start"]),
            Self::Stop => v(&["stop", "--confirm-interruption"]),
            Self::Restart => v(&["restart", "--confirm-interruption"]),
            Self::StartupGet => v(&["startup", "get"]),
            Self::StartupSetServer(c) => v(&["startup", "set", "server", c.wire()]),
            Self::StartupSetTray(c) => v(&["startup", "set", "tray", c.wire()]),
            Self::UpdatesCheck => v(&["updates", "check"]),
            Self::UpdatesPrepare(version) => {
                if !is_version(version) {
                    return Err(Invalid::Version(version.clone()));
                }
                v(&["updates", "prepare", "--version", version])
            }
            Self::UpdatesApply(t) => {
                transaction(t)?;
                v(&[
                    "updates",
                    "apply",
                    "--transaction",
                    t,
                    "--confirm-interruption",
                ])
            }
            Self::UpdatesStatus(None) => v(&["updates", "status"]),
            Self::UpdatesStatus(Some(t)) => {
                transaction(t)?;
                v(&["updates", "status", "--transaction", t])
            }
            Self::UpdatesCancel(t) => {
                transaction(t)?;
                v(&["updates", "cancel", "--transaction", t])
            }
            Self::Logs(lines) => {
                if !(1..=MAX_LOG_LINES).contains(lines) {
                    return Err(Invalid::Lines(*lines));
                }
                vec!["logs".into(), "--lines".into(), lines.to_string()]
            }
        })
    }

    /// Wall-clock bound for one invocation. Stop/restart wait for the daemon's
    /// committed drain and clean-exit receipt (and possibly a polkit dialog);
    /// prepare downloads and verifies a release; apply only hands off to the
    /// detached worker.
    pub fn timeout_secs(&self) -> u64 {
        match self {
            Self::Status | Self::StartupGet | Self::UpdatesStatus(_) | Self::Logs(_) => 15,
            Self::StartupSetTray(_) => 15,
            Self::StartupSetServer(_) | Self::UpdatesCancel(_) => 120,
            Self::Start | Self::UpdatesCheck | Self::UpdatesApply(_) => 180,
            Self::Stop | Self::Restart => 360,
            Self::UpdatesPrepare(_) => 900,
        }
    }

    /// Reads are killed at their bound; a mutation is left to finish because
    /// the controller owns it and killing the CLI could only lose its result.
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::Status
                | Self::StartupGet
                | Self::UpdatesStatus(_)
                | Self::Logs(_)
                | Self::UpdatesCheck
        )
    }
}

fn transaction(t: &str) -> Result<(), Invalid> {
    if is_transaction(t) {
        Ok(())
    } else {
        Err(Invalid::Transaction(t.to_string()))
    }
}

/// Strict SemVer 2.0 without a leading `v`.
pub fn is_version(text: &str) -> bool {
    if text.is_empty() || text.len() > 64 {
        return false;
    }
    let (rest, build) = match text.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (text, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let numeric = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
    };
    let ident_list = |s: &str| {
        s.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| numeric(p))
        && pre.is_none_or(ident_list)
        && build.is_none_or(ident_list)
}

/// Lower-case hyphenated UUID as the controller emits it.
pub fn is_transaction(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}
