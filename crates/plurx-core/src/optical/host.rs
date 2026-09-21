use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::{path::Path, process::Stdio, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use tokio::io::AsyncReadExt;

use super::InspectionResponse;
#[cfg(target_os = "linux")]
use super::{validate_inspection, INSPECTION_SCHEMA_V1};
use crate::config::OpticalDriveConfig;

#[cfg(target_os = "linux")]
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(target_os = "linux")]
const EJECT_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
const PRESENCE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const MAX_HELPER_REPLY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRequirementStatus {
    Met,
    Unmet,
    Unknown,
}

/// Cheap media observation result. `Unknown` preserves the current insertion:
/// a transient udev/helper failure is not evidence that the tray changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpticalMediaPresence {
    Empty,
    Present,
    Unknown,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceReply {
    presence: OpticalMediaPresence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostRequirement {
    pub id: &'static str,
    pub status: HostRequirementStatus,
    pub detail: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum OpticalHostError {
    #[error("optical host integration is unavailable on this operating system")]
    UnsupportedHost,
    #[error("the configured optical helper is unavailable")]
    HelperUnavailable,
    #[error("the optical helper timed out")]
    Timeout,
    #[error("the optical helper reply exceeded its byte limit")]
    OutputLimit,
    #[error("the optical helper exited unsuccessfully")]
    Failed,
    #[error("the optical helper returned invalid JSON: {0}")]
    InvalidReply(String),
    #[error("the optical helper answered for a stale insertion")]
    StaleGeneration,
}

#[async_trait]
pub trait OpticalHostAdapter: Send + Sync + 'static {
    fn requirements(&self, drive: &OpticalDriveConfig) -> Vec<HostRequirement>;

    /// Observe tray/media state without performing title inspection.
    async fn presence(
        &self,
        drive: &OpticalDriveConfig,
    ) -> Result<OpticalMediaPresence, OpticalHostError>;

    async fn inspect(
        &self,
        drive: &OpticalDriveConfig,
        expected_generation: &str,
    ) -> Result<InspectionResponse, OpticalHostError>;

    async fn eject(&self, drive: &OpticalDriveConfig) -> Result<(), OpticalHostError>;
}

#[derive(Debug, Clone)]
pub struct SystemOpticalHost {
    helper_path: PathBuf,
}

impl SystemOpticalHost {
    pub fn new(helper_path: impl Into<PathBuf>) -> Self {
        Self {
            helper_path: helper_path.into(),
        }
    }

    #[cfg(target_os = "linux")]
    async fn run_helper(
        &self,
        action: &str,
        drive: &OpticalDriveConfig,
        expected_generation: Option<&str>,
        timeout: Duration,
        output_limit: u64,
    ) -> Result<Vec<u8>, OpticalHostError> {
        let mut command = tokio::process::Command::new(&self.helper_path);
        command
            .kill_on_drop(true)
            .arg(action)
            .arg("--device")
            .arg(&drive.device_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // The reply owns bounded diagnostics. A second pipe could block
            // after the stdout reader deliberately stops at its limit.
            .stderr(Stdio::null());
        if !drive.mount_path.as_os_str().is_empty() {
            command.arg("--mount").arg(&drive.mount_path);
        }
        if let Some(generation) = expected_generation {
            command
                .arg("--schema")
                .arg(INSPECTION_SCHEMA_V1.to_string())
                .arg("--expected-generation")
                .arg(generation);
        }
        let run = async move {
            let mut child = command
                .spawn()
                .map_err(|_| OpticalHostError::HelperUnavailable)?;
            let stdout = child
                .stdout
                .take()
                .ok_or(OpticalHostError::HelperUnavailable)?;
            let mut bytes = Vec::new();
            stdout
                .take(output_limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| OpticalHostError::Failed)?;
            if bytes.len() > output_limit as usize {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(OpticalHostError::OutputLimit);
            }
            let status = child.wait().await.map_err(|_| OpticalHostError::Failed)?;
            if !status.success() {
                return Err(OpticalHostError::Failed);
            }
            Ok(bytes)
        };
        tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| OpticalHostError::Timeout)?
    }
}

#[async_trait]
impl OpticalHostAdapter for SystemOpticalHost {
    fn requirements(&self, drive: &OpticalDriveConfig) -> Vec<HostRequirement> {
        #[cfg(target_os = "linux")]
        {
            let helper = executable_exists(&self.helper_path);
            let device = std::fs::metadata(&drive.device_path).is_ok();
            let mount = drive.mount_path.as_os_str().is_empty()
                || std::fs::metadata(&drive.mount_path).is_ok_and(|metadata| metadata.is_dir());
            vec![
                HostRequirement {
                    id: "linux_host",
                    status: HostRequirementStatus::Met,
                    detail: "Linux optical host adapter is compiled".to_owned(),
                },
                HostRequirement {
                    id: "helper",
                    status: if helper {
                        HostRequirementStatus::Met
                    } else {
                        HostRequirementStatus::Unmet
                    },
                    detail: if helper {
                        "Configured helper is available".to_owned()
                    } else {
                        "Install or configure plurx-optical-helper".to_owned()
                    },
                },
                HostRequirement {
                    id: "device",
                    status: if device {
                        HostRequirementStatus::Met
                    } else {
                        HostRequirementStatus::Unmet
                    },
                    detail: if device {
                        "Configured drive device exists; operation-time permissions are still checked"
                            .to_owned()
                    } else {
                        "Configured drive device is absent".to_owned()
                    },
                },
                HostRequirement {
                    id: "mount",
                    status: if mount {
                        HostRequirementStatus::Met
                    } else {
                        HostRequirementStatus::Unmet
                    },
                    detail: if mount {
                        "Configured mount requirement is present".to_owned()
                    } else {
                        "Configured mount is absent".to_owned()
                    },
                },
                HostRequirement {
                    id: "permissions",
                    status: HostRequirementStatus::Unknown,
                    detail:
                        "Effective read/control permission is verified by the requested operation"
                            .to_owned(),
                },
            ]
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (drive, &self.helper_path);
            vec![HostRequirement {
                id: "host_adapter",
                status: HostRequirementStatus::Unmet,
                detail: "This release provides physical optical hosting on Linux only".to_owned(),
            }]
        }
    }

    async fn presence(
        &self,
        drive: &OpticalDriveConfig,
    ) -> Result<OpticalMediaPresence, OpticalHostError> {
        #[cfg(target_os = "linux")]
        {
            let bytes = self
                .run_helper("presence", drive, None, PRESENCE_TIMEOUT, 4 * 1024)
                .await?;
            serde_json::from_slice::<PresenceReply>(&bytes)
                .map(|reply| reply.presence)
                .map_err(|error| OpticalHostError::InvalidReply(error.to_string()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = drive;
            Err(OpticalHostError::UnsupportedHost)
        }
    }

    async fn inspect(
        &self,
        drive: &OpticalDriveConfig,
        expected_generation: &str,
    ) -> Result<InspectionResponse, OpticalHostError> {
        #[cfg(target_os = "linux")]
        {
            let bytes = self
                .run_helper(
                    "inspect",
                    drive,
                    Some(expected_generation),
                    INSPECTION_TIMEOUT,
                    MAX_HELPER_REPLY_BYTES,
                )
                .await?;
            let response = serde_json::from_slice::<InspectionResponse>(&bytes)
                .map_err(|error| OpticalHostError::InvalidReply(error.to_string()))?;
            validate_inspection(&response)
                .map_err(|error| OpticalHostError::InvalidReply(error.to_string()))?;
            if response.expected_generation != expected_generation {
                return Err(OpticalHostError::StaleGeneration);
            }
            Ok(response)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (drive, expected_generation);
            Err(OpticalHostError::UnsupportedHost)
        }
    }

    async fn eject(&self, drive: &OpticalDriveConfig) -> Result<(), OpticalHostError> {
        #[cfg(target_os = "linux")]
        {
            self.run_helper("eject", drive, None, EJECT_TIMEOUT, MAX_HELPER_REPLY_BYTES)
                .await?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = drive;
            Err(OpticalHostError::UnsupportedHost)
        }
    }
}

#[cfg(target_os = "linux")]
fn executable_exists(path: &Path) -> bool {
    if path.components().count() > 1 {
        return std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file());
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(path))
            .any(|candidate| std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_host_requirements_are_advisory_and_path_free() {
        let host = SystemOpticalHost::new("definitely-missing-optical-helper");
        let drive = OpticalDriveConfig {
            id: "drive-a".to_owned(),
            label: "Drive A".to_owned(),
            device_path: PathBuf::from("/definitely/missing/device"),
            mount_path: PathBuf::from("/definitely/missing/mount"),
        };
        let requirements = host.requirements(&drive);
        let json = serde_json::to_string(&requirements).expect("requirements JSON");
        assert!(!json.contains("/definitely/missing"));
        assert!(requirements
            .iter()
            .any(|requirement| requirement.status == HostRequirementStatus::Unmet));
    }
}
