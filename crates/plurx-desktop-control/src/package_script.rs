//! Maintainer-script frontend. Initial record registration happens before generic record load.
use crate::{
    installation::{self, Architecture, InstallMethod, InstallationV1},
    native::{self, ReleaseIdentityV1},
    package,
    protocol::ErrorCode,
    secure_fs, supervisor, transaction, Error, Result,
};
use std::{path::Path, time::Duration};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInventory {
    pub method: InstallMethod,
    pub server_package_version: String,
    pub architecture: Architecture,
    pub desktop_installed: bool,
}
pub fn parse_deb_inventory(text: &str) -> Result<SystemInventory> {
    let mut server = None;
    let mut desktop = false;
    for line in text.lines() {
        let p: Vec<_> = line.split('\t').collect();
        if p.len() != 4 {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "invalid dpkg installed-package inventory",
            ));
        }
        let name = p[0].split(':').next().unwrap_or("");
        if p[3] != "installed" {
            continue;
        }
        match name {
            "plurxd" => {
                if server.is_some() {
                    return Err(Error::new(
                        ErrorCode::PolicyConflict,
                        "ambiguous installed server packages",
                    ));
                }
                let arch = match p[2] {
                    "amd64" => Architecture::Amd64,
                    "arm64" => Architecture::Arm64,
                    _ => {
                        return Err(Error::new(
                            ErrorCode::Unsupported,
                            "unsupported package architecture",
                        ))
                    }
                };
                server = Some((p[1].to_string(), arch));
            }
            "plurx-desktop" => desktop = true,
            _ => {
                return Err(Error::new(
                    ErrorCode::PolicyConflict,
                    "unexpected package inventory member",
                ))
            }
        }
    }
    let (version, arch) = server.ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidInstallation,
            "plurxd is not installed/configured",
        )
    })?;
    Ok(SystemInventory {
        method: InstallMethod::LinuxDeb,
        server_package_version: version,
        architecture: arch,
        desktop_installed: desktop,
    })
}
pub fn parse_rpm_inventory(text: &str) -> Result<SystemInventory> {
    let mut server = None;
    let mut desktop = false;
    for line in text.lines() {
        let p: Vec<_> = line.split('\t').collect();
        if p.len() != 3 {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "invalid RPM installed-package inventory",
            ));
        }
        match p[0] {
            "plurxd" => {
                if server.is_some() {
                    return Err(Error::new(
                        ErrorCode::PolicyConflict,
                        "ambiguous installed server packages",
                    ));
                }
                let arch = match p[2] {
                    "x86_64" => Architecture::Amd64,
                    "aarch64" => Architecture::Arm64,
                    _ => {
                        return Err(Error::new(
                            ErrorCode::Unsupported,
                            "unsupported package architecture",
                        ))
                    }
                };
                server = Some((p[1].to_string(), arch));
            }
            "plurx-desktop" => desktop = true,
            _ => {
                return Err(Error::new(
                    ErrorCode::PolicyConflict,
                    "unexpected package inventory member",
                ))
            }
        }
    }
    let (version, arch) = server
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "plurxd is not installed"))?;
    Ok(SystemInventory {
        method: InstallMethod::LinuxRpm,
        server_package_version: version,
        architecture: arch,
        desktop_installed: desktop,
    })
}
pub fn inventory() -> Result<SystemInventory> {
    if Path::new("/usr/bin/dpkg-query").exists() {
        let (_, server) = supervisor::output(
            "/usr/bin/dpkg-query",
            &[
                "-W".into(),
                "-f=${binary:Package}\t${Version}\t${Architecture}\t${db:Status-Status}\n".into(),
                "plurxd".into(),
            ],
            Duration::from_secs(5),
        )?;
        let (_, desktop) = supervisor::output(
            "/usr/bin/dpkg-query",
            &[
                "-W".into(),
                "-f=${binary:Package}\t${Version}\t${Architecture}\t${db:Status-Status}\n".into(),
                "plurx-desktop".into(),
            ],
            Duration::from_secs(5),
        )?;
        parse_deb_inventory(&format!("{server}{desktop}"))
    } else {
        let (_, server) = supervisor::output(
            "/usr/bin/rpm",
            &[
                "-q".into(),
                "--qf".into(),
                "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n".into(),
                "plurxd".into(),
            ],
            Duration::from_secs(5),
        )?;
        let (desktop_code, desktop) = supervisor::output(
            "/usr/bin/rpm",
            &[
                "-q".into(),
                "--qf".into(),
                "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n".into(),
                "plurx-desktop".into(),
            ],
            Duration::from_secs(5),
        )?;
        parse_rpm_inventory(&format!(
            "{server}{}",
            if desktop_code == 0 {
                desktop.as_str()
            } else {
                ""
            }
        ))
    }
}
/// Pure identity refresh used by ordinary root package installs, never by a joined updater.
pub fn refreshed(
    mut i: InstallationV1,
    release: &ReleaseIdentityV1,
    inventory: &SystemInventory,
) -> Result<InstallationV1> {
    release.validate()?;
    if i.owner_uid != 0
        || i.method != inventory.method
        || release.architecture != inventory.architecture
    {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "installed package method/architecture differs from trusted record/release metadata",
        ));
    }
    i.current_version = release.version.clone();
    i.current_tag = format!("v{}", release.version);
    i.current_commit = release.source_commit.clone();
    i.trust_key_ids = crate::update::production_keys()?
        .into_iter()
        .map(|k| k.id)
        .collect();
    i.desktop_installed = inventory.desktop_installed;
    i.validate()?;
    Ok(i)
}
pub fn run(package: &str, phase: &str, original_args: &[String]) -> Result<bool> {
    if !cfg!(target_os = "linux")
        || installation::native_uid() != 0
        || !matches!(package, "plurxd" | "plurx-desktop")
        || !matches!(
            phase,
            "preinstall" | "postinstall" | "preremove" | "postremove"
        )
        || original_args.len() > 16
        || original_args.iter().any(|s| s.len() > 1024)
    {
        return Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "invalid package-script request",
        ));
    }
    let record = installation::default_record()?;
    let existing = match InstallationV1::load(&record) {
        Ok(i) => Some(i),
        Err(e) if e.os_error == Some(libc::ENOENT) => None,
        Err(e) => return Err(e),
    };
    if let Some(i) = &existing {
        // Live root lineage + signed plan, not environment or untrusted script arguments.
        if package::hook_check(i, package, "", phase)? {
            return Ok(true);
        }
    }
    if !matches!(phase, "postinstall" | "postremove") {
        return Ok(false);
    }
    if phase == "postremove" && package == "plurxd" {
        return Ok(false);
    }
    let inventory = inventory()?;
    let release: ReleaseIdentityV1 =
        secure_fs::read_json(Path::new("/usr/lib/plurx-control/release.json"), 0)?;
    release.validate()?;
    if release.architecture != inventory.architecture {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "release architecture differs from actual installed package",
        ));
    }
    let store = existing
        .as_ref()
        .map(transaction::TransactionStore::locked)
        .transpose()?;
    if let Some(s) = &store {
        if s.active()?.is_some_and(|j| !j.terminal()) {
            return Err(Error::new(
                ErrorCode::Busy,
                "cannot refresh record during another transaction",
            ));
        }
    }
    let installed = if let Some(i) = existing {
        refreshed(i, &release, &inventory)?
    } else {
        native::bootstrap(
            inventory.method,
            &release.version,
            &release.source_commit,
            inventory.desktop_installed,
            "http://127.0.0.1:32400",
            None,
        )?
    };
    crate::controller::save_installation(&installed)?;
    Ok(false)
}
