//! Read-only launch lock shared by the unprivileged wrapper and privileged worker.
//! The daemon inherits a shared lock across exec. Installation takes an exclusive
//! nonblocking lock only AFTER verified clean exit, closing guard-to-exec races.
use crate::{
    installation::{InstallMethod, InstallationV1},
    protocol::ErrorCode,
    secure_fs, Error, Result,
};
use std::{
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
};
const LOCK_NAME: &str = "launch.lock";
#[derive(Debug)]
pub struct StartLock {
    file: File,
}
impl StartLock {
    pub fn inherited_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}
#[derive(Clone, Copy)]
struct LockPolicy {
    owner: u32,
    group: u32,
    mode: u32,
}
fn account_gid(uid: u32) -> Result<u32> {
    let mut account: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    // SAFETY: held account/buffer/result storage, bounded buffer length.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut account,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() || account.pw_uid != uid {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "launch-lock account identity is unavailable",
        ));
    }
    Ok(account.pw_gid)
}
fn policy(i: &InstallationV1) -> Result<LockPolicy> {
    let (account, mode) = match i.method {
        InstallMethod::LinuxDeb | InstallMethod::LinuxRpm => (i.service_uid, 0o440),
        // macOS state remains private to its native owner; no public traversal.
        InstallMethod::MacosNative => (i.owner_uid, 0o444),
        _ => {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "unmanaged launch-lock authority",
            ))
        }
    };
    Ok(LockPolicy {
        owner: i.owner_uid,
        group: account_gid(account)?,
        mode,
    })
}
fn validate_lock(dir: &secure_fs::SecureDir, file: &File, policy: LockPolicy) -> Result<()> {
    let held = file.metadata()?;
    let current = dir.open(LOCK_NAME)?.metadata()?;
    for metadata in [&held, &current] {
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != policy.owner
            || metadata.gid() != policy.group
            || metadata.mode() & 0o7777 != policy.mode
        {
            return Err(Error::new(ErrorCode::InvalidInstallation, "launch-lock owner/group/mode/type differs from native authority; repair only while stopped"));
        }
    }
    if held.dev() != current.dev() || held.ino() != current.ino() {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "launch-lock inode changed; no launch or installation authority",
        ));
    }
    Ok(())
}
pub fn initialize(i: &InstallationV1) -> Result<()> {
    let root = i.state_dir()?;
    let policy = policy(i)?;
    match root.open(LOCK_NAME) {
        Ok(file) => return validate_lock(&root, &file, policy),
        Err(error) if error.os_error == Some(libc::ENOENT) => (),
        Err(error) => return Err(error),
    }
    // A lock inode is never atomically replaced: doing so would sever existing
    // flock authority. Publish one exclusive-created inode and validate races.
    let name = c"launch.lock";
    let fd = unsafe {
        libc::openat(
            root.fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return validate_lock(&root, &root.open(LOCK_NAME)?, policy);
        }
        return Err(error.into());
    }
    // SAFETY: successful exclusive openat returned this new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(b"plurx-native-launch-lock-v1\n")?;
    let metadata = file.metadata()?;
    if (metadata.uid() != policy.owner || metadata.gid() != policy.group)
        && unsafe { libc::fchown(fd, policy.owner, policy.group) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::fchmod(fd, policy.mode as libc::mode_t) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    if unsafe { libc::fsync(root.fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    validate_lock(&root, &file, policy)
}
pub fn acquire(i: &InstallationV1, exclusive: bool) -> Result<StartLock> {
    let dir = secure_fs::open_dir(&i.paths.state, i.owner_uid)?;
    let file = dir.open(LOCK_NAME)?;
    let policy = policy(i)?;
    validate_lock(&dir, &file, policy)?;
    // Linux is root:service-primary-group 0440, not world-readable. An arbitrary
    // local UID therefore cannot open this descriptor and hold flock(EX).
    let mode = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
        return Err(Error::new(
            ErrorCode::Busy,
            "daemon launch/installation lock is held; no conflicting mutation or start",
        ));
    }
    validate_lock(&dir, &file, policy)?;
    if !exclusive {
        // Keep shared protection across daemon exec and for that exact generation's lifetime.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) }
                < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(StartLock { file })
}
/// Duplicates a validated held descriptor for child inheritance, never an arbitrary path.
pub fn duplicate(lock: &StartLock) -> Result<File> {
    let fd = unsafe { libc::dup(lock.file.as_raw_fd()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::installation::native_uid;
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    fn fixture() -> (PathBuf, secure_fs::SecureDir, File, LockPolicy) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/desktop-lock-tests")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).expect("scratch");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private scratch");
        let root = root.canonicalize().expect("canonical scratch");
        let dir = secure_fs::open_dir(&root, native_uid()).expect("fixture directory");
        dir.write(LOCK_NAME, b"fixture", 0o440)
            .expect("fixture inode");
        let file = dir.open(LOCK_NAME).expect("held inode");
        let metadata = file.metadata().expect("metadata");
        let policy = LockPolicy {
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: 0o440,
        };
        (root, dir, file, policy)
    }
    #[test]
    fn launch_lock_rejects_replaced_inodes_instead_of_accepting_a_stale_fence() {
        let (root, dir, file, policy) = fixture();
        validate_lock(&dir, &file, policy).expect("original inode");
        std::fs::rename(root.join(LOCK_NAME), root.join("old.lock")).expect("retain old inode");
        dir.write(LOCK_NAME, b"replacement", 0o440)
            .expect("replacement fixture");
        assert!(validate_lock(&dir, &file, policy).is_err());
        std::fs::remove_dir_all(root).expect("remove scratch");
    }
    #[test]
    fn launch_lock_requires_exact_owner_and_trusted_service_group() {
        let (root, dir, file, policy) = fixture();
        assert!(validate_lock(
            &dir,
            &file,
            LockPolicy {
                group: policy.group.wrapping_add(1),
                ..policy
            }
        )
        .is_err());
        assert!(validate_lock(
            &dir,
            &file,
            LockPolicy {
                owner: policy.owner.wrapping_add(1),
                ..policy
            }
        )
        .is_err());
        std::fs::remove_dir_all(root).expect("remove scratch");
    }
}
