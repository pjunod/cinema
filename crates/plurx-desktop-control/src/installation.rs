use crate::{
    protocol::{ErrorCode, SupervisorKind},
    secure_fs, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

pub const REPOSITORY: &str = "pjunod/cinema";
pub const SYSTEM_UNIT: &str = "plurxd.service";
pub const LAUNCH_LABEL: &str = "com.plurx.plurxd";
pub const INSTALL_HELPER: &str = "/usr/lib/plurx-control/plurx-install-helper";
pub const SERVICE_LAUNCH: &str = "/usr/lib/plurx-control/plurx-service-launch";
pub const RECOVERY: &str = "/usr/lib/plurx-control/plurx-recovery";
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallMethod {
    MacosNative,
    LinuxDeb,
    LinuxRpm,
    Source,
    External,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Amd64,
    Arm64,
    Universal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationV1 {
    pub schema_version: u32,
    pub installation_id: String,
    pub method: InstallMethod,
    pub owner_uid: u32,
    pub service_uid: u32,
    pub architecture: Architecture,
    pub desktop_installed: bool,
    pub repository: String,
    pub current_version: String,
    pub current_tag: String,
    pub current_commit: String,
    pub controller_protocol: u32,
    pub trust_key_ids: Vec<String>,
    pub last_transaction_id: Option<String>,
    pub paths: ManagedPathsV1,
    pub service: ServiceV1,
    pub browser_origin: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedPathsV1 {
    pub state: PathBuf,
    pub socket: PathBuf,
    pub runtime: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub log: PathBuf,
    pub canonical_plist: Option<PathBuf>,
    pub login_plist: Option<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceV1 {
    pub kind: SupervisorKind,
    pub name: String,
    pub domain: String,
}

fn absolute(p: &Path) -> bool {
    p.is_absolute()
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}
pub fn home_for(uid: u32) -> Result<PathBuf> {
    // The account database, NOT HOME (which pkexec/sudo/launch wrappers may inherit).
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    // SAFETY: caller-owned result/buffer, copied before buffer is dropped.
    let r = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if r != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "cannot determine native account home",
        ));
    }
    // SAFETY: getpwuid_r returned valid pw_dir into held buffer.
    let home = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
        .to_str()
        .map_err(|_| Error::new(ErrorCode::InvalidInstallation, "non-UTF8 account home"))?;
    Ok(PathBuf::from(home))
}
pub fn native_uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}
pub fn default_record() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home_for(native_uid())?.join("Library/Application Support/plurx/installation.json"))
    } else {
        Ok(PathBuf::from("/etc/plurx/installation.json"))
    }
}
impl InstallationV1 {
    pub fn load(path: &Path) -> Result<Self> {
        let expected_owner = if cfg!(target_os = "macos") {
            native_uid()
        } else {
            0
        };
        let value: Self = secure_fs::read_json(path, expected_owner)?;
        value.validate()?;
        if value.owner_uid != expected_owner {
            return Err(Error::new(
                ErrorCode::InvalidInstallation,
                "record owner differs from platform authority",
            ));
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        let reject = |m| Error::new(ErrorCode::InvalidInstallation, m);
        if self.schema_version != 1
            || self.controller_protocol != 1
            || uuid::Uuid::parse_str(&self.installation_id).is_err()
        {
            return Err(reject("unsupported installation identity/schema/protocol"));
        }
        if self.repository != REPOSITORY
            || semver::Version::parse(&self.current_version).is_err()
            || self.current_commit.len() != 40
            || !self.current_commit.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(reject("invalid release identity"));
        }
        let p = &self.paths;
        if [&p.state, &p.socket, &p.runtime, &p.config, &p.data, &p.log]
            .iter()
            .any(|p| !absolute(p))
        {
            return Err(reject("all managed paths must be absolute and normalized"));
        }
        validate_browser_url(&self.browser_origin)?;
        match self.method {
            InstallMethod::LinuxDeb | InstallMethod::LinuxRpm => {
                if self.owner_uid != 0
                    || self.service_uid == 0
                    || self.service.kind != SupervisorKind::Systemd
                    || self.service.name != SYSTEM_UNIT
                    || self.service.domain != "system"
                    || p.state != Path::new("/var/lib/plurx-control")
                    || p.socket != Path::new("/run/plurx/desktop-control.sock")
                    || p.runtime != Path::new("/usr/lib/plurx")
                    || p.canonical_plist.is_some()
                    || p.login_plist.is_some()
                    || self.architecture == Architecture::Universal
                {
                    return Err(reject(
                        "nonstandard system installation cannot be automatically managed",
                    ));
                }
            }
            InstallMethod::MacosNative => {
                let home = home_for(self.owner_uid)?;
                let runtime = home.join("Library/Application Support/plurx/runtime");
                if self.owner_uid == 0
                    || self.service_uid != self.owner_uid
                    || self.service.kind != SupervisorKind::Launchd
                    || self.service.name != LAUNCH_LABEL
                    || self.service.domain != format!("gui/{}", self.owner_uid)
                    || p.runtime != runtime
                    || p.state != runtime.join("control")
                    || p.socket != runtime.join("control/desktop-control.sock")
                    || p.canonical_plist.as_ref()
                        != Some(&runtime.join("control/com.plurx.plurxd.plist"))
                    || p.login_plist.as_ref()
                        != Some(&home.join("Library/LaunchAgents/com.plurx.plurxd.plist"))
                    || self.architecture != Architecture::Universal
                {
                    return Err(reject(
                        "nonstandard macOS layout requires explicit bootstrap migration",
                    ));
                }
            }
            InstallMethod::Source | InstallMethod::External => (),
        }
        if self.trust_key_ids.len() > 16
            || self
                .trust_key_ids
                .iter()
                .any(|s| s.is_empty() || s.len() > 64)
        {
            return Err(reject("invalid trust key identifiers"));
        }
        Ok(())
    }
    pub fn managed(&self) -> Result<()> {
        if matches!(self.method, InstallMethod::Source | InstallMethod::External) {
            return Err(Error::new(ErrorCode::Unsupported, "externally managed/source installation: use the verified manual bootstrap installer"));
        }
        if (cfg!(target_os = "macos")) != (self.method == InstallMethod::MacosNative) {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "installation is for another platform",
            ));
        }
        Ok(())
    }
    pub fn state_dir(&self) -> Result<secure_fs::SecureDir> {
        let dir = secure_fs::open_dir(&self.paths.state, self.owner_uid)?;
        if std::fs::symlink_metadata(&self.paths.state)?.uid() != self.owner_uid {
            return Err(Error::new(
                ErrorCode::InvalidInstallation,
                "wrong native state owner",
            ));
        }
        Ok(dir)
    }
    pub fn receipt_dir(&self) -> Result<secure_fs::SecureDir> {
        secure_fs::open_dir(
            &self.paths.state.join("shutdown-receipts"),
            self.service_uid,
        )
    }
}
pub fn validate_browser_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| Error::new(ErrorCode::InvalidInstallation, "invalid browser origin"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "unsafe browser URL",
        ));
    }
    Ok(())
}
