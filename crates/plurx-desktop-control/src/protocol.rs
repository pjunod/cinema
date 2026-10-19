//! Versioned wire types. No capability or credential is part of a durable journal.
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const IDENTITY_PATH: &str = "/local/v1/identity";
pub const PREPARE_PATH: &str = "/local/v1/shutdown/prepare";
pub const COMMIT_PATH: &str = "/local/v1/shutdown/commit";
pub const CANCEL_PATH: &str = "/local/v1/shutdown/cancel";
pub const STATUS_PATH: &str = "/local/v1/shutdown/status";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlErrorV1 {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlResponseV1 {
    pub schema_version: u32,
    pub ok: bool,
    pub result: Option<ControlResultV1>,
    pub error: Option<ControlErrorV1>,
}
impl ControlResponseV1 {
    pub fn success(result: ControlResultV1) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(error: ControlErrorV1) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Starting,
    Stopped,
    Unhealthy,
    Unknown,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StartupStateV1 {
    pub state: StartupState,
    pub detail: Option<String>,
}
impl StartupStateV1 {
    pub fn new(state: StartupState) -> Self {
        Self {
            state,
            detail: None,
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Start,
    Stop,
    Restart,
    Upgrade,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResultV1 {
    Status {
        installation_id: String,
        service_state: ServiceState,
        version: Option<String>,
        browser_origin: String,
        server_startup: StartupStateV1,
        tray_startup: StartupStateV1,
        transaction_id: Option<String>,
    },
    Operation {
        operation: Operation,
        service_state: ServiceState,
    },
    Startup {
        server: StartupStateV1,
        tray: StartupStateV1,
    },
    Update {
        state: UpdateState,
        version: Option<String>,
        transaction_id: Option<String>,
        detail: Option<String>,
    },
    Logs {
        lines: Vec<String>,
        truncated: bool,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentityV1 {
    pub pid: u32,
    pub uid: u32,
    pub start_generation: String,
    pub nonce: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorKind {
    Systemd,
    Launchd,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorIdentityV1 {
    pub kind: SupervisorKind,
    pub name: String,
    pub invocation_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonIdentityV1 {
    pub schema_version: u32,
    pub installation_id: String,
    pub process: ProcessIdentityV1,
    pub node_id: String,
    pub instance_id: String,
    pub version: String,
    pub build: String,
    pub browser_origin: String,
    pub supervisor: SupervisorIdentityV1,
    pub ready: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownPrepareRequestV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub operation: Operation,
    pub process_nonce: String,
    pub confirm_interruption: bool,
    pub target_version: Option<String>,
}
// Intentionally no Debug implementation: the capability must never appear in logs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownPreparedV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub process_nonce: String,
    pub generation: String,
    pub expires_unix_ms: u64,
    pub owner_capability: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownCommitRequestV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub process_nonce: String,
    pub generation: String,
    pub owner_capability: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownCommittedV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub process_nonce: String,
    pub generation: String,
    pub committed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownStatusV1 {
    pub schema_version: u32,
    pub operation_id: Option<String>,
    pub phase: String,
    pub generation: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    Clean,
    DrainFailed,
    StoreFailed,
    Unclean,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownReceiptV1 {
    pub schema_version: u32,
    pub operation_id: String,
    pub process: ProcessIdentityV1,
    pub generation: String,
    pub http_drained: bool,
    pub store_closed: bool,
    pub exit_class: ExitClass,
}
