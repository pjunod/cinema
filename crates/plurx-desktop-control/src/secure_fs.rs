//! Descriptor-relative operations for authority files. No symlink traversal on reads/writes.
use crate::protocol::ErrorCode;
use crate::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path},
};

pub const MAX_JSON: u64 = 1024 * 1024;
fn c_name(name: &OsStr) -> Result<CString> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "invalid authority filename",
        ));
    }
    CString::new(b).map_err(|_| Error::new(ErrorCode::InvalidRequest, "NUL in name"))
}
fn cvt(fd: i32) -> Result<File> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful open/openat returns a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn trusted(file: &File, owner: u32, directory: bool) -> Result<()> {
    let m = file.metadata()?;
    if (m.uid() != owner && m.uid() != 0)
        || m.mode() & 0o022 != 0
        || (directory && !m.is_dir())
        || (!directory && (!m.is_file() || m.nlink() != 1))
    {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "untrusted owner, mode, type or hardlink",
        ));
    }
    Ok(())
}
/// Traverse from / with O_NOFOLLOW on EVERY component, not just the leaf.
pub fn open_dir(path: &Path, owner: u32) -> Result<SecureDir> {
    if !path.is_absolute() {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "authority path must be absolute",
        ));
    }
    let root = CString::new("/").map_err(|_| Error::new(ErrorCode::Io, "root"))?;
    // SAFETY: constant flags, valid C string.
    let mut dir = cvt(unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    for component in path.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                let name = c_name(name)?;
                // SAFETY: held directory descriptor and validated single name.
                let next = cvt(unsafe {
                    libc::openat(
                        dir.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                })?;
                trusted(&next, owner, true)?;
                dir = next;
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::InvalidInstallation,
                    "noncanonical authority path",
                ))
            }
        }
    }
    trusted(&dir, owner, true)?;
    Ok(SecureDir { dir, owner })
}
#[derive(Debug)]
pub struct SecureDir {
    dir: File,
    owner: u32,
}
impl SecureDir {
    pub fn owner(&self) -> u32 {
        self.owner
    }
    pub fn fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }
    pub fn child(&self, name: &str, create: bool) -> Result<Self> {
        let name = c_name(OsStr::new(name))?;
        if create {
            // SAFETY: held parent and single name; existing directories are validated below.
            let r = unsafe { libc::mkdirat(self.fd(), name.as_ptr(), 0o700) };
            if r != 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.dir.sync_all()?;
        }
        // SAFETY: single component with nofollow.
        let dir = cvt(unsafe {
            libc::openat(
                self.fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        trusted(&dir, self.owner, true)?;
        if dir.metadata()?.uid() != self.owner {
            return Err(Error::new(
                ErrorCode::InvalidInstallation,
                "state directory owned by a different authority",
            ));
        }
        Ok(Self {
            dir,
            owner: self.owner,
        })
    }
    pub fn open(&self, name: &str) -> Result<File> {
        let name = c_name(OsStr::new(name))?;
        // SAFETY: descriptor-bound single file, nofollow, nonblocking excludes FIFO hangs.
        let f = cvt(unsafe {
            libc::openat(
                self.fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        })?;
        trusted(&f, self.owner, false)?;
        Ok(f)
    }
    pub fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>> {
        let f = self.open(name)?;
        if f.metadata()?.len() > limit {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                "authority file exceeds size limit",
            ));
        }
        let mut b = Vec::new();
        f.take(limit + 1).read_to_end(&mut b)?;
        if b.len() as u64 > limit {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                "authority file grew past limit",
            ));
        }
        Ok(b)
    }
    pub fn json<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        Ok(serde_json::from_slice(&self.read(name, MAX_JSON)?)?)
    }
    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.write(name, &serde_json::to_vec(value)?, 0o600)
    }
    pub fn write(&self, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
        let target = c_name(OsStr::new(name))?;
        let temporary = c_name(OsStr::new(&format!(".{}.new", uuid::Uuid::new_v4())))?;
        // SAFETY: exclusive inode, nofollow, parent descriptor held.
        let mut f = cvt(unsafe {
            libc::openat(
                self.fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        let result = (|| {
            f.write_all(bytes)?;
            // SAFETY: descriptor is owned; mode is bounded to permissions.
            if unsafe { libc::fchmod(f.as_raw_fd(), (mode & 0o777) as libc::mode_t) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            f.sync_all()?;
            // SAFETY: same held directory and validated leaf names.
            if unsafe { libc::renameat(self.fd(), temporary.as_ptr(), self.fd(), target.as_ptr()) }
                != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.dir.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: only our exclusive temporary inode.
            unsafe {
                libc::unlinkat(self.fd(), temporary.as_ptr(), 0);
            }
        }
        result
    }
    pub fn lock(&self) -> Result<Lock> {
        let name = c_name(OsStr::new("coordination.lock"))?;
        // SAFETY: fixed relative name, nofollow. Never truncate a live lock.
        let file = cvt(unsafe {
            libc::openat(
                self.fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        trusted(&file, self.owner, false)?;
        // SAFETY: owned descriptor and nonblocking flock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::new(
                ErrorCode::Busy,
                "another controller owns installation coordination",
            ));
        }
        Ok(Lock { _file: file })
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        let name = c_name(OsStr::new(name))?;
        // SAFETY: never recursive, only a single relative leaf.
        if unsafe { libc::unlinkat(self.fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.dir.sync_all()?;
        Ok(())
    }
}
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

pub fn read_json<T: DeserializeOwned>(path: &Path, owner: u32) -> Result<T> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "missing record parent"))?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "invalid record name"))?;
    open_dir(parent, owner)?.json(name)
}
/// Read a log without loading the whole file or accepting special files.
pub fn log_tail(path: &Path, owner: u32, count: usize) -> Result<(Vec<String>, bool)> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_dir(
        path.parent()
            .ok_or_else(|| Error::new(ErrorCode::Io, "log parent"))?,
        owner,
    )?
    .open(
        path.file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| Error::new(ErrorCode::Io, "log name"))?,
    )?;
    let len = file.metadata()?.len();
    let offset = len.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(256 * 1024).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<_> = text.lines().skip(usize::from(offset > 0)).collect();
    let truncated = offset > 0 || all.len() > count;
    Ok((
        all.iter()
            .skip(all.len().saturating_sub(count))
            .map(|s| (*s).to_string())
            .collect(),
        truncated,
    ))
}
/// Used only to create a new user-owned runtime state root, never to adopt a protected legacy prefix.
pub fn create_user_state(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new().recursive(true).create(path)?;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path.join(".initialized"))
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                File::open(path.join(".initialized"))
            } else {
                Err(e)
            }
        })?;
    Ok(())
}
