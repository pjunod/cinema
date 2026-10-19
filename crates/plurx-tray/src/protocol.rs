//! Desktop control protocol v1 as the Linux UI consumes it. The producer is
//! `plurx_desktop_control::protocol` (CONTROL-CONTRACT.md, task t_a421d903).
//!
//! Decoding is strict on everything the UI acts on: another schema version,
//! an unknown `kind`, state or field, an absent nullable field, or an `ok`
//! flag that disagrees with its result/error is a protocol failure. Nothing
//! unknown is coerced into "stopped", so an unreadable server never invites a
//! second Start.
use serde::{Deserialize, Deserializer, Serialize};

pub const SCHEMA_VERSION: u64 = 1;
/// The controller writes at most this many bytes of JSON per invocation.
pub const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// A field the contract always writes, possibly as `null`. Serde would
/// otherwise default a missing `Option` to `None`, hiding a renamed field.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Starting,
    Stopped,
    Unhealthy,
    Unknown,
}

impl ServiceState {
    pub const ALL: [Self; 5] = [
        Self::Running,
        Self::Starting,
        Self::Stopped,
        Self::Unhealthy,
        Self::Unknown,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Starting => "Starting",
            Self::Stopped => "Stopped",
            Self::Unhealthy => "Unhealthy",
            Self::Unknown => "Unknown",
        }
    }
    pub fn wire(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Starting => "starting",
            Self::Stopped => "stopped",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
    /// The installed symbolic icon (deploy/desktop/icons). Each state has its
    /// own shape; colour is never the only signal.
    pub fn icon_name(self) -> String {
        format!("tv.plurx.Server-{}-symbolic", self.wire())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupState {
    Enabled,
    Disabled,
    Masked,
    Static,
    ApprovalRequired,
    PolicyManaged,
    Unknown,
}

impl StartupState {
    pub const ALL: [Self; 7] = [
        Self::Enabled,
        Self::Disabled,
        Self::Masked,
        Self::Static,
        Self::ApprovalRequired,
        Self::PolicyManaged,
        Self::Unknown,
    ];
    /// Text added to a startup checkbox that is not a plain on/off.
    pub fn qualifier(self) -> Option<&'static str> {
        match self {
            Self::Enabled | Self::Disabled => None,
            Self::Masked => Some("masked by the administrator"),
            Self::Static => Some("fixed by the unit definition"),
            Self::ApprovalRequired => Some("needs approval"),
            Self::PolicyManaged => Some("managed by policy"),
            Self::Unknown => Some("state unknown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupStateV1 {
    pub state: StartupState,
    #[serde(deserialize_with = "nullable")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Start,
    Stop,
    Restart,
    Upgrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    Available,
    Prepared,
    WaitingForIdle,
    ShutdownCommitted,
    Installing,
    Restarting,
    Complete,
    Cancelled,
    Failed,
    RecoveryRequired,
}

impl UpdateState {
    pub const ALL: [Self; 12] = [
        Self::Idle,
        Self::Checking,
        Self::Available,
        Self::Prepared,
        Self::WaitingForIdle,
        Self::ShutdownCommitted,
        Self::Installing,
        Self::Restarting,
        Self::Complete,
        Self::Cancelled,
        Self::Failed,
        Self::RecoveryRequired,
    ];
    /// Before the daemon commits to shutting down the preparation can be
    /// cancelled; afterwards it only runs forward or into recovery.
    pub fn is_cancellable(self) -> bool {
        matches!(self, Self::Prepared | Self::WaitingForIdle)
    }
    /// Still owned by the controller's worker: follow it, offer nothing new.
    pub fn is_in_flight(self) -> bool {
        matches!(
            self,
            Self::Checking
                | Self::Prepared
                | Self::WaitingForIdle
                | Self::ShutdownCommitted
                | Self::Installing
                | Self::Restarting
        )
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Cancelled | Self::Failed | Self::RecoveryRequired
        )
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Idle => "Up to date",
            Self::Checking => "Checking for updates",
            Self::Available => "Update available",
            Self::Prepared => "Downloaded and verified",
            Self::WaitingForIdle => "Waiting for playback and recordings to finish",
            Self::ShutdownCommitted => "Stopping the server",
            Self::Installing => "Installing",
            Self::Restarting => "Restarting",
            Self::Complete => "Upgrade complete",
            Self::Cancelled => "Upgrade cancelled",
            Self::Failed => "Upgrade failed",
            Self::RecoveryRequired => "Recovery required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInstallation,
    Unsupported,
    AuthorizationRequired,
    AuthorizationDenied,
    Busy,
    IdentityMismatch,
    UnsafeShutdown,
    PolicyConflict,
    Unavailable,
    RateLimited,
    UntrustedUpdate,
    RecoveryRequired,
    InvalidRequest,
    Io,
}

impl ErrorCode {
    pub fn summary(self) -> &'static str {
        match self {
            Self::InvalidInstallation => {
                "This plurx installation is not managed by the desktop app"
            }
            Self::Unsupported => "This operation is not supported for this installation",
            Self::AuthorizationRequired => "Authorization is required",
            Self::AuthorizationDenied => "Authorization was denied or cancelled",
            Self::Busy => "Another plurx operation is in progress",
            Self::IdentityMismatch => {
                "The running server could not be verified as this installation"
            }
            Self::UnsafeShutdown => "The server cannot stop safely right now",
            Self::PolicyConflict => "A system policy prevents this change",
            Self::Unavailable => "The server or update service is unavailable",
            Self::RateLimited => "GitHub is rate-limiting update checks",
            Self::UntrustedUpdate => "The release could not be verified",
            Self::RecoveryRequired => "An interrupted upgrade needs recovery",
            Self::InvalidRequest => "The controller rejected the request",
            Self::Io => "A file or system operation failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlErrorV1 {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusV1 {
    pub installation_id: String,
    pub service_state: ServiceState,
    #[serde(deserialize_with = "nullable")]
    pub version: Option<String>,
    pub browser_origin: String,
    pub server_startup: StartupStateV1,
    pub tray_startup: StartupStateV1,
    #[serde(deserialize_with = "nullable")]
    pub transaction_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationV1 {
    pub operation: OperationKind,
    pub service_state: ServiceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupV1 {
    pub server: StartupStateV1,
    pub tray: StartupStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateV1 {
    pub state: UpdateState,
    #[serde(deserialize_with = "nullable")]
    pub version: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub transaction_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsV1 {
    pub lines: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlResult {
    Status(StatusV1),
    Operation(OperationV1),
    Startup(StartupV1),
    Update(UpdateV1),
    Logs(LogsV1),
}

impl ControlResult {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::Operation(_) => "operation",
            Self::Startup(_) => "startup",
            Self::Update(_) => "update",
            Self::Logs(_) => "logs",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[allow(dead_code, reason = "checked before typed decoding")]
    schema_version: u64,
    ok: bool,
    #[serde(deserialize_with = "nullable")]
    result: Option<ControlResult>,
    #[serde(deserialize_with = "nullable")]
    error: Option<ControlErrorV1>,
}

/// A failure the UI can show. Controller errors keep their wire code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFailure {
    Controller(ControlErrorV1),
    Protocol(String),
    Launch(String),
    TimedOut(u64),
    OutputTooLarge,
    Exited { status: i32, diagnostics: String },
}

impl ControlFailure {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Controller(e) => Some(e.code),
            _ => None,
        }
    }

    /// True for a denied/cancelled/missing authorization: the UI keeps the
    /// previously observed controls and check marks and says so.
    pub fn is_authorization(&self) -> bool {
        matches!(
            self.code(),
            Some(ErrorCode::AuthorizationRequired | ErrorCode::AuthorizationDenied)
        )
    }

    pub fn message(&self) -> String {
        match self {
            Self::Controller(e) => {
                let detail = e.message.trim();
                if detail.is_empty() {
                    format!("{}.", e.code.summary())
                } else {
                    format!("{}: {}", e.code.summary(), detail)
                }
            }
            Self::Protocol(t) => {
                format!("The controller returned an unexpected response ({t}).")
            }
            Self::Launch(t) => format!("The plurx controller could not run: {t}."),
            Self::TimedOut(s) => format!(
                "The controller did not answer within {s} seconds; the operation may still be finishing."
            ),
            Self::OutputTooLarge => "The controller response exceeded the 1 MiB limit.".into(),
            Self::Exited {
                status,
                diagnostics,
            } => match diagnostics.lines().rev().find(|l| !l.trim().is_empty()) {
                Some(last) => format!("The controller exited with status {status}: {}", last.trim()),
                None => format!("The controller exited with status {status}."),
            },
        }
    }
}

/// Decodes one complete ControlResponseV1.
pub fn decode_response(bytes: &[u8]) -> Result<ControlResult, ControlFailure> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ControlFailure::OutputTooLarge);
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| ControlFailure::Protocol(format!("malformed JSON: {e}")))?;
    let version = value
        .as_object()
        .ok_or_else(|| ControlFailure::Protocol("response is not a JSON object".into()))?
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(SCHEMA_VERSION) {
        return Err(ControlFailure::Protocol(format!(
            "controller speaks schema {}, this app speaks {SCHEMA_VERSION}",
            version.map_or_else(|| "?".into(), |v| v.to_string())
        )));
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|e| ControlFailure::Protocol(format!("response shape: {e}")))?;
    match (envelope.ok, envelope.result, envelope.error) {
        (true, Some(result), None) => Ok(result),
        (false, None, Some(error)) => Err(ControlFailure::Controller(error)),
        _ => Err(ControlFailure::Protocol(
            "ok flag disagrees with result/error".into(),
        )),
    }
}

/// The browser destination: an absolute http(s) URL with a host and no
/// credentials. It is never a lifecycle-control target.
pub fn browser_url(origin: &str) -> Option<&str> {
    if origin.len() > 2048 || origin.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.starts_with(':') {
        return None;
    }
    Some(origin)
}

/// https://github.com/OWNER/REPO/releases/tag/vVERSION for a strict
/// OWNER/REPO and SemVer; None otherwise. Nothing from the network chooses it.
pub fn release_notes_url(repository: &str, version: &str) -> Option<String> {
    let mut parts = repository.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if parts.next().is_some()
        || !name_ok(owner)
        || !name_ok(repo)
        || !crate::command::is_version(version)
    {
        return None;
    }
    Some(format!(
        "https://github.com/{owner}/{repo}/releases/tag/v{version}"
    ))
}
