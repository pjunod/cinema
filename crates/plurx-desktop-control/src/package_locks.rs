//! Native package lock probes never wait or retain locks needed by the package manager.
use crate::{
    installation::{InstallMethod, InstallationV1},
    protocol::ErrorCode,
    secure_fs, Error, Result,
};
use std::{fs::File, os::fd::AsRawFd, path::Path};
pub fn file_is_locked(file: &File) -> Result<bool> {
    // F_GETLK supports a write-lock query on a validated read-only descriptor.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(lock.l_type != libc::F_UNLCK as libc::c_short)
}
pub fn check(i: &InstallationV1) -> Result<()> {
    let paths: &[&str] = match i.method {
        InstallMethod::LinuxDeb => &[
            "/var/lib/dpkg/lock-frontend",
            "/var/lib/dpkg/lock",
            "/var/lib/apt/lists/lock",
            "/var/cache/apt/archives/lock",
        ],
        InstallMethod::LinuxRpm => &["/var/lib/rpm/.rpm.lock", "/usr/lib/sysimage/rpm/.rpm.lock"],
        _ => return Ok(()),
    };
    for path in paths {
        let path = Path::new(path);
        let parent = match secure_fs::open_dir(
            path.parent()
                .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "package lock parent"))?,
            0,
        ) {
            Ok(p) => p,
            Err(e) if e.os_error == Some(libc::ENOENT) => continue,
            Err(e) => return Err(e),
        };
        let file = match parent.open(
            path.file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "package lock name"))?,
        ) {
            Ok(f) => f,
            Err(e) if e.os_error == Some(libc::ENOENT) => continue,
            Err(e) => return Err(e),
        };
        if file_is_locked(&file)? {
            return Err(Error::new(
                ErrorCode::Busy,
                "package-manager lock held; no package mutation attempted",
            ));
        }
    }
    Ok(())
}
