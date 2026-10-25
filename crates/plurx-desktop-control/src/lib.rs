//! Shared, desktop-free native control and recovery. No GUI dependency belongs here.
#[cfg(unix)]
pub mod cli;
#[cfg(unix)]
pub mod controller;
#[cfg(unix)]
pub mod installation;
#[cfg(unix)]
pub mod ipc;
#[cfg(unix)]
pub mod launch_lock;
#[cfg(unix)]
pub mod native;
#[cfg(unix)]
pub mod package;
#[cfg(unix)]
pub mod package_locks;
#[cfg(unix)]
pub mod package_script;
#[cfg(unix)]
pub mod process;
#[cfg(unix)]
pub mod progress;
pub mod protocol;
#[cfg(unix)]
pub mod secure_fs;
#[cfg(unix)]
pub mod source_install;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod transaction;
#[cfg(unix)]
pub mod update;

pub fn unsupported_entry() -> i32 {
    let error = Error::new(
        ErrorCode::Unsupported,
        "Native desktop service management is supported only on macOS and Linux",
    );
    if let Ok(json) = serde_json::to_string(&protocol::ControlResponseV1::failure(error.wire())) {
        println!("{json}");
    }
    1
}
#[cfg(not(unix))]
pub mod cli {
    pub fn control_entry() -> i32 {
        crate::unsupported_entry()
    }
    pub fn helper_entry() -> i32 {
        crate::unsupported_entry()
    }
    pub fn worker_entry() -> i32 {
        crate::unsupported_entry()
    }
    pub fn recovery_entry() -> i32 {
        crate::unsupported_entry()
    }
    pub fn service_launch_entry() -> i32 {
        crate::unsupported_entry()
    }
}

use protocol::{ControlErrorV1, ErrorCode};
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub os_error: Option<i32>,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            os_error: None,
        }
    }
    pub fn wire(self) -> ControlErrorV1 {
        let retryable = matches!(
            self.code,
            ErrorCode::Busy | ErrorCode::Unavailable | ErrorCode::RateLimited
        );
        ControlErrorV1 {
            code: self.code,
            message: self.message,
            retryable,
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self {
            code: ErrorCode::Io,
            message: e.to_string(),
            os_error: e.raw_os_error(),
        }
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(ErrorCode::InvalidRequest, e.to_string())
    }
}
pub fn unix_time() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::new(ErrorCode::UntrustedUpdate, "system clock precedes epoch"))
}
